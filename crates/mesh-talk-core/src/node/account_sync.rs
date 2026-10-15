//! Bounded direct and channel history catch-up between certified account devices.

use super::account_preferences::AccountPreference;
use super::account_sync_policy::{DeviceSyncPolicy, SyncCategory};
use super::account_sync_wire::{
    AccountHistoryFrame, HistoryCursor, MAX_CHUNK, MAX_RECORD, VERSION,
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

mod page;
#[cfg(test)]
use page::PendingRecord;
use page::{ChannelRecord, FileCardRecord, HistoryPageAssembler, HistoryRecord, PageBuilder};

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

#[derive(Clone, Debug, Serialize)]
pub struct DeviceSyncStatus {
    pub device_id: String,
    pub phase: SyncPhase,
    pub pending_attachments: usize,
    pub error: Option<String>,
}

#[derive(Default)]
struct PeerCursor {
    next: Option<HistoryCursor>,
    known_digest: [u8; 32],
    sweep_digest: [u8; 32],
    next_file: usize,
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
    frame: &AccountHistoryFrame,
) -> Result<(), NodeError> {
    let bytes = frame
        .encode()
        .ok_or_else(|| NodeError::Channel("invalid account-history frame".into()))?;
    channel.send(&bytes).await.map_err(transport)
}

impl Node {
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
        Ok(record)
    }

    pub fn device_sync_policy(&self) -> DeviceSyncPolicy {
        self.account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned")
            .current()
    }

    pub fn set_device_sync_category(
        &self,
        category: SyncCategory,
        enabled: bool,
    ) -> Result<(), NodeError> {
        self.account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned")
            .set(category, enabled)
            .map_err(NodeError::Log)?;
        if matches!(
            category,
            SyncCategory::History
                | SyncCategory::Attachments
                | SyncCategory::Appearance
                | SyncCategory::Contacts
        ) && enabled
        {
            self.account_sync_notify.notify_one();
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

    fn export_sync_records(
        &self,
        requester: &PublicIdentity,
        after: Option<HistoryCursor>,
        include_history: bool,
        include_files: bool,
    ) -> (Vec<HistoryRecord>, Option<HistoryCursor>, [u8; 32]) {
        let own = self.account_id();
        let mut page = PageBuilder::new(after);
        let requester_id = requester.user_id();
        let me = self.user_id();
        let channels = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.channel_ids()
                .into_iter()
                .filter(|id| {
                    book.state(id).is_some_and(|state| {
                        !state.is_dissolved()
                            && state.is_accepted_member(&me)
                            && state.is_member(&requester_id)
                    })
                })
                .collect::<Vec<_>>()
        };
        if include_history {
            {
                let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
                for conversation in sent.conversations() {
                    for entry in sent.entries_ref(&conversation) {
                        if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                            continue;
                        }
                        let Some(env) = DmEnvelope::decode(&entry.plaintext) else {
                            continue;
                        };
                        if env.route.sender_account != own
                            || *account_conversation_id(&own, &env.route.recipient_account)
                                .as_bytes()
                                != *conversation.as_bytes()
                        {
                            continue;
                        }
                        page.offer(
                            HistoryCursor {
                                tag: 0,
                                id: env.msg_id,
                            },
                            || {
                                HistoryRecord::Direct(BackfillRecord {
                                    conv: *conversation.as_bytes(),
                                    from: own.clone(),
                                    wall_clock: entry.wall_clock,
                                    plaintext: entry.plaintext.clone(),
                                    event_id: env.msg_id,
                                })
                            },
                        );
                    }
                }
            }
            {
                let received = self.received.lock().expect("received mutex not poisoned");
                for conversation in received.conversations() {
                    for entry in received.entries_ref(&conversation) {
                        if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                            continue;
                        }
                        let Some(env) = DmEnvelope::decode(&entry.plaintext) else {
                            continue;
                        };
                        let other = if env.route.sender_account == own {
                            &env.route.recipient_account
                        } else if env.route.recipient_account == own {
                            &env.route.sender_account
                        } else {
                            continue;
                        };
                        if env.route.sender_account != entry.from
                            || *account_conversation_id(&own, other).as_bytes()
                                != *conversation.as_bytes()
                        {
                            continue;
                        }
                        page.offer(
                            HistoryCursor {
                                tag: 0,
                                id: env.msg_id,
                            },
                            || {
                                HistoryRecord::Direct(BackfillRecord {
                                    conv: *conversation.as_bytes(),
                                    from: entry.from.clone(),
                                    wall_clock: entry.wall_clock,
                                    plaintext: entry.plaintext.clone(),
                                    event_id: env.msg_id,
                                })
                            },
                        );
                    }
                }
            }
            for &channel in &channels {
                {
                    let log = self.log.lock().expect("log mutex not poisoned");
                    let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
                    let recovered = self
                        .recovered_sent_ids
                        .lock()
                        .expect("recovered sent ids mutex not poisoned");
                    let author =
                        crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
                    for entry in sent.entries_ref(&channel) {
                        if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                            continue;
                        }
                        let id = recovered
                            .as_ref()
                            .and_then(|saved| saved.id_for(entry))
                            .or_else(|| {
                                log.authored_event(channel, author, entry.seq)
                                    .filter(|event| {
                                        event.kind == crate::eventlog::EventKind::Message
                                    })
                                    .map(|event| event.id)
                            });
                        let Some(id) = id else { continue };
                        page.offer(
                            HistoryCursor {
                                tag: 1,
                                id: *id.as_bytes(),
                            },
                            || {
                                HistoryRecord::Channel(ChannelRecord {
                                    conversation: *channel.as_bytes(),
                                    from: me.clone(),
                                    wall_clock: entry.wall_clock,
                                    event_id: *id.as_bytes(),
                                    plaintext: entry.plaintext.clone(),
                                })
                            },
                        );
                    }
                }
                let received = self.received.lock().expect("received mutex not poisoned");
                for entry in received.entries_ref(&channel) {
                    if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                        continue;
                    }
                    page.offer(
                        HistoryCursor {
                            tag: 1,
                            id: *entry.event_id.as_bytes(),
                        },
                        || {
                            HistoryRecord::Channel(ChannelRecord {
                                conversation: *channel.as_bytes(),
                                from: entry.from.clone(),
                                wall_clock: entry.wall_clock,
                                event_id: *entry.event_id.as_bytes(),
                                plaintext: entry.plaintext.clone(),
                            })
                        },
                    );
                }
            }
        }
        if include_files {
            let mut allowed_hosts: HashMap<ConversationId, Option<String>> = HashMap::new();
            for &channel in &channels {
                allowed_hosts.insert(channel, None);
            }
            for other in self.known_account_ids() {
                allowed_hosts.insert(account_conversation_id(&own, &other), Some(other));
            }
            let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
            let rows = self
                .received_files
                .lock()
                .expect("received files mutex not poisoned");
            let log = self.log.lock().expect("log mutex not poisoned");
            let proofs = self
                .account_file_proofs
                .lock()
                .expect("file proofs mutex not poisoned");
            let suppression = self
                .account_suppression
                .lock()
                .expect("account suppression mutex not poisoned");
            for host in rows.conversations() {
                let Some(peer_account) = allowed_hosts.get(&host) else {
                    continue;
                };
                for entry in rows.entries_ref(&host) {
                    if entry.plaintext.is_empty()
                        || delivery.file_erased(host, entry.event_id)
                        || delivery.manifest_event_erased(entry.event_id)
                        || suppression.contains(host, entry.event_id)
                    {
                        continue;
                    }
                    let Some(proof) = log
                        .get(&entry.event_id)
                        .or_else(|| proofs.get(&entry.event_id))
                    else {
                        continue;
                    };
                    if super::files::eligible_manifest_row(entry, Some(proof), None).is_none() {
                        continue;
                    }
                    let payload_size = bincode::DefaultOptions::new()
                        .with_fixint_encoding()
                        .serialized_size(&FileCardPayloadRef {
                            manifest: &entry.plaintext,
                            proof,
                        })
                        .unwrap_or(u64::MAX);
                    if payload_size > MAX_RECORD as u64 {
                        continue;
                    }
                    page.offer(
                        HistoryCursor {
                            tag: 2,
                            id: *entry.event_id.as_bytes(),
                        },
                        || {
                            let payload = bincode::DefaultOptions::new()
                                .with_fixint_encoding()
                                .serialize(&FileCardPayloadRef {
                                    manifest: &entry.plaintext,
                                    proof,
                                })
                                .expect("validated file payload serializes");
                            HistoryRecord::FileCard(FileCardRecord {
                                host: *host.as_bytes(),
                                peer_account: peer_account.clone(),
                                from: entry.from.clone(),
                                wall_clock: entry.wall_clock,
                                event_id: *entry.event_id.as_bytes(),
                                payload,
                            })
                        },
                    );
                }
            }
        }
        page.finish()
    }

    pub(in crate::node) async fn serve_account_history(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        account_id: &str,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        wanted: DeviceSyncPolicy,
    ) {
        if account_id != self.account_id() || !channel.io_admitted() {
            return;
        }
        let peer = channel.peer_identity().clone();
        if !self.linked_devices().iter().any(|device| {
            device.public == peer
                && device.certificate.verify()
                && device.certificate.account_id() == account_id
                && device.certificate.device_ed25519_pub == peer.ed25519_pub
        }) {
            return;
        }
        for device in self.linked_devices() {
            if send_frame(channel, &AccountHistoryFrame::Device { device })
                .await
                .is_err()
            {
                return;
            }
        }
        let offered = self.device_sync_policy();
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
                self.export_sync_records(
                    &peer,
                    cursor,
                    offered.history && wanted.history,
                    offered.attachments && wanted.attachments,
                )
            } else {
                PageBuilder::new(None).finish()
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
            let _ = send_frame(
                channel,
                &AccountHistoryFrame::Complete {
                    next_cursor: None,
                    digest,
                },
            )
            .await;
            return;
        }
        if cursor.is_none() {
            for record in preferences {
                if send_frame(channel, &AccountHistoryFrame::Preference { record })
                    .await
                    .is_err()
                {
                    return;
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
            if send_frame(channel, &header).await.is_err() {
                return;
            }
            for chunk in plaintext.chunks(MAX_CHUNK) {
                if send_frame(
                    channel,
                    &AccountHistoryFrame::RecordChunk {
                        bytes: chunk.to_vec(),
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
            }
        }
        let _ = send_frame(
            channel,
            &AccountHistoryFrame::Complete {
                next_cursor,
                digest,
            },
        )
        .await;
    }

    fn import_account_file_card(&self, record: &FileCardRecord) -> Result<(), NodeError> {
        let host = ConversationId::new(record.host);
        let event_id = EventId::new(record.event_id);
        if let Some(other) = &record.peer_account {
            if other == &self.account_id()
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
        files
            .record_file_alias_durable(&row)
            .map_err(NodeError::Log)?;
        drop(files);
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        self.reseed_live_file_book(&delivery);
        Ok(())
    }

    fn has_live_local_file_card(&self, file_conv: ConversationId) -> bool {
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
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
        for other in self.known_account_ids() {
            hosts.insert(account_conversation_id(&own, &other));
        }
        {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            for channel in book.channel_ids() {
                if book.state(&channel).is_some_and(|state| {
                    !state.is_dissolved()
                        && state.is_accepted_member(&me)
                        && state.is_member(&requester_id)
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
        &self,
        channel: &mut SecureChannel<TcpStream>,
        account_id: &str,
        file_conv: ConversationId,
    ) {
        if account_id != self.account_id()
            || !channel.io_admitted()
            || !self.device_sync_policy().attachments
        {
            return;
        }
        let peer = channel.peer_identity();
        if !self.linked_devices().iter().any(|device| {
            device.public == *peer
                && device.certificate.verify()
                && device.certificate.account_id() == account_id
                && device.certificate.device_ed25519_pub == peer.ed25519_pub
        }) {
            return;
        }
        let Some(manifest) = self.attachment_for_sibling(file_conv, peer) else {
            return;
        };
        let Ok(staging) = tempfile::tempdir() else {
            return;
        };
        let path = staging.path().join("verified-attachment");
        if self.save_directory_archive(file_conv, &path).is_err() {
            return;
        }
        let Ok(mut source) = tokio::fs::File::open(&path).await else {
            return;
        };
        if send_frame(
            channel,
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
            return;
        }
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = vec![0u8; MAX_CHUNK];
        loop {
            let Ok(read) = source.read(&mut buffer).await else {
                return;
            };
            if read == 0 {
                break;
            }
            total += read as u64;
            hasher.update(&buffer[..read]);
            if send_frame(
                channel,
                &AccountHistoryFrame::AttachmentChunk {
                    bytes: buffer[..read].to_vec(),
                },
            )
            .await
            .is_err()
            {
                return;
            }
        }
        let actual: [u8; 32] = hasher.finalize().into();
        if total == manifest.size() && actual == manifest.checksum() {
            let _ = send_frame(channel, &AccountHistoryFrame::AttachmentComplete).await;
        }
    }

    async fn pull_account_attachment(
        &self,
        peer: &PeerRecord,
        file_conv: ConversationId,
    ) -> Result<(), NodeError> {
        if !self.device_sync_policy().attachments {
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
            .privacy_dial(peer.addr, &peer.public)
            .await
            .map_err(transport)?;
        send_frame(
            &mut channel,
            &AccountHistoryFrame::AttachmentRequest {
                version: VERSION,
                account_id: own,
                file_conv: *file_conv.as_bytes(),
            },
        )
        .await?;
        let mut staging: Option<tempfile::TempDir> = None;
        let mut output: Option<std::fs::File> = None;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::File("attachment read timed out".into()))?
                .map_err(transport)?;
            let frame = AccountHistoryFrame::decode(&bytes)
                .ok_or_else(|| NodeError::File("invalid attachment frame".into()))?;
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
                    let file = std::fs::File::create(dir.path().join("incoming"))
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
                    use std::io::Write;
                    output
                        .as_mut()
                        .unwrap()
                        .write_all(&bytes)
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
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                    let dir = staging.take().unwrap();
                    if !self.device_sync_policy().attachments
                        || self
                            .files
                            .lock()
                            .expect("files mutex not poisoned")
                            .manifest(&file_conv)
                            .is_none()
                        || !self.has_live_local_file_card(file_conv)
                    {
                        return Err(NodeError::File("attachment was removed during sync".into()));
                    }
                    // MediaStore encrypts verified plaintext into this account's private store.
                    self.media
                        .store_from_path(file_conv, manifest.name(), &dir.path().join("incoming"))
                        .map_err(|e| NodeError::File(format!("attachment save: {e}")))?;
                    return Ok(());
                }
                _ => return Err(NodeError::File("unexpected attachment frame".into())),
            }
        }
    }

    fn preserve_available_attachment(&self, file_conv: ConversationId) -> Result<bool, NodeError> {
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
        // Preserve an already verified chunk transfer in encrypted managed storage.
        // Saving a generic file can prune its transient chunks, so the archive needs
        // an independent durable copy for this linked device.
        self.media
            .store_from_path(file_conv, manifest.name(), &path)
            .map_err(|e| NodeError::File(format!("attachment save: {e}")))?;
        Ok(true)
    }

    fn apply_account_history_page(&self, page: &[HistoryRecord]) -> Result<(), NodeError> {
        let policy = self.device_sync_policy();
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
                }
            }
        }
        if policy.attachments {
            for record in page {
                if let HistoryRecord::FileCard(record) = record {
                    self.import_account_file_card(record)?;
                }
            }
        }
        Ok(())
    }

    async fn pull_account_history_page(
        &self,
        peer: &PeerRecord,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
    ) -> Result<(Option<HistoryCursor>, [u8; 32]), NodeError> {
        let wanted = self.device_sync_policy();
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
            .privacy_dial(peer.addr, &peer.public)
            .await
            .map_err(transport)?;
        send_frame(
            &mut channel,
            &AccountHistoryFrame::Request {
                version: VERSION,
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
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::Channel("account-history read timed out".into()))?
                .map_err(transport)?;
            let frame = AccountHistoryFrame::decode(&bytes)
                .ok_or_else(|| NodeError::Channel("invalid account-history response".into()))?;
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
                    if self.device_sync_policy().enabled(record.category) {
                        self.account_preferences
                            .lock()
                            .expect("account preferences mutex not poisoned")
                            .merge(record)
                            .map_err(NodeError::Log)?;
                    }
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
                        self.apply_account_history_page(&complete.records)?;
                        return Ok((complete.next_cursor, complete.digest));
                    }
                }
            }
        }
    }

    pub(in crate::node) async fn run_account_history_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RETRY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut cursors: HashMap<String, PeerCursor> = HashMap::new();
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.account_sync_notify.notified() => {}
            }
            let policy = self.device_sync_policy();
            if !policy.history && !policy.attachments && !policy.appearance && !policy.contacts {
                cursors.clear();
                continue;
            }
            for device in self.linked_devices() {
                let id = device.public.user_id();
                if id == self.user_id() {
                    continue;
                }
                let Some(peer) = self.routing_peer(&id) else {
                    self.account_sync_status
                        .lock()
                        .expect("account sync status mutex not poisoned")
                        .insert(
                            id.clone(),
                            DeviceSyncStatus {
                                device_id: id,
                                phase: SyncPhase::Offline,
                                pending_attachments: 0,
                                error: None,
                            },
                        );
                    continue;
                };
                if peer.public != device.public {
                    self.account_sync_status
                        .lock()
                        .expect("account sync status mutex not poisoned")
                        .insert(
                            id.clone(),
                            DeviceSyncStatus {
                                device_id: id,
                                phase: SyncPhase::Error,
                                pending_attachments: 0,
                                error: Some("discovered device identity changed".into()),
                            },
                        );
                    continue;
                }
                let state = cursors.entry(id.clone()).or_default();
                let mut status = DeviceSyncStatus {
                    device_id: id.clone(),
                    phase: SyncPhase::CatchingUp,
                    pending_attachments: 0,
                    error: None,
                };
                if !policy.history && !policy.attachments {
                    state.next = None;
                }
                match self
                    .pull_account_history_page(&peer, state.next, state.known_digest)
                    .await
                {
                    Ok((next, digest)) => {
                        match next {
                            Some(next) if state.next.is_none() => {
                                state.next = Some(next);
                                state.sweep_digest = digest;
                                state.known_digest = [0; 32];
                            }
                            Some(next) if state.sweep_digest == digest => state.next = Some(next),
                            Some(_) => {
                                state.next = None;
                                state.known_digest = [0; 32];
                            }
                            None if state.next.is_none() || state.sweep_digest == digest => {
                                state.next = None;
                                state.known_digest = digest;
                            }
                            None => {
                                state.next = None;
                                state.known_digest = [0; 32];
                            }
                        }
                        if state.next.is_none() {
                            status.phase = SyncPhase::UpToDate;
                        }
                    }
                    Err(error) => {
                        status.phase = SyncPhase::Error;
                        status.error = Some(error.to_string().chars().take(160).collect());
                    }
                }
                if policy.attachments {
                    let mut missing = self
                        .files
                        .lock()
                        .expect("files mutex not poisoned")
                        .file_convs();
                    missing.sort_by_key(|id| *id.as_bytes());
                    missing.retain(|id| !self.media.contains(*id));
                    status.pending_attachments = missing.len();
                    if !missing.is_empty() {
                        let file = missing[state.next_file % missing.len()];
                        state.next_file = state.next_file.wrapping_add(1);
                        let result = match self.preserve_available_attachment(file) {
                            Ok(true) => Ok(()),
                            Ok(false) => self.pull_account_attachment(&peer, file).await,
                            Err(error) => Err(error),
                        };
                        match result {
                            Ok(()) => {
                                status.pending_attachments =
                                    status.pending_attachments.saturating_sub(1)
                            }
                            Err(error) if status.error.is_none() => {
                                status.phase = SyncPhase::Error;
                                status.error = Some(error.to_string().chars().take(160).collect());
                            }
                            Err(_) => {}
                        }
                    }
                    if status.pending_attachments > 0 && status.phase == SyncPhase::UpToDate {
                        status.phase = SyncPhase::CatchingUp;
                    }
                }
                self.account_sync_status
                    .lock()
                    .expect("account sync status mutex not poisoned")
                    .insert(id, status);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;

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
}
