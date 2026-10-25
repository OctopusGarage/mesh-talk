use super::super::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion, MAX_CHUNK};
use super::wire::{
    DocumentComplete, DocumentRequest, MAX_PAGE_BYTES, OperationCapability, OperationContinuation,
    OperationProbe,
};
use super::*;
use crate::{
    discovery::roster::PeerRecord,
    transport::{SecureChannel, TransportError},
};
use rand_core::RngCore;
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
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum DocumentSyncScope {
    #[default]
    All,
    Mandatory,
    Optional,
}
impl DocumentSyncScope {
    fn mask(self) -> u16 {
        match self {
            Self::All => (1 << DOMAINS.len()) - 1,
            Self::Mandatory => {
                (1 << AccountDocumentDomain::GroupInventory as usize)
                    | (1 << AccountDocumentDomain::DeviceHostname as usize)
                    | (1 << AccountDocumentDomain::GroupControl as usize)
            }
            Self::Optional => 127,
        }
    }
}
pub(in crate::node) struct DocumentSyncCursor {
    scope: DocumentSyncScope,
    pub(super) domain: usize,
    pub(super) after: Option<AccountDocumentKey>,
    pub(super) digests: [[u8; 32]; 10],
    // The legacy digest remains access-only, even when the peer ignores the
    // optional operation probe. Its digest and continuation have a separate scope.
    operation_digest: [u8; 32],
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
    hostname_retry_at: Option<Instant>,
}
impl Default for DocumentSyncCursor {
    fn default() -> Self {
        Self {
            scope: DocumentSyncScope::All,
            domain: AccountDocumentDomain::GroupControl as usize,
            after: None,
            digests: [[0; 32]; 10],
            operation_digest: [0; 32],
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
            hostname_retry_at: None,
        }
    }
}
impl DocumentSyncCursor {
    pub(in crate::node) fn mandatory() -> Self {
        Self {
            scope: DocumentSyncScope::Mandatory,
            ..Self::default()
        }
    }
    pub(in crate::node) fn optional() -> Self {
        Self {
            scope: DocumentSyncScope::Optional,
            domain: 0,
            ..Self::default()
        }
    }
    fn enabled_mask(&self, policy: super::super::DeviceSyncPolicy) -> u16 {
        DOMAINS.iter().enumerate().fold(0, |mask, (index, domain)| {
            if self.scope.mask() & (1 << index) != 0 && document_enabled(*domain, policy) {
                mask | (1 << index)
            } else {
                mask
            }
        })
    }
    #[cfg(test)]
    fn group_unavailable(&mut self) {
        self.last_failure_at = Some(Instant::now());
        self.group_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.group_upgrade = true;
        self.after = None;
        self.domain = 7;
        self.pending = true;
        self.up_to_date = false;
    }
    fn group_interrupted(&mut self, received_header: bool) {
        self.last_failure_at = Some(Instant::now());
        self.group_upgrade = false;
        self.group_retry_at =
            (!received_header).then(|| Instant::now() + Duration::from_millis(100));
        self.after = None;
        self.digests[AccountDocumentDomain::GroupControl as usize] = [0; 32];
        self.operation_digest = [0; 32];
        self.settled &= !(1 << AccountDocumentDomain::GroupControl as usize);
        self.pending = true;
        self.up_to_date = false;
        if !received_header {
            // An older source may not understand v5. Its v4 inventory can
            // still progress, without inventing authenticated upgrade evidence.
            self.domain = 7;
        }
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
    fn hostname_unavailable(&mut self) {
        self.hostname_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.after = None;
        self.domain = 0;
        self.pending = true;
        self.up_to_date = false;
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
        let enabled = self.enabled_mask(policy);
        if enabled == 0 {
            return (SyncPhase::UpToDate, None, false);
        }
        let upgrade = self.requires_upgrade
            && ((policy.history || policy.appearance) && !self.control_upgrade
                || policy.history && self.control_upgrade)
            || self.custody_upgrade && policy.history && self.scope.mask() & 64 != 0
            || self.inventory_upgrade && self.scope.mask() & 128 != 0
            || self.group_upgrade
                && self.scope.mask() & (1 << AccountDocumentDomain::GroupControl as usize) != 0;
        if let Some(error) = &self.error
            && (policy.history
                || policy.appearance
                || self.error_domain == Some(AccountDocumentDomain::GroupInventory)
                || self.error_domain == Some(AccountDocumentDomain::GroupControl))
        {
            return (SyncPhase::Error, Some(error.clone()), upgrade);
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

const DOMAINS: [AccountDocumentDomain; 10] = [
    AccountDocumentDomain::Draft,
    AccountDocumentDomain::ReadReceipt,
    AccountDocumentDomain::CustomizationPack,
    AccountDocumentDomain::AvatarOverride,
    AccountDocumentDomain::DmReaction,
    AccountDocumentDomain::DmRecall,
    AccountDocumentDomain::ChannelCustody,
    AccountDocumentDomain::GroupInventory,
    AccountDocumentDomain::DeviceHostname,
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
        AccountDocumentDomain::GroupInventory
            | AccountDocumentDomain::DeviceHostname
            | AccountDocumentDomain::GroupControl
    ) || policy.enabled(category(domain))
}
fn failure() -> NodeError {
    NodeError::Channel("invalid account document exchange".into())
}
fn transport_closed(error: &NodeError) -> bool {
    matches!(error,
        NodeError::Session(super::super::session::SessionError::Transport(TransportError::Io(error)))
        if matches!(error.kind(),std::io::ErrorKind::UnexpectedEof
            |std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::BrokenPipe))
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
        let operation = request
            .operation
            .filter(|probe| domain == AccountDocumentDomain::GroupControl && probe.supported());
        let include_operations = operation.is_some();
        let after = operation
            .as_ref()
            .and_then(|probe| probe.after.as_ref().map(OperationContinuation::key))
            .or(request.after);
        let known = operation
            .as_ref()
            .map_or(request.known_digest, |probe| probe.known_digest);
        let operation = operation.map(|probe| OperationCapability {
            version: 1,
            nonce: probe.nonce,
        });
        let Ok((items, digest)) = self
            .account_blocking(move |node| {
                let store = node.account_documents.lock().expect("documents mutex");
                let digest = if enabled {
                    store.digest_scope(domain, include_operations)
                } else {
                    [0; 32]
                };
                let rows = if !enabled || (after.is_none() && known != [0; 32] && known == digest) {
                    Vec::new()
                } else {
                    store.list_scope(domain, after.as_ref(), 64, include_operations)
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
                .list_scope(domain, Some(&key), 1, include_operations);
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
                operation,
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
            if cursor.scope != DocumentSyncScope::Optional {
                cursor.domain = AccountDocumentDomain::GroupControl as usize;
                cursor.after = None;
            }
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
        if cursor
            .hostname_retry_at
            .is_some_and(|at| at <= Instant::now())
        {
            cursor.hostname_retry_at = None;
        }
        let policy = self.device_sync_policy();
        let mut eligible = false;
        for _ in 0..DOMAINS.len() {
            if cursor.scope.mask() & (1 << cursor.domain) == 0 {
                cursor.domain = (cursor.domain + 1) % DOMAINS.len();
                cursor.after = None;
                continue;
            }
            if ((4..6).contains(&cursor.domain) && cursor.control_retry_at.is_some())
                || (cursor.domain == 6 && cursor.custody_retry_at.is_some())
                || (cursor.domain == 7 && cursor.inventory_retry_at.is_some())
                || (DOMAINS[cursor.domain] == AccountDocumentDomain::GroupControl
                    && cursor.group_retry_at.is_some())
                || (DOMAINS[cursor.domain] == AccountDocumentDomain::DeviceHostname
                    && cursor.hostname_retry_at.is_some())
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
            cursor.up_to_date = cursor.enabled_mask(policy) == 0;
            cursor.pending = false;
            return Ok(());
        }
        let attempted_domain = DOMAINS[cursor.domain];
        let result = self.pull_document_exchange(peer, cursor).await;
        if result.is_err() {
            cursor.last_failure_at = Some(Instant::now());
        }
        if attempted_domain == AccountDocumentDomain::GroupControl
            && result.as_ref().is_err_and(transport_closed)
        {
            cursor.pending = true;
            cursor.up_to_date = false;
            cursor.after = None;
            cursor.digests[AccountDocumentDomain::GroupControl as usize] = [0; 32];
            // Source publication can invalidate a selected page. Retry that
            // page promptly; malformed frames/nonces keep the ordinary error
            // path and are never treated as a transport interruption.
            cursor.retry_at = cursor
                .group_retry_at
                .is_none()
                .then(|| Instant::now() + Duration::from_millis(100));
            cursor.error = None;
            cursor.error_domain = None;
            return result;
        }
        if result.is_err()
            && match attempted_domain {
                AccountDocumentDomain::DeviceHostname => cursor.hostname_retry_at.is_some(),
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
        let operation_probe = DOMAINS[cursor.domain] == AccountDocumentDomain::GroupControl;
        let result = self.pull_document_exchange_inner(peer, cursor).await;
        if operation_probe && result.is_err() {
            self.retire_operation_support(&peer.public, Instant::now());
        }
        result
    }
    fn retire_operation_support(
        &self,
        peer: &crate::identity::device::PublicIdentity,
        observed_at: Instant,
    ) {
        let mut support = self
            .group_operation_versions
            .lock()
            .expect("operation versions mutex");
        if support
            .get(&peer.user_id())
            .is_some_and(|known| known.public == *peer && known.observed_at <= observed_at)
        {
            support.remove(&peer.user_id());
        }
    }
    async fn pull_document_exchange_inner(
        self: &Arc<Self>,
        peer: &PeerRecord,
        cursor: &mut DocumentSyncCursor,
    ) -> Result<(), NodeError> {
        let domain = DOMAINS[cursor.domain];
        let (_, revision) = self.device_sync_policy_snapshot();
        let operation = if domain == AccountDocumentDomain::GroupControl {
            // Retain authenticated support while refreshing it. An in-flight
            // request has no negative outcome; failure/no-echo retires support
            // when that outcome is actually observed below.
            let mut nonce = [0; 32];
            while nonce == [0; 32] {
                rand_core::OsRng.fill_bytes(&mut nonce);
            }
            Some(OperationProbe {
                version: 1,
                nonce,
                after: match &cursor.after {
                    Some(AccountDocumentKey::GroupOperation {
                        channel,
                        source_device,
                    }) => Some(OperationContinuation {
                        channel: channel.clone(),
                        source_device: source_device.clone(),
                    }),
                    _ => None,
                },
                known_digest: std::mem::take(&mut cursor.operation_digest),
            })
        } else {
            None
        };
        let request = DocumentRequest {
            version: match domain {
                AccountDocumentDomain::DeviceHostname => 5,
                AccountDocumentDomain::GroupInventory => 4,
                AccountDocumentDomain::GroupControl => 5,
                AccountDocumentDomain::ChannelCustody => 3,
                AccountDocumentDomain::DmReaction | AccountDocumentDomain::DmRecall => 2,
                _ => 1,
            },
            account_id: self.account_id(),
            domain,
            after: cursor
                .after
                .clone()
                .filter(|key| !matches!(key, AccountDocumentKey::GroupOperation { .. })),
            known_digest: cursor.digests[cursor.domain],
            operation,
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
                    if domain == AccountDocumentDomain::GroupControl {
                        cursor.group_interrupted(false);
                    } else if domain == AccountDocumentDomain::DeviceHostname {
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
        if domain == AccountDocumentDomain::GroupControl
            && (channel.peer_identity() != &peer.public
                || !self.certified_sibling(&peer.public)
                || !self.account_allowed(channel.peer_identity(), channel.peer_announcement()))
        {
            return Err(failure());
        }
        let mut pending: Option<(AccountDocumentMeta, zeroize::Zeroizing<Vec<u8>>)> = None;
        let mut group_records = Vec::new();
        // An old peer ignores the optional operation cursor and may legitimately
        // return access rows below it. Apply that bound only after its echo.
        let mut last = request.after.clone();
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
                        if domain == AccountDocumentDomain::GroupControl {
                            // Each access observation is independently signed.
                            // Preserve only fully received, validated role rows;
                            // operation rows still require the matching terminal
                            // capability and never enter the store on EOF.
                            if let Some(row) = pending.take() {
                                group_records.push(row);
                            }
                            for (meta, mut payload) in group_records.drain(..) {
                                if matches!(meta.key, AccountDocumentKey::GroupControl { .. })
                                    && payload.len() == meta.content_len as usize
                                {
                                    self.commit_imported_document(
                                        meta,
                                        std::mem::take(&mut *payload),
                                        revision,
                                        peer.public.clone(),
                                    )
                                    .await?;
                                }
                            }
                            cursor.group_interrupted(records > 0);
                            return Err(NodeError::Session(
                                super::super::session::SessionError::Transport(error),
                            ));
                        } else if domain == AccountDocumentDomain::DeviceHostname {
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
                    if domain == AccountDocumentDomain::DeviceHostname {
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
                        if domain == AccountDocumentDomain::GroupControl {
                            group_records.push((meta, payload));
                        } else {
                            self.commit_imported_document(
                                meta,
                                std::mem::take(&mut *payload),
                                revision,
                                peer.public.clone(),
                            )
                            .await?;
                        }
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
                    // Role observations have their own source signature and do
                    // not depend on the optional operation capability echo.
                    // Commit a complete role row before waiting for a later
                    // operation blob, which may be missing or interrupted.
                    if domain == AccountDocumentDomain::GroupControl
                        && matches!(meta.key, AccountDocumentKey::GroupControl { .. })
                        && payload.len() == meta.content_len as usize
                    {
                        if !channel.io_admitted()
                            || channel.peer_identity() != &peer.public
                            || !self.certified_sibling(&peer.public)
                            || !self.account_allowed(
                                channel.peer_identity(),
                                channel.peer_announcement(),
                            )
                        {
                            return Err(failure());
                        }
                        let (meta, mut payload) = pending.take().expect("complete role row");
                        self.commit_imported_document(
                            meta,
                            std::mem::take(&mut *payload),
                            revision,
                            peer.public.clone(),
                        )
                        .await?;
                    }
                }
                AccountHistoryFrame::DocumentsComplete { body } => {
                    cursor.supported = true;
                    if domain == AccountDocumentDomain::DeviceHostname {
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
                        if domain == AccountDocumentDomain::GroupControl {
                            group_records.push((meta, payload));
                        } else {
                            self.commit_imported_document(
                                meta,
                                std::mem::take(&mut *payload),
                                revision,
                                peer.public.clone(),
                            )
                            .await?;
                        }
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
                    // The authenticated Noise channel and a fresh nonce bind the
                    // capability to this exact full peer and current exchange.
                    // A plain v5 response proves access support only.
                    let include_operations = match (&request.operation, &complete.operation) {
                        (Some(probe), Some(capability))
                            if capability.version == probe.version
                                && capability.nonce == probe.nonce =>
                        {
                            true
                        }
                        (_, Some(_)) => return Err(failure()),
                        _ => false,
                    };
                    if domain == AccountDocumentDomain::GroupControl {
                        let effective_after = if include_operations {
                            request
                                .operation
                                .as_ref()
                                .and_then(|probe| {
                                    probe.after.as_ref().map(OperationContinuation::key)
                                })
                                .or_else(|| request.after.clone())
                        } else {
                            request.after.clone()
                        };
                        if !channel.io_admitted()
                            || channel.peer_identity() != &peer.public
                            || !self.certified_sibling(&peer.public)
                            || !self.account_allowed(
                                channel.peer_identity(),
                                channel.peer_announcement(),
                            )
                            || group_records.iter().any(|(meta, _)| {
                                (!include_operations
                                    && matches!(
                                        meta.key,
                                        AccountDocumentKey::GroupOperation { .. }
                                    ))
                                    || effective_after
                                        .as_ref()
                                        .is_some_and(|after| after >= &meta.key)
                            })
                        {
                            return Err(failure());
                        }
                        // Keep the entire bounded control page out of the store
                        // until capability and continuation scope are verified.
                        for (meta, mut payload) in group_records.drain(..) {
                            self.commit_imported_document(
                                meta,
                                std::mem::take(&mut *payload),
                                revision,
                                peer.public.clone(),
                            )
                            .await?;
                        }
                    }
                    let local = self
                        .account_documents
                        .lock()
                        .expect("documents mutex")
                        .digest_scope(domain, include_operations);
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
                        cursor.digests[cursor.domain] =
                            if domain == AccountDocumentDomain::GroupControl {
                                // Legacy peers continue to compare access-only
                                // metadata, regardless of the optional probe scope.
                                self.account_documents
                                    .lock()
                                    .expect("documents mutex")
                                    .digest_scope(domain, false)
                            } else if local == complete.digest {
                                complete.digest
                            } else {
                                [0; 32]
                            };
                        if include_operations && local == complete.digest {
                            cursor.operation_digest = complete.digest;
                        }
                        cursor.domain = (cursor.domain + 1) % DOMAINS.len();
                    }
                    if domain == AccountDocumentDomain::GroupControl
                        && (!include_operations || !complete.enabled)
                    {
                        self.retire_operation_support(&peer.public, Instant::now());
                    }
                    if domain == AccountDocumentDomain::GroupControl && complete.enabled {
                        if !channel.io_admitted()
                            || channel.peer_identity() != &peer.public
                            || !self.certified_sibling(&peer.public)
                            || !self.account_allowed(
                                channel.peer_identity(),
                                channel.peer_announcement(),
                            )
                        {
                            return Err(failure());
                        }
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
                        if include_operations {
                            self.group_operation_versions
                                .lock()
                                .expect("operation versions mutex")
                                .insert(
                                    peer.public.user_id(),
                                    GroupProtocolSupport {
                                        public: peer.public.clone(),
                                        observed_at: Instant::now(),
                                    },
                                );
                        }
                    }
                    let mut supported_mask = cursor.scope.mask();
                    if cursor.group_retry_at.is_some() {
                        supported_mask &= !(1 << AccountDocumentDomain::GroupControl as usize);
                    }
                    if cursor.hostname_retry_at.is_some() {
                        supported_mask &= !(1 << AccountDocumentDomain::DeviceHostname as usize);
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
                        && cursor.remotely_disabled & cursor.scope.mask() == 0
                        && !cursor.requires_upgrade
                        && (cursor.scope.mask() & 64 == 0 || !cursor.custody_upgrade)
                        && (cursor.scope.mask() & 128 == 0 || !cursor.inventory_upgrade)
                        && (cursor.scope.mask()
                            & (1 << AccountDocumentDomain::GroupControl as usize)
                            == 0
                            || !cursor.group_upgrade);
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
    #[tokio::test]
    async fn shared_historical_digest_byte_never_allows_cross_domain_metadata() {
        for request_hostname in [false, true] {
            let mut fixture = UpgradeFixture::new().await;
            fixture
                .source
                .set_device_hostname(Some("private host"))
                .unwrap();
            let group = fixture
                .source
                .create_channel("domain isolation", vec![])
                .await
                .unwrap();
            fixture.source.publish_account_group_access_for(group);
            let (requested, wrong_key) = if request_hostname {
                (
                    AccountDocumentDomain::DeviceHostname,
                    AccountDocumentKey::GroupControl {
                        channel: hex::encode(group.as_bytes()),
                        source_device: fixture.source.user_id(),
                    },
                )
            } else {
                (
                    AccountDocumentDomain::GroupControl,
                    AccountDocumentKey::DeviceHostname {
                        device_id: fixture.source.user_id(),
                    },
                )
            };
            let document = fixture
                .source
                .account_document(&wrong_key)
                .unwrap()
                .unwrap();
            let meta = document.meta.clone();
            let payload = document.payload().to_vec();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut peer = fixture.peer.clone();
            peer.addr = listener.local_addr().unwrap();
            let source = fixture.source.clone();
            let responder = tokio::spawn(async move {
                let (mut channel, _) = receive_document_request(&source, listener).await;
                for frame in [
                    AccountHistoryFrame::DocumentHeader {
                        body: serde_json::to_vec(&meta).unwrap(),
                    },
                    AccountHistoryFrame::DocumentChunk { bytes: payload },
                ] {
                    if channel
                        .send(&frame.encode_version(AccountSyncVersion::V2).unwrap())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            assert!(
                fixture
                    .target
                    .pull_document_exchange(&peer, &mut DocumentSyncCursor::for_domain(requested))
                    .await
                    .is_err()
            );
            responder.await.unwrap();
            assert!(
                fixture.target.account_document_meta(&wrong_key).is_none(),
                "a valid signed key from another explicit domain must never be imported"
            );
            fixture.servers.shutdown().await;
        }
    }

    #[tokio::test]
    async fn a_changed_role_page_retries_without_claiming_the_authenticated_source_is_unsupported()
    {
        use crate::node::AccountGroupRole;
        let mut fixture = UpgradeFixture::new().await;
        let first = fixture
            .source
            .create_channel("first role", vec![])
            .await
            .unwrap();
        let second = fixture
            .source
            .create_channel("second role", vec![])
            .await
            .unwrap();
        fixture.source.publish_account_group_access(&mut None);
        let selected = fixture.source.account_documents.lock().unwrap().list_scope(
            AccountDocumentDomain::GroupControl,
            None,
            64,
            false,
        );
        assert_eq!(selected.len(), 2);
        let first_key = selected[0].key.clone();
        let second_key = selected[1].key.clone();
        let changed_group = match &second_key {
            AccountDocumentKey::GroupControl { channel, .. }
                if *channel == hex::encode(first.as_bytes()) =>
            {
                first
            }
            AccountDocumentKey::GroupControl { channel, .. }
                if *channel == hex::encode(second.as_bytes()) =>
            {
                second
            }
            _ => panic!("expected a selected real group role"),
        };
        let (blocked, blocked_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let blocked = Arc::new(std::sync::Mutex::new(Some(blocked)));
        let release_rx = Arc::new(std::sync::Mutex::new(Some(release_rx)));
        let exact_peer = fixture.target.identity.public();
        let blocked_key = first_key.clone();
        *fixture.source.account_session_test_hook.lock().unwrap() =
            Some(Arc::new(move |peer, frame| {
                let blocked = blocked.clone();
                let release_rx = release_rx.clone();
                let blocked_key = blocked_key.clone();
                let exact_peer = exact_peer.clone();
                Box::pin(async move {
                    if peer != exact_peer {
                        return;
                    }
                    let AccountHistoryFrame::DocumentHeader { body } = frame else {
                        return;
                    };
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    if meta.key != blocked_key {
                        return;
                    }
                    let signal = blocked.lock().unwrap().take();
                    if let Some(signal) = signal {
                        signal.send(()).unwrap();
                        let released = release_rx.lock().unwrap().take().unwrap();
                        released.await.unwrap();
                    }
                })
            }));
        let target = fixture.target.clone();
        let peer = fixture.peer.clone();
        let pulling = tokio::spawn(async move {
            let mut cursor = DocumentSyncCursor::mandatory();
            let result = target.pull_account_documents_page(&peer, &mut cursor).await;
            (result, cursor)
        });
        tokio::time::timeout(Duration::from_secs(5), blocked_rx)
            .await
            .unwrap()
            .unwrap();
        fixture
            .source
            .dissolve_channel(changed_group)
            .await
            .unwrap();
        fixture.source.publish_account_group_access(&mut None);
        let updated = fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .meta(&second_key)
            .unwrap();
        assert!(
            updated.content_hash != selected[1].content_hash,
            "the actual dissolve must replace the second selected role observation"
        );
        release.send(()).unwrap();
        let (result, mut cursor) = tokio::time::timeout(Duration::from_secs(5), pulling)
            .await
            .unwrap()
            .unwrap();
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        assert!(result.is_err() || cursor.pending);
        assert!(
            !cursor.group_upgrade && !cursor.requires_upgrade,
            "a changed page after a valid signed role header is not authenticated unsupported evidence"
        );
        assert!(
            cursor.group_retry_at.is_none(),
            "ordinary source publication must not impose a five-minute group protocol cooldown"
        );
        assert!(cursor.pending && !cursor.up_to_date);
        assert!(
            fixture
                .target
                .account_documents
                .lock()
                .unwrap()
                .meta(&first_key)
                .is_some(),
            "a fully received independently valid role observation survives the interrupted page"
        );
        assert!(
            fixture
                .target
                .account_documents
                .lock()
                .unwrap()
                .meta(&second_key)
                .is_none(),
            "the changed row was never received and cannot be installed from the stale selection"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let _ = fixture
                    .target
                    .pull_account_documents_page(&fixture.peer, &mut cursor)
                    .await;
                if fixture
                    .target
                    .account_group_access(changed_group)
                    .unwrap()
                    .role
                    == AccountGroupRole::Dissolved
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a stable replacement role must converge promptly after the page retry");
        fixture.servers.shutdown().await;
    }
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
        cursor.domain = AccountDocumentDomain::GroupControl as usize;
        assert!(cursor.group_retry_at.unwrap() > Instant::now() + Duration::from_secs(240));
        cursor
    }
    #[tokio::test]
    async fn authenticated_operation_probe_establishes_separate_full_source_support() {
        let mut fixture = UpgradeFixture::new().await;
        fixture.prove_support().await;
        assert!(
            fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .get(&fixture.source.user_id())
                .is_some_and(|support| support.public == fixture.source.identity.public()),
            "an operation-capable authenticated exchange must establish its own support evidence"
        );
        fixture.servers.shutdown().await;
    }
    /// Speak the access-only v5 JSON dialect over a real authenticated Noise
    /// session. Unknown JSON fields are deliberately ignored by this responder.
    async fn receive_document_request(
        source: &Arc<Node>,
        listener: tokio::net::TcpListener,
    ) -> (SecureChannel<TcpStream>, Vec<u8>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = source.privacy_accept(stream).await.unwrap();
        let bytes = channel.recv().await.unwrap();
        let (_, frame) = AccountHistoryFrame::decode_versioned(&bytes).unwrap();
        let body = match frame {
            AccountHistoryFrame::SessionRequest {
                request_id,
                request:
                    super::super::super::account_sync_wire::AccountSessionRequest::Documents { body },
            } => {
                channel
                    .send(
                        &AccountHistoryFrame::SessionReady { request_id }
                            .encode_version(AccountSyncVersion::V2)
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                body
            }
            AccountHistoryFrame::DocumentsRequest { body } => body,
            _ => panic!("expected account documents request"),
        };
        (channel, body)
    }
    async fn access_only_reply(
        fixture: &UpgradeFixture,
        cursor: &mut DocumentSyncCursor,
        wrong_nonce: bool,
        send_operation: bool,
    ) -> (Result<(), NodeError>, serde_json::Value) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = fixture.peer.clone();
        peer.addr = listener.local_addr().unwrap();
        let source = fixture.source.clone();
        let server = tokio::spawn(async move {
            let (mut channel, body) = receive_document_request(&source, listener).await;
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if send_operation {
                let (meta, payload) = {
                    let store = source.account_documents.lock().unwrap();
                    let meta = store
                        .list(AccountDocumentDomain::GroupControl, None, 1)
                        .pop()
                        .unwrap();
                    let payload = store.get(&meta.key).unwrap().unwrap().payload().to_vec();
                    (meta, payload)
                };
                for frame in [
                    AccountHistoryFrame::DocumentHeader {
                        body: serde_json::to_vec(&meta).unwrap(),
                    },
                    AccountHistoryFrame::DocumentChunk { bytes: payload },
                ] {
                    let _ = channel
                        .send(&frame.encode_version(AccountSyncVersion::V2).unwrap())
                        .await;
                }
            }
            let mut complete = serde_json::json!({
                "enabled": true,
                "domain": "group_control",
                "next": null,
                "digest": source.account_documents.lock().unwrap()
                    .digest_scope(AccountDocumentDomain::GroupControl, false),
            });
            if wrong_nonce {
                let mut nonce = [11u8; 32];
                if let Some(echoed) = request.pointer("/operation/nonce") {
                    nonce = serde_json::from_value(echoed.clone()).unwrap();
                    nonce[0] ^= 1;
                }
                complete["operation"] = serde_json::json!({ "version": 1, "nonce": nonce });
            }
            let _ = channel
                .send(
                    &AccountHistoryFrame::DocumentsComplete {
                        body: serde_json::to_vec(&complete).unwrap(),
                    }
                    .encode_version(AccountSyncVersion::V2)
                    .unwrap(),
                )
                .await;
            request
        });
        let result = fixture.target.pull_document_exchange(&peer, cursor).await;
        (result, server.await.unwrap())
    }
    fn seed_operation_support(fixture: &UpgradeFixture) {
        fixture
            .target
            .group_operation_versions
            .lock()
            .unwrap()
            .insert(
                fixture.source.user_id(),
                GroupProtocolSupport {
                    public: fixture.source.identity.public(),
                    observed_at: Instant::now(),
                },
            );
    }
    async fn later_operation_probe_result_retires_a_concurrent_positive(close: bool) {
        let mut fixture = UpgradeFixture::new().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = fixture.peer.clone();
        peer.addr = listener.local_addr().unwrap();
        let source = fixture.source.clone();
        let (seen, seen_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut channel, _) = receive_document_request(&source, listener).await;
            seen.send(()).unwrap();
            release_rx.await.unwrap();
            if !close {
                let complete = DocumentComplete {
                    enabled: true,
                    domain: AccountDocumentDomain::GroupControl,
                    next: None,
                    digest: source
                        .account_documents
                        .lock()
                        .unwrap()
                        .digest_scope(AccountDocumentDomain::GroupControl, false),
                    operation: None,
                };
                channel
                    .send(
                        &AccountHistoryFrame::DocumentsComplete {
                            body: serde_json::to_vec(&complete).unwrap(),
                        }
                        .encode_version(AccountSyncVersion::V2)
                        .unwrap(),
                    )
                    .await
                    .unwrap();
            }
        });
        seed_operation_support(&fixture);
        let target = fixture.target.clone();
        let pending = tokio::spawn(async move {
            target
                .pull_document_exchange(&peer, &mut DocumentSyncCursor::default())
                .await
        });
        seen_rx.await.unwrap();
        assert!(
            fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .contains_key(&fixture.source.user_id()),
            "an in-flight refresh is not negative capability evidence and must not erase the prior authenticated operation support"
        );
        fixture.prove_support().await;
        assert!(
            fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .contains_key(&fixture.source.user_id()),
            "the independent positive probe must complete before the older pending request resolves"
        );
        release.send(()).unwrap();
        let result = pending.await.unwrap();
        if close {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        server.await.unwrap();
        assert!(
            !fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .contains_key(&fixture.source.user_id()),
            "the later failed or access-only outcome must retire a preceding concurrent positive"
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn a_later_failed_operation_probe_retires_a_concurrent_positive() {
        later_operation_probe_result_retires_a_concurrent_positive(true).await;
    }
    #[tokio::test]
    async fn a_later_access_only_reply_retires_a_concurrent_positive() {
        later_operation_probe_result_retires_a_concurrent_positive(false).await;
    }
    #[tokio::test]
    async fn access_only_v5_reply_retires_old_operation_support_without_an_upgrade_claim() {
        let mut fixture = UpgradeFixture::new().await;
        seed_operation_support(&fixture);
        let mut cursor = DocumentSyncCursor::default();
        let (result, _) = access_only_reply(&fixture, &mut cursor, false, false).await;
        result.unwrap();
        assert!(
            !fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .contains_key(&fixture.source.user_id()),
            "a successful access-only v5 response is not operation support"
        );
        assert!(!cursor.group_upgrade && !cursor.requires_upgrade);
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn operation_capability_echo_with_another_nonce_is_rejected() {
        let mut fixture = UpgradeFixture::new().await;
        seed_operation_support(&fixture);
        let (result, _) =
            access_only_reply(&fixture, &mut DocumentSyncCursor::default(), true, false).await;
        assert!(
            result.is_err(),
            "another exchange's nonce cannot prove operation support"
        );
        assert!(
            !fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .contains_key(&fixture.source.user_id())
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn operation_continuation_never_enters_the_legacy_after_field() {
        let mut fixture = UpgradeFixture::new().await;
        let mut cursor = DocumentSyncCursor {
            after: Some(AccountDocumentKey::GroupOperation {
                channel: "a".repeat(64),
                source_device: fixture.source.user_id(),
            }),
            ..DocumentSyncCursor::default()
        };
        cursor.digests[AccountDocumentDomain::GroupControl as usize] = [91; 32];
        let (result, request) = access_only_reply(&fixture, &mut cursor, false, false).await;
        result.unwrap();
        assert!(
            request["after"].is_null(),
            "old v5 peers cannot decode operation keys in the legacy continuation"
        );
        assert!(
            request
                .pointer("/operation/after")
                .is_some_and(|after| !after.is_null())
        );
        assert_eq!(
            request["known_digest"],
            serde_json::json!(([91u8; 32])),
            "the legacy access-only digest keeps its original scope"
        );
        assert_eq!(
            request.pointer("/operation/known_digest"),
            Some(&serde_json::json!(([0u8; 32]))),
            "a previous access-only digest cannot suppress the first operation scope fetch"
        );
        fixture.servers.shutdown().await;
    }
    fn install_operation_head(fixture: &UpgradeFixture) -> AccountDocumentKey {
        use crate::{
            eventlog::ConversationId,
            node::account_group::operations::wire::{OperationFrame, OperationProjection},
        };
        let group = ConversationId::new([71; 32]);
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let payload =
            OperationFrame::new(&fixture.source, group, 1, OperationProjection::default()).encode();
        let meta = AccountDocumentMeta::new(
            &fixture.source.account,
            fixture.source.user_id(),
            key.clone(),
            1,
            &payload,
            false,
        );
        fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .merge(meta, &payload)
            .unwrap();
        key
    }
    #[tokio::test]
    async fn optional_cursor_with_no_enabled_content_settles_without_mandatory_work() {
        let mut fixture = UpgradeFixture::new().await;
        fixture
            .target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        fixture
            .target
            .set_device_sync_category(SyncCategory::Appearance, false)
            .unwrap();
        let mut cursor = DocumentSyncCursor::optional();
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(!cursor.pending && cursor.up_to_date);
        assert_eq!(
            cursor.status(fixture.target.device_sync_policy()).0,
            crate::node::SyncPhase::UpToDate
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn authenticated_mandatory_upgrade_preserves_an_optional_page_continuation() {
        let mut fixture = UpgradeFixture::new().await;
        let mut keys = Vec::new();
        for index in 1..=65 {
            let key = AccountDocumentKey::Draft {
                conversation: DocumentConversation::Account(format!("{index:032x}")),
            };
            fixture
                .source
                .put_account_document_if(key.clone(), b"bounded draft".to_vec(), None, |commit| {
                    commit()
                })
                .unwrap();
            keys.push(key);
        }
        let mut cursor = DocumentSyncCursor::optional();
        cursor.controls_unavailable();
        cursor.after = Some(keys[63].clone());
        fixture.prove_support().await;
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(
            fixture.target.account_document_meta(&keys[0]).is_none(),
            "fresh mandatory capability cannot restart an optional sweep"
        );
        assert!(fixture.target.account_document_meta(&keys[64]).is_some());
        assert!(cursor.after.is_none());
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn access_only_v5_request_excludes_operation_rows_and_their_digest() {
        let mut fixture = UpgradeFixture::new().await;
        install_operation_head(&fixture);
        let request = DocumentRequest {
            version: 5,
            account_id: fixture.target.account_id(),
            domain: AccountDocumentDomain::GroupControl,
            after: None,
            known_digest: [0; 32],
            operation: None,
        };
        let mut channel = fixture
            .target
            .begin_account_exchange(
                &fixture.peer,
                AccountSyncVersion::V2,
                AccountHistoryFrame::DocumentsRequest {
                    body: request.encode(),
                },
            )
            .await
            .unwrap();
        let mut records = 0;
        let digest = loop {
            let bytes = channel.recv().await.unwrap();
            match AccountHistoryFrame::decode_versioned(&bytes).unwrap().1 {
                AccountHistoryFrame::DocumentHeader { .. } => records += 1,
                AccountHistoryFrame::DocumentChunk { .. } => {}
                AccountHistoryFrame::DocumentsComplete { body } => {
                    break serde_json::from_slice::<DocumentComplete>(&body)
                        .unwrap()
                        .digest;
                }
                _ => panic!("expected document response"),
            }
        };
        assert_eq!(
            records, 0,
            "a peer without an explicit operation probe receives only access rows"
        );
        assert_eq!(
            digest,
            fixture
                .source
                .account_documents
                .lock()
                .unwrap()
                .digest_scope(AccountDocumentDomain::GroupControl, false)
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn operation_rows_are_not_installed_without_the_matching_completion_capability() {
        let mut fixture = UpgradeFixture::new().await;
        let key = install_operation_head(&fixture);
        let (result, _) =
            access_only_reply(&fixture, &mut DocumentSyncCursor::default(), false, true).await;
        assert!(
            fixture
                .target
                .account_documents
                .lock()
                .unwrap()
                .meta(&key)
                .is_none(),
            "a source cannot install an operation row before authenticating this exchange's capability"
        );
        assert!(result.is_err());
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn a_broken_operation_blob_cannot_starve_a_complete_role_invalidation() {
        use crate::node::AccountGroupRole;
        let mut fixture = UpgradeFixture::new().await;
        let group = fixture
            .source
            .create_channel("Role survives operation damage", vec![])
            .await
            .unwrap();
        fixture.source.publish_account_group_access(&mut None);
        fixture.prove_support().await;
        assert_eq!(
            fixture.target.account_group_access(group).unwrap().role,
            AccountGroupRole::Owner
        );
        fixture.source.dissolve_channel(group).await.unwrap();
        fixture.source.publish_account_group_access(&mut None);
        let operation_key = install_operation_head(&fixture);
        {
            let store = fixture.source.account_documents.lock().unwrap();
            let hash = store.meta(&operation_key).unwrap().content_hash;
            store.remove_blob_for_test(hash);
        }
        assert!(
            fixture
                .target
                .pull_document_exchange(&fixture.peer, &mut DocumentSyncCursor::default())
                .await
                .is_err()
        );
        assert_eq!(
            fixture.target.account_group_access(group).unwrap().role,
            AccountGroupRole::Dissolved,
            "a fully received authenticated role invalidation must survive a later operation blob failure"
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn legacy_access_rows_below_an_operation_continuation_still_converge() {
        let mut fixture = UpgradeFixture::new().await;
        let group = fixture
            .source
            .create_channel("legacy response", vec![])
            .await
            .unwrap();
        fixture.source.publish_account_group_access(&mut None);
        let key = AccountDocumentKey::GroupControl {
            channel: hex::encode(group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        assert!(
            fixture
                .source
                .account_documents
                .lock()
                .unwrap()
                .meta(&key)
                .is_some()
        );
        let mut cursor = DocumentSyncCursor {
            after: Some(AccountDocumentKey::GroupOperation {
                channel: "f".repeat(64),
                source_device: fixture.source.user_id(),
            }),
            ..DocumentSyncCursor::default()
        };
        let (result, _) = access_only_reply(&fixture, &mut cursor, false, true).await;
        assert!(
            result.is_ok(),
            "a legacy responder ignores the optional operation continuation"
        );
        assert!(
            fixture
                .target
                .account_documents
                .lock()
                .unwrap()
                .meta(&key)
                .is_some()
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn operation_probe_rejects_a_peer_with_the_same_ed_key_and_another_transport_key() {
        let mut fixture = UpgradeFixture::new().await;
        let mut peer = fixture.peer.clone();
        peer.public.x25519_pub[0] ^= 1;
        assert!(
            fixture
                .target
                .pull_document_exchange(&peer, &mut DocumentSyncCursor::default())
                .await
                .is_err()
        );
        assert!(
            !fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .contains_key(&fixture.source.user_id())
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn a_first_operation_scope_fetch_imports_actual_signed_operation_metadata() {
        let mut fixture = UpgradeFixture::new().await;
        let key = install_operation_head(&fixture);
        let mut cursor = DocumentSyncCursor::default();
        cursor.digests[AccountDocumentDomain::GroupControl as usize] = fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .digest_scope(AccountDocumentDomain::GroupControl, false);
        fixture
            .target
            .pull_document_exchange(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(
            fixture
                .target
                .account_documents
                .lock()
                .unwrap()
                .meta(&key)
                .is_some()
        );
        fixture.servers.shutdown().await;
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
    fn group_and_hostname_domains_remain_mandatory_with_optional_content_disabled() {
        let mut cursor = DocumentSyncCursor::default();
        let policy = crate::node::DeviceSyncPolicy {
            history: false,
            appearance: false,
            attachments: false,
            contacts: false,
            profile: false,
        };
        assert_eq!(cursor.domain, AccountDocumentDomain::GroupControl as usize);
        assert_eq!(cursor.digests.len(), 10);
        assert_eq!(
            DocumentSyncScope::Mandatory.mask(),
            (1 << AccountDocumentDomain::GroupInventory as usize)
                | (1 << AccountDocumentDomain::DeviceHostname as usize)
                | (1 << AccountDocumentDomain::GroupControl as usize)
        );
        assert!(document_enabled(
            AccountDocumentDomain::DeviceHostname,
            policy
        ));
        assert!(document_enabled(
            AccountDocumentDomain::GroupControl,
            policy
        ));
        cursor.remotely_disabled = 1 << AccountDocumentDomain::GroupControl as usize;
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
        cursor.domain = AccountDocumentDomain::GroupControl as usize;
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
