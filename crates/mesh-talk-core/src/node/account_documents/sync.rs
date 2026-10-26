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
pub(in crate::node) struct DocumentSyncCursor {
    pub(super) domain: usize,
    pub(super) after: Option<AccountDocumentKey>,
    pub(super) digests: [[u8; 32]; 9],
    pub(in crate::node) retry_at: Option<Instant>,
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
    hostname_retry_at: Option<Instant>,
}
impl Default for DocumentSyncCursor {
    fn default() -> Self {
        Self {
            domain: 7,
            after: None,
            digests: [[0; 32]; 9],
            retry_at: None,
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
            hostname_retry_at: None,
        }
    }
}
impl DocumentSyncCursor {
    fn hostname_unavailable(&mut self) {
        self.hostname_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.after = None;
        self.domain = 0;
        self.pending = true;
        self.up_to_date = false;
    }
    fn inventory_unavailable(&mut self) {
        self.inventory_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.inventory_upgrade = true;
        self.after = None;
        self.domain = 6;
        self.pending = true;
        self.up_to_date = false;
    }
    fn custody_unavailable(&mut self) {
        self.custody_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.custody_upgrade = true;
        self.after = None;
        self.domain = 0;
        self.pending = true;
        self.up_to_date = false;
    }
    fn controls_unavailable(&mut self) {
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
            && (policy.history || policy.appearance && !self.control_upgrade)
            || self.custody_upgrade && policy.history
            || self.inventory_upgrade;
        if let Some(error) = &self.error
            && (policy.history
                || policy.appearance
                || self.error_domain == Some(AccountDocumentDomain::GroupInventory))
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
    AccountDocumentDomain::DeviceHostname,
];
fn category(domain: AccountDocumentDomain) -> SyncCategory {
    match domain {
        AccountDocumentDomain::Draft
        | AccountDocumentDomain::ReadReceipt
        | AccountDocumentDomain::DmReaction
        | AccountDocumentDomain::DmRecall
        | AccountDocumentDomain::ChannelCustody
        | AccountDocumentDomain::GroupInventory => SyncCategory::History,
        _ => SyncCategory::Appearance,
    }
}
/// Group inventory follows the mandatory account directory contract. Optional
/// message/history and appearance domains retain their existing policy gates.
fn document_enabled(domain: AccountDocumentDomain, policy: super::super::DeviceSyncPolicy) -> bool {
    matches!(
        domain,
        AccountDocumentDomain::GroupInventory | AccountDocumentDomain::DeviceHostname
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
        if cursor
            .hostname_retry_at
            .is_some_and(|at| at <= Instant::now())
        {
            cursor.hostname_retry_at = None;
        }
        let policy = self.device_sync_policy();
        let mut eligible = false;
        for _ in 0..DOMAINS.len() {
            if ((4..6).contains(&cursor.domain) && cursor.control_retry_at.is_some())
                || (cursor.domain == 6 && cursor.custody_retry_at.is_some())
                || (cursor.domain == 7 && cursor.inventory_retry_at.is_some())
                || (cursor.domain == 8 && cursor.hostname_retry_at.is_some())
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
        if result.is_err()
            && match attempted_domain {
                AccountDocumentDomain::DeviceHostname => cursor.hostname_retry_at.is_some(),
                AccountDocumentDomain::GroupInventory => cursor.inventory_retry_at.is_some(),
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
                AccountDocumentDomain::DeviceHostname => 5,
                AccountDocumentDomain::GroupInventory => 4,
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
                        cursor.hostname_unavailable();
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
                            cursor.hostname_unavailable();
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
                    if request.version == 5 {
                        cursor.hostname_retry_at = None;
                    }
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
                    if request.version == 5 {
                        cursor.hostname_retry_at = None;
                    }
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
                    let mut supported_mask = 511u16;
                    if cursor.hostname_retry_at.is_some() {
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
                        && !cursor.inventory_upgrade;
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
