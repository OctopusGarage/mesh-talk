use super::super::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion, MAX_CHUNK};
use super::wire::{DocumentComplete, DocumentRequest, MAX_PAGE_BYTES};
use super::*;
use crate::{
    discovery::roster::PeerRecord,
    transport::{SecureChannel, TransportError},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::TcpStream;
/// A validated exchange proves protocol support for one full source identity.
/// Its local observation time retires only failures predating that exchange.
pub(in crate::node) struct GroupProtocolSupport {
    pub public: crate::identity::device::PublicIdentity,
    pub observed_at: Instant,
}
pub(in crate::node) struct DocumentSyncCursor {
    pub(super) domain: usize,
    pub(super) after: Option<AccountDocumentKey>,
    pub(super) digests: [[u8; 32]; 9],
    pub(in crate::node) retry_at: Option<Instant>,
    last_failure_at: Option<Instant>,
    pub(in crate::node) error: Option<String>,
    error_domain: Option<AccountDocumentDomain>,
    pub(in crate::node) pending: bool,
    pub(in crate::node) up_to_date: bool,
    pub(in crate::node) requires_upgrade: bool,
    pub(super) remotely_disabled: u16,
    visited: u16,
    settled: u16,
    supported: bool,
    control_retry_at: Option<Instant>,
    control_upgrade: bool,
    custody_retry_at: Option<Instant>,
    custody_upgrade: bool,
    inventory_retry_at: Option<Instant>,
    inventory_upgrade: bool,
    group_retry_at: Option<Instant>,
    group_upgrade: bool,
}
impl Default for DocumentSyncCursor {
    fn default() -> Self {
        Self {
            domain: 8,
            after: None,
            digests: [[0; 32]; 9],
            retry_at: None,
            last_failure_at: None,
            error: None,
            error_domain: None,
            pending: false,
            up_to_date: false,
            requires_upgrade: false,
            remotely_disabled: 0,
            visited: 0,
            settled: 0,
            supported: false,
            control_retry_at: None,
            control_upgrade: false,
            custody_retry_at: None,
            custody_upgrade: false,
            inventory_retry_at: None,
            inventory_upgrade: false,
            group_retry_at: None,
            group_upgrade: false,
        }
    }
}
impl DocumentSyncCursor {
    fn group_unavailable(&mut self) {
        self.last_failure_at = Some(Instant::now());
        self.group_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.group_upgrade = true;
        self.after = None;
        self.domain = 7;
        self.pending = true;
        self.up_to_date = false;
    }
    fn group_supported(&mut self) {
        // A validated v5 response proves support for this domain. Clear earlier
        // downgraded-domain cooldowns so a real upgrade resumes immediately.
        self.group_retry_at = None;
        self.group_upgrade = false;
        self.control_retry_at = None;
        self.control_upgrade = false;
        self.custody_retry_at = None;
        self.custody_upgrade = false;
        self.inventory_retry_at = None;
        self.inventory_upgrade = false;
        self.requires_upgrade = false;
        self.last_failure_at = None;
        self.retry_at = None;
        self.error = None;
        self.error_domain = None;
    }
    fn inventory_unavailable(&mut self) {
        self.last_failure_at = Some(Instant::now());
        self.inventory_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.inventory_upgrade = true;
        self.after = None;
        self.domain = 6;
        self.pending = true;
        self.up_to_date = false;
    }
    fn custody_unavailable(&mut self) {
        self.last_failure_at = Some(Instant::now());
        self.custody_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.custody_upgrade = true;
        self.after = None;
        self.domain = 0;
        self.pending = true;
        self.up_to_date = false;
    }
    fn controls_unavailable(&mut self) {
        self.last_failure_at = Some(Instant::now());
        self.control_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.custody_retry_at = self.control_retry_at;
        self.requires_upgrade = true;
        self.control_upgrade = true;
        self.retry_at = None;
        self.error = None;
        self.after = None;
        self.domain = 0;
        self.visited &= 15;
        self.settled &= 15;
        self.pending = true;
        self.up_to_date = false;
    }
    pub(in crate::node) fn status(
        &self,
        policy: super::super::DeviceSyncPolicy,
    ) -> (super::super::SyncPhase, Option<String>, bool) {
        use super::super::SyncPhase;
        let upgrade = self.requires_upgrade
            && ((policy.history || policy.appearance) && !self.control_upgrade
                || policy.history && self.control_upgrade)
            || self.custody_upgrade && policy.history
            || self.inventory_upgrade
            || self.group_upgrade;
        if let Some(error) = &self.error
            && (policy.history
                || policy.appearance
                || self.error_domain == Some(AccountDocumentDomain::GroupInventory)
                || self.error_domain == Some(AccountDocumentDomain::GroupControl))
        {
            return (SyncPhase::Error, Some(error.clone()), upgrade);
        }
        let mut enabled = 0u16;
        for (i, domain) in DOMAINS.iter().enumerate() {
            if document_enabled(*domain, policy) {
                enabled |= 1 << i;
            }
        }
        if self.remotely_disabled & enabled != 0 {
            return (SyncPhase::Disabled, Some("source_sync_off".into()), false);
        }
        (
            if self.up_to_date {
                SyncPhase::UpToDate
            } else {
                SyncPhase::CatchingUp
            },
            None,
            upgrade,
        )
    }
}

#[cfg(test)]
impl DocumentSyncCursor {
    pub(super) fn for_domain(domain: AccountDocumentDomain) -> Self {
        Self {
            domain: domain as usize,
            ..Self::default()
        }
    }
}

const DOMAINS: [AccountDocumentDomain; 9] = [
    AccountDocumentDomain::Draft,
    AccountDocumentDomain::ReadReceipt,
    AccountDocumentDomain::CustomizationPack,
    AccountDocumentDomain::AvatarOverride,
    AccountDocumentDomain::DmReaction,
    AccountDocumentDomain::DmRecall,
    AccountDocumentDomain::ChannelCustody,
    AccountDocumentDomain::GroupInventory,
    AccountDocumentDomain::GroupControl,
];
fn category(domain: AccountDocumentDomain) -> SyncCategory {
    match domain {
        AccountDocumentDomain::Draft
        | AccountDocumentDomain::ReadReceipt
        | AccountDocumentDomain::DmReaction
        | AccountDocumentDomain::DmRecall
        | AccountDocumentDomain::ChannelCustody
        | AccountDocumentDomain::GroupInventory
        | AccountDocumentDomain::GroupControl => SyncCategory::History,
        _ => SyncCategory::Appearance,
    }
}
/// Group inventory follows the mandatory account directory contract. Optional
/// message/history and appearance domains retain their existing policy gates.
fn document_enabled(domain: AccountDocumentDomain, policy: super::super::DeviceSyncPolicy) -> bool {
    matches!(
        domain,
        AccountDocumentDomain::GroupInventory | AccountDocumentDomain::GroupControl
    ) || policy.enabled(category(domain))
}
fn failure() -> NodeError {
    NodeError::Channel("invalid account document exchange".into())
}
impl Node {
    pub(in crate::node) async fn serve_account_documents(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        request: DocumentRequest,
    ) -> bool {
        if request.account_id != self.account_id()
            || !channel.io_admitted()
            || !self.certified_sibling(channel.peer_identity())
            || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
        {
            return false;
        }
        let (policy, revision) = self.device_sync_policy_snapshot();
        let enabled = document_enabled(request.domain, policy);
        let domain = request.domain;
        let after = request.after;
        let known = request.known_digest;
        let Ok((items, digest)) = self
            .account_blocking(move |node| {
                let store = node.account_documents.lock().expect("documents mutex");
                let digest = if enabled {
                    store.digest(domain)
                } else {
                    [0; 32]
                };
                let rows = if !enabled || (after.is_none() && known != [0; 32] && known == digest) {
                    Vec::new()
                } else {
                    store.list(domain, after.as_ref(), 64)
                };
                Ok((rows, digest))
            })
            .await
        else {
            return false;
        };
        let mut selected = Vec::new();
        let mut bytes = 0usize;
        let mut next = None;
        for item in items {
            if bytes + item.content_len as usize > MAX_PAGE_BYTES {
                next = selected.last().map(|m: &AccountDocumentMeta| m.key.clone());
                break;
            }
            bytes += item.content_len as usize;
            selected.push(item);
        }
        if next.is_none()
            && let Some(last) = selected.last()
        {
            let key = last.key.clone();
            let more = self
                .account_documents
                .lock()
                .expect("documents mutex")
                .list(domain, Some(&key), 1);
            if !more.is_empty() {
                next = Some(key);
            }
        }
        let work = async {
            for meta in selected {
                let key = meta.key.clone();
                let expected = meta.clone();
                let Ok(payload) = self
                    .account_blocking(move |node| {
                        let store = node.account_documents.lock().expect("documents mutex");
                        if store.meta(&key).as_ref() != Some(&expected) {
                            return Err(failure());
                        }
                        store.get(&key).map_err(NodeError::Log)
                    })
                    .await
                else {
                    return false;
                };
                let body = serde_json::to_vec(&meta).expect("document metadata serializes");
                if self
                    .send_account_frame(
                        channel,
                        AccountSyncVersion::V2,
                        revision,
                        &AccountHistoryFrame::DocumentHeader { body },
                    )
                    .await
                    .is_err()
                {
                    return false;
                }
                if let Some(document) = payload {
                    for chunk in document.payload().chunks(MAX_CHUNK) {
                        if self
                            .send_account_frame(
                                channel,
                                AccountSyncVersion::V2,
                                revision,
                                &AccountHistoryFrame::DocumentChunk {
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
            }
            let body = serde_json::to_vec(&DocumentComplete {
                domain,
                next,
                digest,
                enabled,
            })
            .expect("document completion serializes");
            self.send_account_frame(
                channel,
                AccountSyncVersion::V2,
                revision,
                &AccountHistoryFrame::DocumentsComplete { body },
            )
            .await
            .is_ok()
        };
        tokio::time::timeout(
            super::super::account_sync::attachment_deadline(bytes as u64),
            work,
        )
        .await
        .unwrap_or(false)
    }
    pub(in crate::node) async fn pull_account_documents_page(
        self: &Arc<Self>,
        peer: &PeerRecord,
        cursor: &mut DocumentSyncCursor,
    ) -> Result<(), NodeError> {
        let supported_at = self
            .account_group_versions
            .lock()
            .expect("group versions mutex")
            .get(&peer.public.user_id())
            .filter(|support| support.public == peer.public)
            .map(|support| support.observed_at);
        if (cursor.group_upgrade
            || cursor.inventory_upgrade
            || cursor.custody_upgrade
            || cursor.control_upgrade
            || cursor.requires_upgrade)
            && cursor
                .last_failure_at
                .zip(supported_at)
                .is_some_and(|(failed, supported)| supported > failed)
        {
            cursor.group_supported();
            cursor.domain = 8;
            cursor.after = None;
        }
        if cursor.retry_at.is_some_and(|at| Instant::now() < at) {
            return Ok(());
        }
        cursor.error = None;
        cursor.error_domain = None;
        if cursor
            .custody_retry_at
            .is_some_and(|at| at <= Instant::now())
        {
            cursor.custody_retry_at = None;
        }
        if cursor
            .control_retry_at
            .is_some_and(|at| at <= Instant::now())
        {
            cursor.control_retry_at = None;
        }
        if cursor
            .inventory_retry_at
            .is_some_and(|at| at <= Instant::now())
        {
            cursor.inventory_retry_at = None;
        }
        if cursor.group_retry_at.is_some_and(|at| at <= Instant::now()) {
            cursor.group_retry_at = None;
        }
        let policy = self.device_sync_policy();
        let mut eligible = false;
        for _ in 0..DOMAINS.len() {
            if ((4..6).contains(&cursor.domain) && cursor.control_retry_at.is_some())
                || (cursor.domain == 6 && cursor.custody_retry_at.is_some())
                || (cursor.domain == 7 && cursor.inventory_retry_at.is_some())
                || (cursor.domain == 8 && cursor.group_retry_at.is_some())
            {
                cursor.domain = (cursor.domain + 1) % DOMAINS.len();
                cursor.after = None;
                continue;
            }
            if document_enabled(DOMAINS[cursor.domain], policy) {
                eligible = true;
                break;
            }
            cursor.visited |= 1 << cursor.domain;
            cursor.settled |= 1 << cursor.domain;
            cursor.domain = (cursor.domain + 1) % DOMAINS.len();
            cursor.after = None;
        }
        if !eligible {
            cursor.up_to_date = false;
            cursor.pending = false;
            return Ok(());
        }
        let attempted_domain = DOMAINS[cursor.domain];
        let result = self.pull_document_exchange(peer, cursor).await;
        if result.is_err() {
            cursor.last_failure_at = Some(Instant::now());
        }
        if result.is_err()
            && match attempted_domain {
                AccountDocumentDomain::GroupInventory => cursor.inventory_retry_at.is_some(),
                AccountDocumentDomain::GroupControl => cursor.group_retry_at.is_some(),
                AccountDocumentDomain::ChannelCustody => cursor.custody_retry_at.is_some(),
                AccountDocumentDomain::DmReaction | AccountDocumentDomain::DmRecall => {
                    cursor.control_retry_at.is_some()
                }
                _ => false,
            }
        {
            return result;
        }
        if result.is_err() {
            cursor.pending = false;
            cursor.up_to_date = false;
            cursor.after = None;
            cursor.digests[cursor.domain] = [0; 32];
            if cursor.retry_at.is_none_or(|at| at <= Instant::now()) {
                cursor.retry_at = Some(Instant::now() + Duration::from_secs(5));
                cursor.error = Some("account document exchange interrupted".into());
                cursor.error_domain = Some(attempted_domain);
            }
        }
        result
    }
    async fn pull_document_exchange(
        self: &Arc<Self>,
        peer: &PeerRecord,
        cursor: &mut DocumentSyncCursor,
    ) -> Result<(), NodeError> {
        let domain = DOMAINS[cursor.domain];
        let (_, revision) = self.device_sync_policy_snapshot();
        let request = DocumentRequest {
            version: match domain {
                AccountDocumentDomain::GroupInventory => 4,
                AccountDocumentDomain::GroupControl => 5,
                AccountDocumentDomain::ChannelCustody => 3,
                AccountDocumentDomain::DmReaction | AccountDocumentDomain::DmRecall => 2,
                _ => 1,
            },
            account_id: self.account_id(),
            domain,
            after: cursor.after.clone(),
            known_digest: cursor.digests[cursor.domain],
        };
        let exchange = self
            .begin_account_exchange(
                peer,
                AccountSyncVersion::V2,
                AccountHistoryFrame::DocumentsRequest {
                    body: request.encode(),
                },
            )
            .await;
        let mut channel = match exchange {
            Ok(channel) => channel,
            Err(error) => {
                if request.version >= 2
                    && matches!(&error,NodeError::Session(super::super::session::SessionError::Transport(TransportError::Io(e))) if matches!(e.kind(),std::io::ErrorKind::UnexpectedEof|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::BrokenPipe))
                {
                    if request.version == 5 {
                        cursor.group_unavailable();
                    } else if request.version == 4 {
                        cursor.inventory_unavailable();
                    } else if request.version == 3 {
                        cursor.custody_unavailable();
                    } else {
                        cursor.controls_unavailable();
                    }
                }
                return Err(error);
            }
        };
        cursor.supported |= channel.reusable();
        let mut pending: Option<(AccountDocumentMeta, zeroize::Zeroizing<Vec<u8>>)> = None;
        let mut last = cursor.after.clone();
        let mut records = 0;
        let mut total = 0usize;
        let started = tokio::time::Instant::now();
        loop {
            let idle = tokio::time::timeout(Duration::from_secs(15), channel.recv());
            let deadline = started + super::super::account_sync::attachment_deadline(total as u64);
            let received = tokio::time::timeout_at(deadline, idle)
                .await
                .map_err(|_| failure())?
                .map_err(|_| failure())?;
            let bytes = match received {
                Ok(bytes) => bytes,
                Err(error) => {
                    let pre_reply_close = matches!(&error,TransportError::Io(e) if matches!(e.kind(),std::io::ErrorKind::UnexpectedEof|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::BrokenPipe));
                    if request.version >= 2 && pre_reply_close {
                        if request.version == 5 {
                            cursor.group_unavailable();
                        } else if request.version == 4 {
                            cursor.inventory_unavailable();
                        } else if request.version == 3 {
                            cursor.custody_unavailable();
                        } else {
                            cursor.controls_unavailable();
                        }
                    } else if !cursor.supported && pre_reply_close {
                        cursor.requires_upgrade = true;
                        cursor.retry_at = Some(Instant::now() + Duration::from_secs(300));
                        cursor.error = Some("peer requires account document upgrade".into());
                    }
                    return Err(failure());
                }
            };
            let Some((AccountSyncVersion::V2, frame)) =
                AccountHistoryFrame::decode_versioned(&bytes)
            else {
                return Err(failure());
            };
            match frame {
                AccountHistoryFrame::DocumentHeader { body } => {
                    cursor.supported = true;
                    if request.version == 4 {
                        cursor.inventory_retry_at = None;
                        cursor.inventory_upgrade = false;
                    }
                    if request.version == 3 {
                        cursor.custody_retry_at = None;
                        cursor.custody_upgrade = false;
                    }
                    if request.version == 2 {
                        cursor.requires_upgrade = false;
                        cursor.control_upgrade = false;
                        cursor.control_retry_at = None;
                    }
                    if let Some((meta, mut payload)) = pending.take() {
                        self.commit_imported_document(
                            meta,
                            std::mem::take(&mut *payload),
                            revision,
                            peer.public.clone(),
                        )
                        .await?;
                    }
                    let meta: AccountDocumentMeta =
                        serde_json::from_slice(&body).map_err(|_| failure())?;
                    if !meta.valid(&self.account.public().ed25519_pub)
                        || meta.key.domain() != domain
                        || last.as_ref().is_some_and(|key| key >= &meta.key)
                        || records >= 64
                    {
                        return Err(failure());
                    }
                    total = total
                        .checked_add(meta.content_len as usize)
                        .ok_or_else(failure)?;
                    if total > MAX_PAGE_BYTES {
                        return Err(failure());
                    }
                    last = Some(meta.key.clone());
                    records += 1;
                    pending = Some((meta, zeroize::Zeroizing::new(Vec::new())));
                }
                AccountHistoryFrame::DocumentChunk { bytes } => {
                    let Some((meta, payload)) = pending.as_mut() else {
                        return Err(failure());
                    };
                    if payload.len() + bytes.len() > meta.content_len as usize {
                        return Err(failure());
                    }
                    payload.extend_from_slice(&bytes);
                }
                AccountHistoryFrame::DocumentsComplete { body } => {
                    cursor.supported = true;
                    if request.version == 4 {
                        cursor.inventory_retry_at = None;
                        cursor.inventory_upgrade = false;
                    }
                    if request.version == 3 {
                        cursor.custody_retry_at = None;
                        cursor.custody_upgrade = false;
                    }
                    if request.version == 2 {
                        cursor.requires_upgrade = false;
                        cursor.control_upgrade = false;
                        cursor.control_retry_at = None;
                    }
                    if let Some((meta, mut payload)) = pending.take() {
                        self.commit_imported_document(
                            meta,
                            std::mem::take(&mut *payload),
                            revision,
                            peer.public.clone(),
                        )
                        .await?;
                    }
                    let complete: DocumentComplete =
                        serde_json::from_slice(&body).map_err(|_| failure())?;
                    if complete.domain != domain
                        || complete
                            .next
                            .as_ref()
                            .is_some_and(|next| Some(next) != last.as_ref() || records == 0)
                        || (!complete.enabled && records > 0)
                    {
                        return Err(failure());
                    }
                    let local = self
                        .account_documents
                        .lock()
                        .expect("documents mutex")
                        .digest(domain);
                    cursor.visited |= 1 << cursor.domain;
                    if complete.enabled {
                        cursor.remotely_disabled &= !(1 << cursor.domain);
                    } else {
                        cursor.remotely_disabled |= 1 << cursor.domain;
                    }
                    if !complete.enabled
                        || complete.next.is_none()
                            && complete.digest != [0; 32]
                            && local == complete.digest
                    {
                        cursor.settled |= 1 << cursor.domain
                    } else {
                        cursor.settled &= !(1 << cursor.domain)
                    }
                    cursor.after = complete.next;
                    if cursor.after.is_none() {
                        cursor.digests[cursor.domain] = if local == complete.digest {
                            complete.digest
                        } else {
                            [0; 32]
                        };
                        cursor.domain = (cursor.domain + 1) % DOMAINS.len();
                    }
                    if request.version == 5 && complete.enabled {
                        cursor.group_supported();
                        self.account_group_versions
                            .lock()
                            .expect("group versions mutex")
                            .insert(
                                peer.public.user_id(),
                                GroupProtocolSupport {
                                    public: peer.public.clone(),
                                    observed_at: Instant::now(),
                                },
                            );
                    }
                    let mut supported_mask = 511u16;
                    if cursor.group_retry_at.is_some() {
                        supported_mask &= !256;
                    }
                    if cursor.inventory_retry_at.is_some() {
                        supported_mask &= !128;
                    }
                    if cursor.control_retry_at.is_some() {
                        supported_mask &= !48;
                    }
                    if cursor.custody_retry_at.is_some() {
                        supported_mask &= !64;
                    }
                    cursor.pending =
                        cursor.after.is_some() || cursor.visited & supported_mask != supported_mask;
                    cursor.up_to_date = cursor.settled & supported_mask == supported_mask
                        && cursor.remotely_disabled == 0
                        && !cursor.requires_upgrade
                        && !cursor.custody_upgrade
                        && !cursor.inventory_upgrade
                        && !cursor.group_upgrade;
                    cursor.retry_at = None;
                    cursor.error = None;
                    channel.recycle(self);
                    return Ok(());
                }
                _ => return Err(failure()),
            }
        }
    }
    async fn commit_imported_document(
        self: &Arc<Self>,
        meta: AccountDocumentMeta,
        payload: Vec<u8>,
        revision: u64,
        peer: crate::identity::device::PublicIdentity,
    ) -> Result<(), NodeError> {
        self.account_blocking(move |node| {
            let payload = zeroize::Zeroizing::new(payload);
            let _commit = node.account_sync_commit.lock().expect("sync commit mutex");
            let (policy, current) = node.device_sync_policy_snapshot();
            if current != revision
                || !document_enabled(meta.key.domain(), policy)
                || !node.certified_sibling(&peer)
            {
                return Err(NodeError::Authorization(
                    "account document policy changed".into(),
                ));
            }
            let private_control = meta.key.is_private_control();
            let changed = node
                .account_documents
                .lock()
                .expect("documents mutex")
                .merge(meta, &payload)
                .map_err(NodeError::Log)?;
            if changed {
                node.documents_changed();
                if private_control {
                    node.delivery_status_notify.notify_waiters();
                    node.note_account_history_change();
                }
            }
            Ok(())
        })
        .await
    }
}
#[cfg(test)]
mod status_tests {
    use super::*;
    use crate::node::{DeviceSyncPolicy, SyncPhase};
    #[test]
    fn document_status_keeps_upgrade_policy_and_digest_completion_distinct() {
        let policy = DeviceSyncPolicy {
            history: true,
            appearance: true,
            ..DeviceSyncPolicy::default()
        };
        let mut cursor = DocumentSyncCursor::default();
        assert_eq!(cursor.status(policy).0, SyncPhase::CatchingUp);
        cursor.visited = 15;
        cursor.settled = 15;
        cursor.up_to_date = true;
        assert_eq!(cursor.status(policy).0, SyncPhase::UpToDate);
        cursor.remotely_disabled = 1;
        cursor.up_to_date = false;
        let disabled = cursor.status(policy);
        assert_eq!(disabled.0, SyncPhase::Disabled);
        assert_eq!(disabled.1.as_deref(), Some("source_sync_off"));
        cursor.requires_upgrade = true;
        cursor.error = Some("peer requires account document upgrade".into());
        let unsupported = cursor.status(policy);
        assert_eq!(unsupported.0, SyncPhase::Error);
        assert!(unsupported.2);
        let off = DeviceSyncPolicy {
            history: false,
            appearance: false,
            ..policy
        };
        let status = cursor.status(off);
        assert_eq!(status.0, SyncPhase::CatchingUp);
        assert!(!status.2);
        assert!(status.1.is_none());
    }
}

#[cfg(test)]
mod control_policy_status_tests {
    use super::*;
    #[test]
    fn control_only_upgrade_hint_is_hidden_when_history_is_disabled() {
        let mut cursor = DocumentSyncCursor::default();
        cursor.controls_unavailable();
        let mut policy = super::super::super::DeviceSyncPolicy {
            appearance: true,
            history: true,
            ..Default::default()
        };
        assert!(cursor.status(policy).2);
        policy.history = false;
        assert!(
            !cursor.status(policy).2,
            "an unsupported disabled category must not require an upgrade"
        );
        policy.history = true;
        assert!(cursor.status(policy).2);
    }
}

#[cfg(test)]
mod group_control_tests {
    use super::*;
    struct UpgradeFixture {
        _directory: tempfile::TempDir,
        source: Arc<Node>,
        target: Arc<Node>,
        peer: PeerRecord,
        servers: tokio::task::JoinSet<()>,
    }
    impl UpgradeFixture {
        async fn new() -> Self {
            use crate::node::{account_devices::AccountDevice, account_documents::tests::node};
            let directory = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let source = node(&directory.path().join("source"), &account);
            let target = node(&directory.path().join("target"), &account);
            for (left, right) in [(&source, &target), (&target, &source)] {
                left.account_devices
                    .lock()
                    .unwrap()
                    .insert(AccountDevice {
                        public: right.identity.public(),
                        certificate: account.certify(&right.identity.public().ed25519_pub),
                    })
                    .unwrap();
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let peer = PeerRecord {
                public: source.identity.public(),
                addr: listener.local_addr().unwrap(),
                name: "Upgraded source fixture".into(),
                account_id: Some(account.account_id()),
                last_seen: Instant::now(),
                post_office: false,
                routes: vec![],
            };
            let mut servers = tokio::task::JoinSet::new();
            servers.spawn(source.clone().run_accept_loop(listener));
            Self {
                _directory: directory,
                source,
                target,
                peer,
                servers,
            }
        }
        async fn prove_support(&self) {
            self.target
                .pull_document_exchange(&self.peer, &mut DocumentSyncCursor::default())
                .await
                .unwrap();
        }
        async fn poll(&self, cursor: &mut DocumentSyncCursor) {
            self.target
                .pull_account_documents_page(&self.peer, cursor)
                .await
                .unwrap();
        }
    }
    fn cooling_cursor() -> DocumentSyncCursor {
        let mut cursor = DocumentSyncCursor::default();
        cursor.group_unavailable();
        cursor.inventory_unavailable();
        cursor.custody_unavailable();
        cursor.controls_unavailable();
        cursor.domain = 8;
        assert!(cursor.group_retry_at.unwrap() > Instant::now() + Duration::from_secs(240));
        cursor
    }
    #[tokio::test]
    async fn a_v5_response_on_one_cursor_unblocks_another_only_for_the_exact_source() {
        for wrong_identity in [true, false] {
            let mut fixture = UpgradeFixture::new().await;
            let mut background = cooling_cursor();
            // A different foreground probe learns that the peer now supports v5.
            fixture.prove_support().await;
            if wrong_identity {
                fixture
                    .target
                    .account_group_versions
                    .lock()
                    .unwrap()
                    .get_mut(&fixture.source.user_id())
                    .unwrap()
                    .public
                    .x25519_pub[0] ^= 1;
            }
            fixture.poll(&mut background).await;
            fixture.servers.shutdown().await;
            if wrong_identity {
                assert!(
                    background.group_retry_at.is_some(),
                    "same short ID with another transport key is not support evidence"
                );
                assert!(background.group_upgrade);
            } else {
                assert!(
                    background.group_retry_at.is_none(),
                    "authenticated support learned on another cursor must retire the old cooldown"
                );
                assert!(background.inventory_retry_at.is_none());
                assert!(background.custody_retry_at.is_none());
                assert!(background.control_retry_at.is_none());
                assert!(!background.status(fixture.target.device_sync_policy()).2);
            }
        }
    }
    #[tokio::test]
    async fn old_support_cannot_clear_a_later_failure_or_create_a_retry_spin() {
        let mut fixture = UpgradeFixture::new().await;
        fixture.prove_support().await;
        let mut background = cooling_cursor();
        fixture.poll(&mut background).await;
        assert!(
            background.group_retry_at.is_some(),
            "support observed before the failure cannot cancel its backoff"
        );
        fixture.prove_support().await;
        fixture.poll(&mut background).await;
        fixture.servers.shutdown().await;
        assert!(
            background.group_retry_at.is_none(),
            "a genuinely later v5 exchange resumes the waiting cursor"
        );
    }
    #[tokio::test]
    async fn another_cursors_support_does_not_reset_ordinary_document_pagination() {
        let mut fixture = UpgradeFixture::new().await;
        let first = AccountDocumentKey::Draft {
            conversation: DocumentConversation::Account("a".repeat(32)),
        };
        let second = AccountDocumentKey::Draft {
            conversation: DocumentConversation::Account("b".repeat(32)),
        };
        for key in [&first, &second] {
            fixture
                .source
                .put_account_document_if(key.clone(), b"retained draft".to_vec(), None, |commit| {
                    commit()
                })
                .unwrap();
        }
        fixture.prove_support().await;
        let mut background = DocumentSyncCursor::for_domain(AccountDocumentDomain::Draft);
        background.after = Some(first.clone());
        fixture.poll(&mut background).await;
        fixture.servers.shutdown().await;
        assert!(fixture.target.account_document(&first).unwrap().is_none());
        assert_eq!(
            fixture
                .target
                .account_document(&second)
                .unwrap()
                .unwrap()
                .payload(),
            b"retained draft"
        );
    }
    #[test]
    fn ninth_domain_mask_is_mandatory_even_with_all_optional_content_disabled() {
        let mut cursor = DocumentSyncCursor::default();
        let policy = crate::node::DeviceSyncPolicy {
            history: false,
            appearance: false,
            attachments: false,
            contacts: false,
            profile: false,
        };
        assert_eq!(cursor.domain, 8);
        assert_eq!(cursor.digests.len(), 9);
        assert!(document_enabled(
            AccountDocumentDomain::GroupControl,
            policy
        ));
        cursor.remotely_disabled = 1 << 8;
        assert_eq!(cursor.status(policy).0, crate::node::SyncPhase::Disabled);
        cursor.remotely_disabled = 0;
        cursor.group_unavailable();
        assert!(
            cursor.status(policy).2,
            "mandatory control upgrades cannot disappear with history disabled"
        );
    }

    #[tokio::test]
    async fn authenticated_v5_response_clears_older_domain_cooldowns_without_waiting() {
        use crate::node::{account_devices::AccountDevice, account_documents::tests::node};
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = node(&dir.path().join("source"), &account);
        let target = node(&dir.path().join("target"), &account);
        for (left, right) in [(&source, &target), (&target, &source)] {
            left.account_devices
                .lock()
                .unwrap()
                .insert(AccountDevice {
                    public: right.identity.public(),
                    certificate: account.certify(&right.identity.public().ed25519_pub),
                })
                .unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = PeerRecord {
            public: source.identity.public(),
            addr: listener.local_addr().unwrap(),
            name: "Upgraded source fixture".into(),
            account_id: Some(account.account_id()),
            last_seen: Instant::now(),
            post_office: false,
            routes: vec![],
        };
        let server = tokio::spawn(source.clone().run_accept_loop(listener));
        let mut cursor = DocumentSyncCursor::default();
        cursor.group_unavailable();
        cursor.inventory_unavailable();
        cursor.custody_unavailable();
        cursor.controls_unavailable();
        cursor.domain = 8;
        // Exercise the authenticated exchange itself, as a fresh capability
        // probe, rather than advancing the clock or clearing cooldown flags.
        target
            .pull_document_exchange(&peer, &mut cursor)
            .await
            .unwrap();
        server.abort();
        let _ = server.await;
        assert!(cursor.group_retry_at.is_none());
        assert!(cursor.inventory_retry_at.is_none());
        assert!(cursor.custody_retry_at.is_none());
        assert!(cursor.control_retry_at.is_none());
        assert!(
            !cursor.group_upgrade
                && !cursor.inventory_upgrade
                && !cursor.custody_upgrade
                && !cursor.control_upgrade
                && !cursor.requires_upgrade
        );
        assert_eq!(
            target
                .account_group_versions
                .lock()
                .unwrap()
                .get(&source.user_id())
                .map(|support| &support.public),
            Some(&source.identity.public())
        );
    }
}
