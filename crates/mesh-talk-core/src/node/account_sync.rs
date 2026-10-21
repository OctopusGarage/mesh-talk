//! Bounded direct and channel history catch-up between certified account devices.

use super::account_preferences::AccountPreference;
use super::account_sync_policy::{DeviceSyncPolicy, SyncCategory};
use super::account_sync_wire::{
    AccountHistoryFrame, AccountSyncVersion, HistoryCursor, MAX_CHUNK, MAX_RECORD,
};
use super::conversation::account_conversation_id;
use super::dm_envelope::DmEnvelope;
use super::node::NodeError;
use super::pairing::BackfillRecord;
use super::session::SessionError;
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::identity::device::PublicIdentity;
use crate::transport::SecureChannel;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

#[cfg(test)]
use super::account_sync_wire::VERSION;

mod archive;
mod wake;
pub(in crate::node) use wake::AccountWake;
mod page;
pub(in crate::node) use archive::ArchiveCache;
#[cfg(test)]
mod selection_tests;
use page::{ChannelRecord, FileCardRecord, HistoryPageAssembler, HistoryRecord};
#[cfg(test)]
use page::{PageBuilder, PendingRecord};

const RECORDS_PER_PAGE: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPhase {
    Offline,
    CatchingUp,
    UpToDate,
    Error,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceSyncStatus {
    pub device_id: String,
    pub phase: SyncPhase,
    pub pending_attachments: usize,
    pub legacy_file_metadata: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct PeerCursor {
    next: Option<HistoryCursor>,
    known_digest: [u8; 32],
    sweep_digest: [u8; 32],
    next_file: usize,
}

impl PeerCursor {
    /// Continue forward through a changing archive. A changed sweep gets one
    /// fresh sweep afterward to collect new IDs behind the exclusive cursor.
    fn complete_page(&mut self, next: Option<HistoryCursor>, digest: [u8; 32]) -> bool {
        match next {
            Some(next) => {
                if self.next.is_none() {
                    self.sweep_digest = digest;
                }
                self.next = Some(next);
                self.known_digest = [0; 32];
                false
            }
            None => {
                let complete = self.next.is_none() || self.sweep_digest == digest;
                self.next = None;
                self.known_digest = if complete { digest } else { [0; 32] };
                complete
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct FileCardPayload {
    manifest: Vec<u8>,
    proof: Event,
}

#[derive(Serialize)]
struct FileCardPayloadRef<'a> {
    manifest: &'a [u8],
    proof: &'a Event,
}

fn transport(error: crate::transport::TransportError) -> NodeError {
    NodeError::Session(SessionError::Transport(error))
}

async fn send_frame(
    channel: &mut SecureChannel<TcpStream>,
    version: AccountSyncVersion,
    frame: &AccountHistoryFrame,
) -> Result<(), NodeError> {
    let bytes = frame
        .encode_version(version)
        .ok_or_else(|| NodeError::Channel("invalid account-history frame".into()))?;
    channel.send(&bytes).await.map_err(transport)
}

impl Node {
    /// Keep disk/scan work out of the shared accept/delivery task. Retirement
    /// holds its permit inside the writer even if the awaiting future is cancelled.
    pub(in crate::node) async fn account_blocking<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> Result<T, NodeError> + Send + 'static,
    ) -> Result<T, NodeError> {
        let permit = self
            .runtime_work
            .admit()
            .ok_or_else(|| NodeError::Channel("participant runtime is stopping".into()))?;
        let node = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work(node)
        })
        .await
        .map_err(|error| NodeError::Channel(format!("account sync worker: {error}")))?
    }
    async fn send_account_frame(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        revision: u64,
        frame: &AccountHistoryFrame,
    ) -> Result<(), NodeError> {
        if self.device_sync_policy_snapshot().1 != revision {
            return Err(NodeError::Authorization(
                "account sync policy changed".into(),
            ));
        }
        send_frame(channel, version, frame).await
    }

    /// Process-local revision of durable history projections for host polling.
    pub fn account_history_revision(&self) -> u64 {
        self.account_history_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(in crate::node) fn note_account_history_change(&self) {
        self.account_history_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.note_ui_change();
    }

    pub fn device_sync_status(&self) -> Vec<DeviceSyncStatus> {
        let policy = self.device_sync_policy();
        let current = self
            .account_sync_status
            .lock()
            .expect("account sync status mutex not poisoned");
        let mut statuses = self
            .linked_devices()
            .into_iter()
            .filter(|device| device.public.user_id() != self.user_id())
            .map(|device| {
                let id = device.public.user_id();
                let mut status = current.get(&id).cloned().unwrap_or(DeviceSyncStatus {
                    device_id: id,
                    phase: SyncPhase::Offline,
                    pending_attachments: 0,
                    legacy_file_metadata: false,
                    error: None,
                });
                if !policy.history && !policy.attachments && !policy.appearance && !policy.contacts
                {
                    status.phase = SyncPhase::Disabled;
                    status.error = None;
                }
                status
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        statuses
    }
    pub fn account_preferences(&self, category: SyncCategory) -> Vec<AccountPreference> {
        self.account_preferences
            .lock()
            .expect("account preferences mutex not poisoned")
            .entries(category)
    }

    pub fn set_account_preference(
        &self,
        category: SyncCategory,
        key: String,
        value: Option<String>,
    ) -> Result<AccountPreference, NodeError> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |duration| {
                duration.as_millis().min(u64::MAX as u128) as u64
            });
        let record = self
            .account_preferences
            .lock()
            .expect("account preferences mutex not poisoned")
            .set_local(category, key, value, self.user_id(), now_ms)
            .map_err(NodeError::Log)?;
        if self.device_sync_policy().enabled(category) {
            self.account_sync_notify.notify_one();
        }
        self.note_ui_change();
        self.note_account_local_change();
        Ok(record)
    }

    pub fn device_sync_policy(&self) -> DeviceSyncPolicy {
        self.device_sync_policy_snapshot().0
    }

    pub(in crate::node) fn certified_sibling(&self, public: &PublicIdentity) -> bool {
        self.linked_devices().iter().any(|device| {
            device.public == *public
                && device.certificate.verify()
                && device.certificate.account_id() == self.account_id()
                && device.certificate.device_ed25519_pub == public.ed25519_pub
        })
    }

    pub(in crate::node) fn device_sync_policy_snapshot(&self) -> (DeviceSyncPolicy, u64) {
        let store = self
            .account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned");
        (store.current(), store.revision())
    }

    pub fn set_device_sync_category(
        &self,
        category: SyncCategory,
        enabled: bool,
    ) -> Result<(), NodeError> {
        {
            let _commit = self
                .account_sync_commit
                .lock()
                .expect("account sync commit mutex not poisoned");
            self.account_sync_policy
                .lock()
                .expect("account sync policy mutex not poisoned")
                .set(category, enabled)
                .map_err(NodeError::Log)?;
        }
        self.account_sync_notify.notify_one();
        self.note_ui_change();
        self.note_account_local_change();
        if category == SyncCategory::History && enabled {
            let conversations = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .conversations();
            for conversation in conversations {
                self.emit_new_messages(conversation);
            }
            self.delivery_notify.notify_one();
        }
        if category == SyncCategory::Profile && enabled {
            let conversations = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .conversations();
            for conversation in conversations {
                self.process_profile_events(conversation);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn export_sync_records(
        &self,
        requester: &PublicIdentity,
        after: Option<HistoryCursor>,
        include_history: bool,
        include_files: bool,
        version: AccountSyncVersion,
    ) -> (Vec<HistoryRecord>, Option<HistoryCursor>, [u8; 32]) {
        self.prepare_account_export(requester, after, include_history, include_files, version)
            .unwrap()
    }

    pub(in crate::node) async fn serve_account_history(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        wanted: DeviceSyncPolicy,
    ) -> bool {
        tokio::time::timeout(
            Duration::from_secs(20),
            self.serve_account_history_inner(
                channel,
                version,
                account_id,
                cursor,
                known_digest,
                wanted,
            ),
        )
        .await
        .unwrap_or(false)
    }

    async fn serve_account_history_inner(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        wanted: DeviceSyncPolicy,
    ) -> bool {
        if account_id != self.account_id() || !channel.io_admitted() {
            return false;
        }
        let peer = channel.peer_identity().clone();
        if !self.linked_devices().iter().any(|device| {
            device.public == peer
                && device.certificate.verify()
                && device.certificate.account_id() == account_id
                && device.certificate.device_ed25519_pub == peer.ed25519_pub
        }) {
            return false;
        }
        let (offered, revision) = self.device_sync_policy_snapshot();
        for device in self.linked_devices() {
            if self
                .send_account_frame(
                    channel,
                    version,
                    revision,
                    &AccountHistoryFrame::Device { device },
                )
                .await
                .is_err()
            {
                return false;
            }
        }
        let mut preferences = Vec::new();
        {
            let store = self
                .account_preferences
                .lock()
                .expect("account preferences mutex not poisoned");
            for category in [SyncCategory::Appearance, SyncCategory::Contacts] {
                if offered.enabled(category) && wanted.enabled(category) {
                    preferences.extend(store.entries(category));
                }
            }
        }
        preferences.sort_by(|a, b| {
            (matches!(a.category, SyncCategory::Contacts), a.key.as_str())
                .cmp(&(matches!(b.category, SyncCategory::Contacts), b.key.as_str()))
        });
        let (records, next_cursor, history_digest) =
            if (offered.history && wanted.history) || (offered.attachments && wanted.attachments) {
                let result = self
                    .account_blocking(move |node| {
                        node.prepare_account_export(
                            &peer,
                            cursor,
                            offered.history && wanted.history,
                            version.includes_file_cards(
                                offered.history && wanted.history,
                                offered.attachments && wanted.attachments,
                            ),
                            version,
                        )
                    })
                    .await;
                let Ok(page) = result else {
                    return false;
                };
                page
            } else {
                (Vec::new(), None, archive::empty_history_digest())
            };
        let mut digest_hasher = Sha256::new();
        digest_hasher.update(b"mesh-talk-account-sync-v2");
        digest_hasher.update(history_digest);
        for record in &preferences {
            digest_hasher.update([matches!(record.category, SyncCategory::Contacts) as u8]);
            digest_hasher.update((record.key.len() as u32).to_be_bytes());
            digest_hasher.update(record.key.as_bytes());
            digest_hasher.update(record.revision.to_be_bytes());
            digest_hasher.update(record.device_id.as_bytes());
            if let Some(value) = &record.value {
                digest_hasher.update([1]);
                digest_hasher.update((value.len() as u32).to_be_bytes());
                digest_hasher.update(value.as_bytes());
            } else {
                digest_hasher.update([0]);
            }
        }
        let digest: [u8; 32] = digest_hasher.finalize().into();
        if cursor.is_none() && known_digest == digest {
            return self
                .send_account_frame(
                    channel,
                    version,
                    revision,
                    &AccountHistoryFrame::Complete {
                        next_cursor: None,
                        digest,
                    },
                )
                .await
                .is_ok();
        }
        if cursor.is_none() {
            for record in preferences {
                if self
                    .send_account_frame(
                        channel,
                        version,
                        revision,
                        &AccountHistoryFrame::Preference { record },
                    )
                    .await
                    .is_err()
                {
                    return false;
                }
            }
        }
        for record in &records {
            let (header, plaintext) = match record {
                HistoryRecord::Direct(record) => (
                    AccountHistoryFrame::RecordHeader {
                        conversation: record.conv,
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.plaintext.len() as u32,
                    },
                    &record.plaintext,
                ),
                HistoryRecord::Channel(record) => (
                    AccountHistoryFrame::ChannelHeader {
                        channel: record.conversation,
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.plaintext.len() as u32,
                    },
                    &record.plaintext,
                ),
                HistoryRecord::FileCard(record) => (
                    AccountHistoryFrame::FileCardHeader {
                        host: record.host,
                        peer_account: record.peer_account.clone(),
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.payload.len() as u32,
                    },
                    &record.payload,
                ),
            };
            if self
                .send_account_frame(channel, version, revision, &header)
                .await
                .is_err()
            {
                return false;
            }
            for chunk in plaintext.chunks(MAX_CHUNK) {
                if self
                    .send_account_frame(
                        channel,
                        version,
                        revision,
                        &AccountHistoryFrame::RecordChunk {
                            bytes: chunk.to_vec(),
                        },
                    )
                    .await
                    .is_err()
                {
                    return false;
                }
            }
        }
        self.send_account_frame(
            channel,
            version,
            revision,
            &AccountHistoryFrame::Complete {
                next_cursor,
                digest,
            },
        )
        .await
        .is_ok()
    }

    fn import_account_file_card(
        &self,
        record: &FileCardRecord,
        version: AccountSyncVersion,
    ) -> Result<(), NodeError> {
        let host = ConversationId::new(record.host);
        let event_id = EventId::new(record.event_id);
        if let Some(other) = &record.peer_account {
            if (other == &self.account_id() && version != AccountSyncVersion::V2)
                || account_conversation_id(&self.account_id(), other) != host
            {
                return Err(NodeError::Authorization(
                    "invalid account file conversation".into(),
                ));
            }
        } else {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            if !book.state(&host).is_some_and(|state| {
                !state.pending_invite() && state.is_accepted_member(&self.user_id())
            }) {
                return Err(NodeError::Authorization(
                    "file card requires accepted channel membership".into(),
                ));
            }
        }
        let payload: FileCardPayload = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_RECORD as u64)
            .reject_trailing_bytes()
            .deserialize(&record.payload)
            .map_err(|_| NodeError::Channel("invalid account file card".into()))?;
        let row = super::received_log::ReceivedEntry {
            conversation: host,
            event_id,
            from: record.from.clone(),
            wall_clock: record.wall_clock,
            plaintext: payload.manifest,
        };
        let personal = record.peer_account.as_deref() == Some(self.account_id().as_str());
        if personal
            && (!super::files::personal_file_proof_matches(&row, &payload.proof)
                || !self.linked_devices().iter().any(|device| {
                    device.public.ed25519_pub == *payload.proof.author.ed25519_pub()
                        && device.certificate.verify()
                        && device.certificate.account_id() == self.account_id()
                        && device.certificate.device_ed25519_pub == device.public.ed25519_pub
                }))
        {
            return Err(NodeError::Authorization(
                "invalid certified personal file proof".into(),
            ));
        }
        if !personal && super::files::is_personal_file_proof(&payload.proof) {
            return Err(NodeError::Authorization(
                "personal file proof has another host".into(),
            ));
        }
        if self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock) {
            return Ok(());
        }
        if payload.proof.id != event_id
            || payload.proof.kind != crate::eventlog::EventKind::FileManifest
            || !payload.proof.verify_integrity()
            || !payload.proof.verify_signature()
            || payload.proof.author.user_id() != row.from
            || payload.proof.wall_clock != row.wall_clock
            || super::files::eligible_manifest_row(&row, Some(&payload.proof), None).is_none()
        {
            return Err(NodeError::Authorization("invalid signed file card".into()));
        }
        if let Some(existing) = self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .get(&event_id)
            && existing != &payload.proof
        {
            return Err(NodeError::Authorization(
                "file proof conflicts with signed event".into(),
            ));
        }
        let suppressed = self
            .account_suppression
            .lock()
            .expect("account suppression mutex not poisoned")
            .contains(host, event_id);
        let erased = {
            let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
            delivery.file_erased(host, event_id) || delivery.manifest_event_erased(event_id)
        };
        if suppressed || erased {
            return Ok(());
        }
        self.account_file_proofs
            .lock()
            .expect("file proofs mutex not poisoned")
            .insert(payload.proof)
            .map_err(NodeError::Log)?;
        let mut files = self
            .received_files
            .lock()
            .expect("files mutex not poisoned");
        let fresh = !files
            .entries_ref(&host)
            .iter()
            .any(|entry| entry.event_id == event_id);
        files
            .record_file_alias_durable(&row)
            .map_err(NodeError::Log)?;
        drop(files);
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        self.reseed_live_file_book(&delivery);
        if fresh {
            self.note_account_history_change();
        }
        Ok(())
    }

    fn has_live_local_file_card(&self, file_conv: ConversationId) -> bool {
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        self.has_live_local_file_card_with(file_conv, &delivery)
    }

    fn has_live_local_file_card_with(
        &self,
        file_conv: ConversationId,
        delivery: &super::delivery_store::DeliveryStore,
    ) -> bool {
        let rows = self
            .received_files
            .lock()
            .expect("files mutex not poisoned");
        let suppression = self
            .account_suppression
            .lock()
            .expect("suppression mutex not poisoned");
        for host in rows.conversations() {
            for row in rows.entries_ref(&host) {
                if delivery.file_erased(host, row.event_id)
                    || delivery.manifest_event_erased(row.event_id)
                    || suppression.contains(host, row.event_id)
                {
                    continue;
                }
                if super::files::validated_manifest(&row.plaintext)
                    .is_some_and(|manifest| manifest.file_conv() == file_conv)
                {
                    return true;
                }
            }
        }
        false
    }

    fn attachment_for_sibling(
        &self,
        file_conv: ConversationId,
        requester: &PublicIdentity,
    ) -> Option<crate::file::AnyManifest> {
        let own = self.account_id();
        let me = self.user_id();
        let requester_id = requester.user_id();
        let mut hosts = HashSet::new();
        hosts.insert(account_conversation_id(&own, &own));
        for other in self.known_account_ids() {
            hosts.insert(account_conversation_id(&own, &other));
        }
        {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            for channel in book.channel_ids() {
                if book.state(&channel).is_some_and(|state| {
                    !state.is_dissolved()
                        && state.is_accepted_member(&me)
                        && state.is_accepted_member(&requester_id)
                }) {
                    hosts.insert(channel);
                }
            }
        }
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        let rows = self
            .received_files
            .lock()
            .expect("files mutex not poisoned");
        let log = self.log.lock().expect("log mutex not poisoned");
        let proofs = self
            .account_file_proofs
            .lock()
            .expect("file proofs mutex not poisoned");
        let suppression = self
            .account_suppression
            .lock()
            .expect("suppression mutex not poisoned");
        for host in hosts {
            for row in rows.entries_ref(&host) {
                if delivery.file_erased(host, row.event_id)
                    || delivery.manifest_event_erased(row.event_id)
                    || suppression.contains(host, row.event_id)
                {
                    continue;
                }
                let proof = log.get(&row.event_id).or_else(|| proofs.get(&row.event_id));
                if let Some(manifest) = super::files::eligible_manifest_row(row, proof, None)
                    && manifest.file_conv() == file_conv
                {
                    return Some(manifest);
                }
            }
        }
        None
    }

    pub(in crate::node) async fn serve_account_attachment(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        file_conv: ConversationId,
    ) -> bool {
        let size = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .map_or(0, |manifest| manifest.size());
        let deadline =
            Duration::from_secs(30).saturating_add(Duration::from_secs(size.div_ceil(16 * 1024)));
        tokio::time::timeout(
            deadline,
            self.serve_account_attachment_inner(channel, version, account_id, file_conv),
        )
        .await
        .unwrap_or(false)
    }

    async fn serve_account_attachment_inner(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        file_conv: ConversationId,
    ) -> bool {
        let (offered, revision) = self.device_sync_policy_snapshot();
        if account_id != self.account_id() || !channel.io_admitted() || !offered.attachments {
            return false;
        }
        let peer = channel.peer_identity().clone();
        if !self.linked_devices().iter().any(|device| {
            device.public == peer
                && device.certificate.verify()
                && device.certificate.account_id() == account_id
                && device.certificate.device_ed25519_pub == peer.ed25519_pub
        }) {
            return false;
        }
        let staging = self
            .account_blocking(move |node| {
                let manifest = node
                    .attachment_for_sibling(file_conv, &peer)
                    .ok_or_else(|| {
                        NodeError::Authorization("attachment no longer shared".into())
                    })?;
                let staging =
                    tempfile::tempdir().map_err(|error| NodeError::File(error.to_string()))?;
                node.save_directory_archive(
                    file_conv,
                    &staging.path().join("verified-attachment"),
                )?;
                Ok((staging, manifest))
            })
            .await;
        let Ok((staging, manifest)) = staging else {
            return false;
        };
        let path = staging.path().join("verified-attachment");
        let Ok(mut source) = tokio::fs::File::open(&path).await else {
            return false;
        };
        if self
            .send_account_frame(
                channel,
                version,
                revision,
                &AccountHistoryFrame::AttachmentStart {
                    file_conv: *file_conv.as_bytes(),
                    name: manifest.name().to_string(),
                    size: manifest.size(),
                    sha256: manifest.checksum(),
                },
            )
            .await
            .is_err()
        {
            return false;
        }
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = vec![0u8; MAX_CHUNK];
        loop {
            let Ok(read) = source.read(&mut buffer).await else {
                return false;
            };
            if read == 0 {
                break;
            }
            total += read as u64;
            hasher.update(&buffer[..read]);
            if self
                .send_account_frame(
                    channel,
                    version,
                    revision,
                    &AccountHistoryFrame::AttachmentChunk {
                        bytes: buffer[..read].to_vec(),
                    },
                )
                .await
                .is_err()
            {
                return false;
            }
        }
        let actual: [u8; 32] = hasher.finalize().into();
        if total == manifest.size() && actual == manifest.checksum() {
            self.send_account_frame(
                channel,
                version,
                revision,
                &AccountHistoryFrame::AttachmentComplete,
            )
            .await
            .is_ok()
        } else {
            false
        }
    }

    async fn pull_account_attachment(
        self: &Arc<Self>,
        peer: &PeerRecord,
        file_conv: ConversationId,
        version: AccountSyncVersion,
    ) -> Result<(), NodeError> {
        let (wanted, revision) = self.device_sync_policy_snapshot();
        if !wanted.attachments {
            return Ok(());
        }
        let own = self.account_id();
        if peer.account_id.as_deref() != Some(own.as_str())
            || !self.linked_devices().iter().any(|device| {
                device.public == peer.public
                    && device.certificate.verify()
                    && device.certificate.account_id() == own
                    && device.certificate.device_ed25519_pub == peer.public.ed25519_pub
            })
        {
            return Err(NodeError::Authorization(
                "peer is not a certified linked device".into(),
            ));
        }
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::File("unknown attachment".into()))?;
        let mut channel = self
            .begin_account_exchange(
                peer,
                version,
                AccountHistoryFrame::AttachmentRequest {
                    version: version.number(),
                    account_id: own,
                    file_conv: *file_conv.as_bytes(),
                },
            )
            .await?;
        let mut staging: Option<tempfile::TempDir> = None;
        let mut output: Option<tokio::fs::File> = None;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::File("attachment read timed out".into()))?
                .map_err(transport)?;
            let (response_version, frame) = AccountHistoryFrame::decode_versioned(&bytes)
                .ok_or_else(|| NodeError::File("invalid attachment frame".into()))?;
            if response_version != version {
                return Err(NodeError::File("attachment codec changed".into()));
            }
            match frame {
                AccountHistoryFrame::AttachmentStart {
                    file_conv: claimed,
                    name,
                    size,
                    sha256,
                } if staging.is_none()
                    && claimed == *file_conv.as_bytes()
                    && name == manifest.name()
                    && size == manifest.size()
                    && sha256 == manifest.checksum() =>
                {
                    let dir = tempfile::tempdir()
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                    let file = tokio::fs::File::create(dir.path().join("incoming"))
                        .await
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                    output = Some(file);
                    staging = Some(dir);
                }
                AccountHistoryFrame::AttachmentChunk { bytes } if output.is_some() => {
                    if bytes.is_empty() || bytes.len() > MAX_CHUNK {
                        return Err(NodeError::File("invalid attachment chunk".into()));
                    }
                    received = received.saturating_add(bytes.len() as u64);
                    if received > manifest.size() {
                        return Err(NodeError::File("attachment exceeds manifest size".into()));
                    }
                    hasher.update(&bytes);
                    use tokio::io::AsyncWriteExt;
                    output
                        .as_mut()
                        .unwrap()
                        .write_all(&bytes)
                        .await
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                }
                AccountHistoryFrame::AttachmentComplete if output.is_some() => {
                    let actual: [u8; 32] = hasher.finalize().into();
                    if received != manifest.size() || actual != manifest.checksum() {
                        return Err(NodeError::File("attachment checksum mismatch".into()));
                    }
                    output
                        .take()
                        .unwrap()
                        .sync_all()
                        .await
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                    let dir = staging.take().unwrap();
                    self.account_blocking(move |node| {
                        node.commit_account_attachment(file_conv, revision, &manifest, dir)
                    })
                    .await?;
                    channel.recycle(self);
                    return Ok(());
                }
                _ => return Err(NodeError::File("unexpected attachment frame".into())),
            }
        }
    }

    fn commit_account_attachment(
        &self,
        file_conv: ConversationId,
        revision: u64,
        manifest: &crate::file::AnyManifest,
        dir: tempfile::TempDir,
    ) -> Result<(), NodeError> {
        let _commit = self
            .account_sync_commit
            .lock()
            .expect("account sync commit mutex not poisoned");
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        let policy = self
            .account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned");
        if policy.revision() != revision
            || !policy.current().attachments
            || self
                .files
                .lock()
                .expect("files mutex not poisoned")
                .manifest(&file_conv)
                .is_none()
            || !self.has_live_local_file_card_with(file_conv, &delivery)
        {
            return Err(NodeError::File("attachment was removed during sync".into()));
        }
        // MediaStore encrypts verified plaintext into this account's private store.
        self.media
            .store_from_path(file_conv, manifest.name(), &dir.path().join("incoming"))
            .map_err(|e| NodeError::File(format!("attachment save: {e}")))?;
        self.note_account_history_change();
        Ok(())
    }

    fn preserve_available_attachment(
        &self,
        file_conv: ConversationId,
        revision: u64,
    ) -> Result<bool, NodeError> {
        if self.media.contains(file_conv) {
            return Ok(true);
        }
        if !self.has_live_local_file_card(file_conv)
            || self
                .file_progress(file_conv)
                .is_none_or(|progress| progress.done != progress.total)
        {
            return Ok(false);
        }
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::File("unknown attachment".into()))?;
        let staging =
            tempfile::tempdir().map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
        let path = staging.path().join("verified-attachment");
        self.save_directory_archive(file_conv, &path)?;
        // The same final current-policy/card guard covers local retention and remote pulls.
        // commit_account_attachment expects the verified file at this staging path.
        std::fs::rename(&path, staging.path().join("incoming"))
            .map_err(|error| NodeError::File(error.to_string()))?;
        self.commit_account_attachment(file_conv, revision, &manifest, staging)?;
        Ok(true)
    }

    #[cfg(test)]
    fn apply_account_history_page(
        &self,
        page: &[HistoryRecord],
        version: AccountSyncVersion,
    ) -> Result<(), NodeError> {
        self.apply_account_history_page_at_revision(page, version, None)
    }

    fn apply_account_history_page_at_revision(
        &self,
        page: &[HistoryRecord],
        version: AccountSyncVersion,
        expected_revision: Option<u64>,
    ) -> Result<(), NodeError> {
        let _commit = self
            .account_sync_commit
            .lock()
            .expect("account sync commit mutex not poisoned");
        let (policy, revision) = self.device_sync_policy_snapshot();
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(NodeError::Authorization(
                "account sync policy changed".into(),
            ));
        }
        if policy.history {
            {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                for record in page {
                    if let HistoryRecord::Channel(record) = record {
                        let channel = ConversationId::new(record.conversation);
                        if !book.state(&channel).is_some_and(|state| {
                            !state.pending_invite() && state.is_accepted_member(&self.user_id())
                        }) {
                            return Err(NodeError::Authorization(
                                "channel history requires accepted membership".into(),
                            ));
                        }
                    }
                }
            }
            {
                let log = self.log.lock().expect("log mutex not poisoned");
                for record in page {
                    if let HistoryRecord::Channel(record) = record {
                        let channel = ConversationId::new(record.conversation);
                        if let Some(event) = log.get(&EventId::new(record.event_id))
                            && (event.conversation_id != channel
                                || event.kind != crate::eventlog::EventKind::Message
                                || event.author.user_id() != record.from)
                        {
                            return Err(NodeError::Authorization(
                                "channel history conflicts with signed event".into(),
                            ));
                        }
                    }
                }
            }
            let direct: Vec<_> = page
                .iter()
                .filter_map(|record| match record {
                    HistoryRecord::Direct(record) => Some(record.clone()),
                    HistoryRecord::Channel(_) | HistoryRecord::FileCard(_) => None,
                })
                .collect();
            self.import_account_backfill(&direct)?;
            for record in page {
                if let HistoryRecord::Channel(record) = record {
                    let conversation = ConversationId::new(record.conversation);
                    let event_id = EventId::new(record.event_id);
                    if self.recipient_expired(&record.plaintext, event_id, record.wall_clock) {
                        continue;
                    }
                    if self
                        .account_suppression
                        .lock()
                        .expect("account suppression mutex not poisoned")
                        .contains(conversation, event_id)
                    {
                        continue;
                    }
                    let mut received = self.received.lock().expect("received mutex not poisoned");
                    if let Some(existing) = received.entry(event_id) {
                        // The live receive and sender sidecar can timestamp the
                        // same signed event differently. Preserve the local row
                        // when its identity and plaintext already match.
                        if existing.conversation == conversation
                            && existing.from == record.from
                            && existing.plaintext == record.plaintext
                        {
                            continue;
                        }
                        return Err(NodeError::Authorization(
                            "channel history conflicts with local plaintext".into(),
                        ));
                    }
                    received
                        .record_durable(&super::received_log::ReceivedEntry {
                            conversation,
                            event_id,
                            from: record.from.clone(),
                            wall_clock: record.wall_clock,
                            plaintext: record.plaintext.clone(),
                        })
                        .map_err(NodeError::Log)?;
                    self.note_account_history_change();
                }
            }
        }
        if version.includes_file_cards(policy.history, policy.attachments) {
            for record in page {
                if let HistoryRecord::FileCard(record) = record {
                    self.import_account_file_card(record, version)?;
                }
            }
        }
        Ok(())
    }

    async fn pull_account_history_page(
        self: &Arc<Self>,
        peer: &PeerRecord,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
    ) -> Result<(Option<HistoryCursor>, [u8; 32], AccountSyncVersion), NodeError> {
        let mut rejected_before_reply = false;
        let result = self
            .pull_account_history_version(
                peer,
                cursor,
                known_digest,
                AccountSyncVersion::V2,
                &mut rejected_before_reply,
            )
            .await;
        match result {
            Ok((next, digest)) => Ok((next, digest, AccountSyncVersion::V2)),
            Err(_) if rejected_before_reply => {
                let mut ignored = false;
                self.pull_account_history_version(
                    peer,
                    cursor,
                    known_digest,
                    AccountSyncVersion::V1,
                    &mut ignored,
                )
                .await
                .map(|(next, digest)| (next, digest, AccountSyncVersion::V1))
            }
            Err(error) => Err(error),
        }
    }

    async fn pull_account_history_version(
        self: &Arc<Self>,
        peer: &PeerRecord,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        version: AccountSyncVersion,
        rejected_before_reply: &mut bool,
    ) -> Result<(Option<HistoryCursor>, [u8; 32]), NodeError> {
        let (wanted, revision) = self.device_sync_policy_snapshot();
        if !wanted.history && !wanted.attachments && !wanted.appearance && !wanted.contacts {
            return Ok((None, [0; 32]));
        }
        let own = self.account_id();
        if peer.account_id.as_deref() != Some(own.as_str())
            || !self.linked_devices().iter().any(|device| {
                device.public == peer.public
                    && device.certificate.verify()
                    && device.certificate.account_id() == own
                    && device.certificate.device_ed25519_pub == peer.public.ed25519_pub
            })
        {
            return Err(NodeError::Authorization(
                "peer is not a certified linked device".into(),
            ));
        }
        let mut channel = self
            .begin_account_exchange(
                peer,
                version,
                AccountHistoryFrame::Request {
                    version: version.number(),
                    account_id: own.clone(),
                    cursor: if wanted.history || wanted.attachments {
                        cursor
                    } else {
                        None
                    },
                    known_digest,
                    wanted,
                },
            )
            .await?;
        let mut page = HistoryPageAssembler::new();
        let mut seen_devices = HashSet::new();
        let mut seen_preferences = HashSet::new();
        let mut started_records = false;
        let mut valid_reply = channel.reusable();
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::Channel("account-history read timed out".into()))?
                .map_err(|error| {
                    *rejected_before_reply = !valid_reply && matches!(&error,
                        crate::transport::TransportError::Io(io) if matches!(io.kind(),
                            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe));
                    transport(error)
                })?;
            let (response_version, frame) = AccountHistoryFrame::decode_versioned(&bytes)
                .ok_or_else(|| NodeError::Channel("invalid account-history response".into()))?;
            if response_version != version {
                return Err(NodeError::Channel("account-history codec changed".into()));
            }
            valid_reply = true;
            match frame {
                AccountHistoryFrame::Device { device }
                    if !started_records && page.is_idle() && seen_devices.len() < 64 =>
                {
                    if !seen_devices.insert(device.public.user_id()) {
                        return Err(NodeError::Channel("duplicate linked device".into()));
                    }
                    self.account_devices
                        .lock()
                        .expect("account devices mutex not poisoned")
                        .insert(device)
                        .map_err(NodeError::Log)?;
                }
                AccountHistoryFrame::Preference { record }
                    if page.is_idle()
                        && page.is_empty()
                        && seen_preferences.len() < super::account_preferences::MAX_KEYS =>
                {
                    started_records = true;
                    let key = (
                        matches!(record.category, SyncCategory::Contacts),
                        record.key.clone(),
                    );
                    if !seen_preferences.insert(key) {
                        return Err(NodeError::Channel("duplicate account preference".into()));
                    }
                    if !self
                        .linked_devices()
                        .iter()
                        .any(|device| device.public.user_id() == record.device_id)
                    {
                        return Err(NodeError::Authorization("unknown preference author".into()));
                    }
                    self.account_blocking(move |node| {
                        let _commit = node
                            .account_sync_commit
                            .lock()
                            .expect("account sync commit mutex not poisoned");
                        let policy = node
                            .account_sync_policy
                            .lock()
                            .expect("account sync policy mutex not poisoned");
                        if policy.revision() == revision
                            && policy.current().enabled(record.category)
                        {
                            let changed = node
                                .account_preferences
                                .lock()
                                .expect("account preferences mutex not poisoned")
                                .merge(record)
                                .map_err(NodeError::Log)?;
                            if changed {
                                node.note_ui_change();
                            }
                        }
                        Ok(())
                    })
                    .await?;
                }
                other => {
                    if matches!(
                        other,
                        AccountHistoryFrame::RecordHeader { .. }
                            | AccountHistoryFrame::ChannelHeader { .. }
                            | AccountHistoryFrame::FileCardHeader { .. }
                    ) {
                        started_records = true;
                    }
                    if let Some(complete) = page.accept(other, &own)? {
                        // A disabled category may have skipped frames above. Never
                        // acknowledge their digest or retain a continuation cursor,
                        // even if off/on changes restored the original bools (ABA).
                        if self.device_sync_policy_snapshot().1 != revision {
                            return Ok((None, [0; 32]));
                        }
                        let next = complete.next_cursor;
                        let digest = complete.digest;
                        self.account_blocking(move |node| {
                            // Recheck after waiting for the blocking pool.
                            if node.device_sync_policy_snapshot().1 != revision {
                                return Err(NodeError::Authorization(
                                    "account sync policy changed".into(),
                                ));
                            }
                            node.apply_account_history_page_at_revision(
                                &complete.records,
                                version,
                                Some(revision),
                            )
                        })
                        .await?;
                        channel.recycle(self);
                        return Ok((next, digest));
                    }
                }
            }
        }
    }

    pub(in crate::node) async fn run_account_history_loop(self: Arc<Self>) {
        use std::collections::VecDeque;
        const HISTORY_WORKERS: usize = 4;
        const ATTACHMENT_WORKERS: usize = 2;
        let mut tick = tokio::time::interval(RETRY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut cursors: HashMap<String, PeerCursor> = HashMap::new();
        let mut queue: VecDeque<(String, PeerRecord)> = VecDeque::new();
        let mut active = HashSet::new();
        let mut history = tokio::task::JoinSet::new();
        let mut history_owners: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut attachment_owners: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut attachments = tokio::task::JoinSet::new();
        let mut active_attachments = HashSet::new();
        let mut retry_attachment: HashMap<String, std::time::Instant> = HashMap::new();
        let mut revision = self.device_sync_policy_snapshot().1;
        loop {
            while active.len() < HISTORY_WORKERS {
                let Some((id, peer)) = queue.pop_front() else {
                    break;
                };
                if !active.insert(id.clone()) {
                    continue;
                }
                let state = cursors.remove(&id).unwrap_or_default();
                let node = self.clone();
                let Some(permit) = self.runtime_work.admit() else {
                    return;
                };
                let owner = id.clone();
                let task = history.spawn(permit.track(async move {
                    let result = tokio::time::timeout(
                        Duration::from_secs(20),
                        node.pull_account_history_page(&peer, state.next, state.known_digest),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err(NodeError::Channel(
                            "account history exchange timed out".into(),
                        ))
                    });
                    (id, peer, state, result)
                }));
                history_owners.insert(task.id(), owner);
            }
            let mut enqueue_all = false;
            tokio::select! {
                _ = tick.tick() => { enqueue_all = true; }
                _ = self.account_sync_notify.notified() => { enqueue_all = true; }
                result = history.join_next_with_id(), if !history.is_empty() => {
                    if let Some(Err(error)) = &result
                        && let Some(id) = history_owners.remove(&error.id()) {
                            active.remove(&id); cursors.remove(&id);
                            self.set_account_sync_status(id.clone(), DeviceSyncStatus { device_id: id,
                                phase: SyncPhase::Error, pending_attachments: 0, legacy_file_metadata: false,
                                error: Some("account history worker terminated".into()) });
                    }
                    if let Some(Ok((task, (id, peer, mut state, result)))) = result {
                        history_owners.remove(&task);
                        active.remove(&id);
                        let (policy, current_revision) = self.device_sync_policy_snapshot();
                        if current_revision != revision { enqueue_all = true; }
                        else {
                            let mut status = DeviceSyncStatus { device_id: id.clone(), phase: SyncPhase::CatchingUp,
                                pending_attachments: 0, legacy_file_metadata: false, error: None };
                            let mut continuation = false;
                            match result {
                                Ok((next, digest, version)) => {
                                    status.legacy_file_metadata = version == AccountSyncVersion::V1;
                                    if state.complete_page(next, digest) { status.phase = SyncPhase::UpToDate; }
                                    else { continuation = true; }
                                }
                                Err(error) => {
                                    status.phase = SyncPhase::Error;
                                    status.error = Some(error.to_string().chars().take(160).collect());
                                }
                            }
                            if policy.attachments {
                                let missing = self.account_blocking(|node| {
                                    let mut missing = node.files.lock().expect("files mutex not poisoned").file_convs();
                                    missing.sort_by_key(|id| *id.as_bytes());
                                    missing.retain(|file| !node.media.contains(*file));
                                    Ok(missing)
                                }).await.unwrap_or_default();
                                status.pending_attachments = missing.len();
                                if !missing.is_empty() && attachments.len() < ATTACHMENT_WORKERS
                                    && !active_attachments.contains(&id)
                                    && retry_attachment.get(&id).is_none_or(|at| *at <= std::time::Instant::now()) {
                                    let file = missing[state.next_file % missing.len()];
                                    state.next_file = state.next_file.wrapping_add(1);
                                    active_attachments.insert(id.clone());
                                    let node = self.clone(); let peer = peer.clone(); let id = id.clone();
                                    let version = if status.legacy_file_metadata { AccountSyncVersion::V1 } else { AccountSyncVersion::V2 };
                                    let transfer_size = self.files.lock().expect("files mutex not poisoned")
                                        .manifest(&file).map_or(0, |manifest| manifest.size());
                                    // Bound the complete exchange without rejecting large legitimate
                                    // archives solely because they need more than a fixed short timeout.
                                    let deadline = Duration::from_secs(30).saturating_add(
                                        Duration::from_secs(transfer_size.div_ceil(16 * 1024)));
                                    let Some(permit) = self.runtime_work.admit() else { return; };
                                    let owner = id.clone();
                                    let task = attachments.spawn(permit.track(async move {
                                        let result = tokio::time::timeout(deadline, async {
                                            if node.account_blocking(move |node| node.preserve_available_attachment(file, revision)).await? {
                                                Ok(())
                                            } else { node.pull_account_attachment(&peer, file, version).await }
                                        }).await.unwrap_or_else(|_| Err(NodeError::File("account attachment exchange timed out".into())));
                                        (id, result)
                                    }));
                                    attachment_owners.insert(task.id(), owner);
                                }
                                if status.pending_attachments > 0 && status.phase == SyncPhase::UpToDate {
                                    status.phase = SyncPhase::CatchingUp;
                                }
                            }
                            self.set_account_sync_status(id.clone(), status);
                            cursors.insert(id.clone(), state);
                            if continuation {
                                // One page per peer per turn: tail insertion gives waiting siblings
                                // their turn while bounded workers immediately drain the backlog.
                                queue.push_back((id, peer));
                                tokio::task::yield_now().await;
                            }
                        }
                    }
                }
                result = attachments.join_next_with_id(), if !attachments.is_empty() => {
                    if let Some(Err(error)) = &result
                        && let Some(id) = attachment_owners.remove(&error.id()) {
                            active_attachments.remove(&id);
                            retry_attachment.insert(id, std::time::Instant::now() + RETRY_INTERVAL);
                    }
                    if let Some(Ok((task, (id, result)))) = result {
                        attachment_owners.remove(&task);
                        active_attachments.remove(&id);
                        if let Err(error) = result {
                            retry_attachment.insert(id.clone(), std::time::Instant::now() + RETRY_INTERVAL);
                            let previous_status = self.account_sync_status.lock().expect("account sync status mutex not poisoned").get(&id).cloned();
                            if let Some(mut status) = previous_status {
                                status.phase = SyncPhase::Error;
                                status.error = Some(error.to_string().chars().take(160).collect());
                                self.set_account_sync_status(id, status);
                            }
                        } else {
                            retry_attachment.remove(&id);
                            enqueue_all = true;
                        }
                    }
                }
            }
            let (policy, current_revision) = self.device_sync_policy_snapshot();
            if current_revision != revision {
                // Cancellation drops every network future. Blocking writers retain retirement
                // admission and perform their own current-policy checks before commit.
                history.abort_all();
                attachments.abort_all();
                while history.join_next().await.is_some() {}
                while attachments.join_next().await.is_some() {}
                active.clear();
                active_attachments.clear();
                history_owners.clear();
                attachment_owners.clear();
                queue.clear();
                cursors.clear();
                retry_attachment.clear();
                revision = current_revision;
                enqueue_all = true;
            }
            if !policy.history && !policy.attachments && !policy.appearance && !policy.contacts {
                queue.clear();
                cursors.clear();
                continue;
            }
            if enqueue_all {
                for device in self.linked_devices() {
                    let id = device.public.user_id();
                    if id == self.user_id()
                        || active.contains(&id)
                        || queue.iter().any(|(queued, _)| queued == &id)
                    {
                        continue;
                    }
                    let Some(peer) = self.routing_peer(&id) else {
                        self.set_account_sync_status(
                            id.clone(),
                            DeviceSyncStatus {
                                device_id: id,
                                phase: SyncPhase::Offline,
                                pending_attachments: 0,
                                legacy_file_metadata: false,
                                error: None,
                            },
                        );
                        continue;
                    };
                    if peer.public != device.public {
                        self.set_account_sync_status(
                            id.clone(),
                            DeviceSyncStatus {
                                device_id: id,
                                phase: SyncPhase::Error,
                                pending_attachments: 0,
                                legacy_file_metadata: false,
                                error: Some("discovered device identity changed".into()),
                            },
                        );
                        continue;
                    }
                    queue.push_back((id, peer));
                }
            }
        }
    }

    fn set_account_sync_status(&self, id: String, status: DeviceSyncStatus) {
        let mut statuses = self
            .account_sync_status
            .lock()
            .expect("account sync status mutex not poisoned");
        if statuses.get(&id) != Some(&status) {
            statuses.insert(id, status);
            drop(statuses);
            self.note_ui_change();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;

    #[tokio::test]
    async fn category_toggle_during_sibling_page_restarts_catch_up_including_off_on_aba() {
        use crate::{discovery::Roster, identity::device::DeviceIdentity};
        use std::sync::Mutex;
        use tokio::{net::TcpListener, sync::mpsc};

        for aba in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let open = |name: &str| {
                let (dm, _) = mpsc::unbounded_channel();
                let (channel, _) = mpsc::unbounded_channel();
                let (file, _) = mpsc::unbounded_channel();
                Node::open_with_account(
                    DeviceIdentity::generate(),
                    Account::from_secret_bytes(account.secret_bytes()),
                    Arc::new(Mutex::new(Roster::default())),
                    dm,
                    channel,
                    file,
                    &directory.path().join(format!("{name}-events.log")),
                    &directory.path().join(format!("{name}-sent.log")),
                    "pw",
                )
                .unwrap()
            };
            let local = open("local");
            let remote = open("remote");
            for (owner, sibling) in [(&local, &remote), (&remote, &local)] {
                owner
                    .account_devices
                    .lock()
                    .unwrap()
                    .insert(super::super::account_devices::AccountDevice {
                        public: sibling.identity.public(),
                        certificate: account.certify(&sibling.identity.public().ed25519_pub),
                    })
                    .unwrap();
                owner
                    .set_device_sync_category(SyncCategory::Appearance, true)
                    .unwrap();
            }
            remote
                .set_account_preference(
                    SyncCategory::Appearance,
                    "theme".into(),
                    Some("dark".into()),
                )
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            local.roster.lock().unwrap().update(
                &crate::discovery::Announce::new_with_account(
                    &remote.identity,
                    &account,
                    "Remote",
                    address.port(),
                ),
                address.ip(),
                &local.user_id(),
            );
            let (request_seen, first_request) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let (second_seen, second_request) = tokio::sync::oneshot::channel();
            let server_remote = remote.clone();
            let server = tokio::spawn(async move {
                let mut first = Some((request_seen, released));
                let mut second_seen = Some(second_seen);
                loop {
                    let (mut channel, bytes) =
                        selection_tests::accept_legacy_account_request(&listener, &server_remote)
                            .await;
                    let Some(AccountHistoryFrame::Request {
                        account_id,
                        cursor,
                        known_digest,
                        wanted,
                        ..
                    }) = AccountHistoryFrame::decode(&bytes)
                    else {
                        panic!("expected authenticated account-history request");
                    };
                    if let Some((request_seen, released)) = first.take() {
                        assert!(wanted.appearance);
                        request_seen.send(()).unwrap();
                        released.await.unwrap();
                    } else {
                        assert_eq!(
                            known_digest, [0; 32],
                            "changed category policy must not reuse an in-flight page digest (ABA={aba})"
                        );
                        assert!(
                            cursor.is_none(),
                            "changed policy restarts at the first page"
                        );
                        if let Some(second_seen) = second_seen.take() {
                            let _ = second_seen.send(());
                        }
                    }
                    server_remote
                        .serve_account_history(
                            &mut channel,
                            AccountSyncVersion::V2,
                            &account_id,
                            cursor,
                            known_digest,
                            wanted,
                        )
                        .await;
                }
            });
            let worker = tokio::spawn(local.clone().run_account_history_loop());
            let outcome = tokio::time::timeout(Duration::from_secs(3), async {
                first_request.await.unwrap();
                local
                    .set_device_sync_category(SyncCategory::Appearance, false)
                    .unwrap();
                if aba {
                    local
                        .set_device_sync_category(SyncCategory::Appearance, true)
                        .unwrap();
                }
                release.send(()).unwrap();
                loop {
                    if local
                        .device_sync_status()
                        .iter()
                        .any(|status| status.phase == SyncPhase::UpToDate)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                if !aba {
                    assert!(
                        local
                            .account_preferences(SyncCategory::Appearance)
                            .is_empty(),
                        "disabled category must not import the in-flight preference"
                    );
                    local
                        .set_device_sync_category(SyncCategory::Appearance, true)
                        .unwrap();
                }
                loop {
                    if local
                        .account_preferences(SyncCategory::Appearance)
                        .iter()
                        .any(|record| record.value.as_deref() == Some("dark"))
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                // The ABA case can merge its first page; still require the
                // queued follow-up request to discard the stale page digest.
                second_request
                    .await
                    .expect("server observes an invalidated follow-up request");
            })
            .await;
            worker.abort();
            let _ = worker.await;
            server.abort();
            let error = server.await.unwrap_err();
            assert!(error.is_cancelled(), "{error}");
            outcome.expect("re-enabled appearance catches up after an interrupted category page");
        }
    }

    #[test]
    fn history_page_assembler_accepts_only_complete_bounded_records() {
        let own = Account::generate().account_id();
        let mut page = HistoryPageAssembler::new();
        let header = AccountHistoryFrame::ChannelHeader {
            channel: [1; 32],
            from: "ab".repeat(16),
            wall_clock: 1,
            event_id: [2; 32],
            total_len: 3,
        };
        assert!(page.accept(header, &own).unwrap().is_none());
        assert!(
            page.accept(
                AccountHistoryFrame::Complete {
                    next_cursor: None,
                    digest: [3; 32],
                },
                &own,
            )
            .is_err()
        );
        assert!(
            page.accept(AccountHistoryFrame::RecordChunk { bytes: vec![4; 4] }, &own,)
                .is_err()
        );
        assert!(
            page.accept(AccountHistoryFrame::RecordChunk { bytes: vec![4; 3] }, &own,)
                .unwrap()
                .is_none()
        );
        let complete = page
            .accept(
                AccountHistoryFrame::Complete {
                    next_cursor: None,
                    digest: [3; 32],
                },
                &own,
            )
            .unwrap()
            .unwrap();
        assert_eq!(complete.records.len(), 1);
        assert_eq!(complete.digest, [3; 32]);
        assert!(
            matches!(&complete.records[0], HistoryRecord::Channel(record) if record.plaintext == vec![4; 3])
        );
    }

    #[test]
    fn archive_page_uses_exclusive_cursor_and_bounded_payloads() {
        let build = |id: u8| {
            HistoryRecord::Channel(ChannelRecord {
                conversation: [1; 32],
                from: "ab".repeat(16),
                wall_clock: id as u64,
                event_id: [id; 32],
                plaintext: vec![id; MAX_RECORD],
            })
        };
        let mut first = PageBuilder::new(None);
        for id in (1..=40).rev() {
            first.offer(
                HistoryCursor {
                    tag: 1,
                    id: [id; 32],
                },
                || build(id),
            );
            assert!(first.records.len() <= RECORDS_PER_PAGE + 1);
        }
        let (rows, next, digest) = first.finish();
        assert_eq!(rows.len(), RECORDS_PER_PAGE);
        assert_eq!(rows[0].id(), [1; 32]);
        assert_eq!(next.unwrap().id, [16; 32]);

        let mut second = PageBuilder::new(next);
        for id in 1..=40 {
            second.offer(
                HistoryCursor {
                    tag: 1,
                    id: [id; 32],
                },
                || build(id),
            );
        }
        let (rows, next, second_digest) = second.finish();
        assert_eq!(rows.len(), RECORDS_PER_PAGE);
        assert_eq!(rows[0].id(), [17; 32]);
        assert_eq!(next.unwrap().id, [32; 32]);
        assert_eq!(
            digest, second_digest,
            "scan order and cursor must not change the digest"
        );
    }

    #[test]
    fn pending_history_record_rejects_cross_account_and_tampered_headers() {
        let own = Account::generate().account_id();
        let peer = Account::generate().account_id();
        let outsider = Account::generate().account_id();
        let id = [9; 32];
        let plaintext =
            DmEnvelope::new(peer.clone(), own.clone(), id, b"history".to_vec()).encode();
        let valid = PendingRecord {
            kind: 0,
            conversation: *account_conversation_id(&own, &peer).as_bytes(),
            peer_account: None,
            from: peer.clone(),
            wall_clock: 1,
            event_id: id,
            total_len: plaintext.len(),
            plaintext,
        };
        assert!(valid.clone().finish(&own).is_some());
        assert!(valid.clone().finish(&outsider).is_none());
        assert!(
            PendingRecord {
                from: outsider,
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                event_id: [8; 32],
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                conversation: [7; 32],
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                total_len: valid.total_len + 1,
                ..valid
            }
            .finish(&own)
            .is_none()
        );
    }

    #[tokio::test]
    async fn unrelated_account_cannot_request_sibling_history_or_retained_attachment_over_tcp() {
        use crate::{discovery::Roster, identity::device::DeviceIdentity};
        use std::sync::Mutex;
        use tokio::{net::TcpListener, sync::mpsc};

        let directory = tempfile::tempdir().unwrap();
        let open = |name: &str| {
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (file, _) = mpsc::unbounded_channel();
            Node::open(
                DeviceIdentity::generate(),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &directory.path().join(format!("{name}-events.log")),
                &directory.path().join(format!("{name}-sent.log")),
                "pw",
            )
            .unwrap()
        };
        let victim = open("victim");
        let attacker = open("attacker");
        assert_ne!(victim.account_id(), attacker.account_id());
        assert!(
            !victim
                .linked_devices()
                .iter()
                .any(|device| device.public == attacker.identity.public())
        );
        victim
            .set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
        victim
            .set_device_sync_category(SyncCategory::Appearance, true)
            .unwrap();
        victim
            .set_account_preference(
                SyncCategory::Appearance,
                "theme".into(),
                Some("private-theme".into()),
            )
            .unwrap();
        let group = victim
            .create_channel("victim archive", vec![])
            .await
            .unwrap();
        victim
            .send_channel_message(group, b"private archived text")
            .await
            .unwrap();
        let contents = b"private retained attachment bytes";
        let source = directory.path().join("private.bin");
        std::fs::write(&source, contents).unwrap();
        let attachment = victim
            .send_file_channel(group, &source, crate::file::FileKind::File)
            .await
            .unwrap();
        let verified = directory.path().join("retained.bin");
        victim
            .save_directory_archive(attachment, &verified)
            .unwrap();
        assert_eq!(
            std::fs::read(verified).unwrap(),
            contents,
            "the requested attachment actually exists and is readable locally"
        );
        assert!(!victim.channel_history(group, 10).is_empty());
        assert!(
            !victim
                .account_preferences(SyncCategory::Appearance)
                .is_empty()
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        struct Server(tokio::task::JoinHandle<()>);
        impl Drop for Server {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _server = Server(tokio::spawn(Arc::clone(&victim).run_accept_loop(listener)));
        let victim_account = victim.account_id();
        let requests = [
            AccountHistoryFrame::Request {
                version: VERSION,
                account_id: victim_account.clone(),
                cursor: None,
                known_digest: [0; 32],
                wanted: DeviceSyncPolicy::default(),
            },
            AccountHistoryFrame::Request {
                version: VERSION,
                account_id: victim_account.clone(),
                cursor: None,
                known_digest: [0; 32],
                wanted: DeviceSyncPolicy {
                    history: true,
                    attachments: true,
                    profile: true,
                    appearance: true,
                    contacts: true,
                },
            },
            AccountHistoryFrame::AttachmentRequest {
                version: VERSION,
                account_id: victim_account,
                file_conv: *attachment.as_bytes(),
            },
        ];
        let requests = requests.into_iter().flat_map(|request| {
            let mut session_request = request.clone();
            match &mut session_request {
                AccountHistoryFrame::Request { version, .. }
                | AccountHistoryFrame::AttachmentRequest { version, .. } => {
                    *version = AccountSyncVersion::V2.number();
                }
                _ => unreachable!("only history and attachment requests are in this table"),
            }
            let wrapped = AccountHistoryFrame::SessionRequest {
                request_id: [7; 16],
                request: super::super::account_sync_wire::AccountSessionRequest::from_frame(
                    &session_request,
                )
                .unwrap(),
            };
            [request, wrapped]
        });
        for request in requests {
            // Bypass sibling-selection client helpers: the unrelated account
            // completes Noise authentication and directly submits a valid frame.
            let mut channel = attacker
                .privacy_dial(address, &victim.identity.public())
                .await
                .unwrap();
            let version = if matches!(request, AccountHistoryFrame::SessionRequest { .. }) {
                AccountSyncVersion::V2
            } else {
                AccountSyncVersion::V1
            };
            channel
                .send(&request.encode_version(version).unwrap())
                .await
                .unwrap();
            let response = tokio::time::timeout(Duration::from_secs(3), channel.recv())
                .await
                .expect("server promptly closes an unauthorized sibling request");
            assert!(
                response.is_err(),
                "server must disclose no history, device certificates, preferences, or attachment frame to an unrelated account"
            );
        }
    }
}
