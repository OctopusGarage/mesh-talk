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
/// Operation support is action-specific; old v1 peers implement only the
/// original three controls even though they authenticate the same probe nonce.
pub(in crate::node) struct GroupOperationSupport {
    pub public: crate::identity::device::PublicIdentity,
    pub observed_at: Instant,
    pub kinds: u16,
}
fn local_operation_capabilities() -> u16 {
    use crate::node::AccountGroupOperationKind::*;
    [
        Text,
        Sticker,
        File,
        Reaction,
        Recall,
        Invite,
        AcceptInvitation,
        DeclineInvitation,
        Rename,
        RemoveAccount,
        TransferOwnership,
        Dissolve,
        HistoryPolicy,
        LeaveAccount,
    ]
    .into_iter()
    .filter(|kind| kind.supported_here())
    .fold(0, |mask, kind| mask | kind.capability_bit())
}
#[cfg(test)]
pub(in crate::node) fn seed_legacy_control_page_for_test(
    node: &Node,
    group: crate::eventlog::ConversationId,
) -> AccountDocumentKey {
    seed_legacy_control_page_revision_for_test(node, group, 1)
}

#[cfg(test)]
pub(in crate::node) fn seed_legacy_control_page_revision_for_test(
    node: &Node,
    group: crate::eventlog::ConversationId,
    revision: u64,
) -> AccountDocumentKey {
    use crate::node::account_group::operations::legacy_wire::{
        LegacyOperationFrame, LegacyOperationProjection,
    };
    let key = AccountDocumentKey::GroupLegacyOperation {
        channel: hex::encode(group.as_bytes()),
        source_device: node.user_id(),
    };
    let bytes =
        LegacyOperationFrame::new(node, group, revision, LegacyOperationProjection::default())
            .encode();
    let meta = AccountDocumentMeta::new(
        &node.account,
        node.user_id(),
        key.clone(),
        revision,
        &bytes,
        false,
    );
    node.account_documents
        .lock()
        .unwrap()
        .merge(meta, &bytes)
        .unwrap();
    key
}

#[cfg(test)]
pub(in crate::node) fn publish_legacy_control_page_for_test(
    node: &Node,
    count: u8,
) -> Vec<AccountDocumentKey> {
    let keys = (1..=count)
        .map(|value| {
            seed_legacy_control_page_for_test(
                node,
                crate::eventlog::ConversationId::new([value; 32]),
            )
        })
        .collect();
    node.documents_changed();
    keys
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
enum DocumentSyncScope {
    #[default]
    All,
    Mandatory,
    Control,
    Directory,
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
            Self::Control => 1 << AccountDocumentDomain::GroupControl as usize,
            Self::Directory => Self::Mandatory.mask() & !Self::Control.mask(),
            Self::Optional => Self::All.mask() & !Self::Mandatory.mask(),
        }
    }
}
#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(in crate::node) struct DocumentSyncCursor {
    scope: DocumentSyncScope,
    pub(super) domain: usize,
    pub(super) after: Option<AccountDocumentKey>,
    pub(super) digests: [[u8; 32]; 12],
    // The legacy digest remains access-only, even when the peer ignores the
    // optional operation probe. Its digest and continuation have a separate scope.
    operation_digest: [u8; 32],
    personal_supported: bool,
    personal_upgrade: bool,
    personal_required: bool,
    pub(in crate::node) retry_at: Option<Instant>,
    admission_deferred: bool,
    page_completed: bool,
    last_failure_at: Option<Instant>,
    // A failed initial Control negotiation owns no continuation or page bytes.
    // A newer authenticated wake received during or after that failed attempt
    // may retire this cooldown once; an earlier token cannot renew it.
    failed_control_negotiation_started_at: Option<Instant>,
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
    presentation_retry_at: Option<Instant>,
}
impl Default for DocumentSyncCursor {
    fn default() -> Self {
        Self {
            scope: DocumentSyncScope::All,
            domain: AccountDocumentDomain::GroupControl as usize,
            after: None,
            digests: [[0; 32]; 12],
            operation_digest: [0; 32],
            personal_supported: false,
            personal_upgrade: false,
            personal_required: false,
            retry_at: None,
            admission_deferred: false,
            page_completed: false,
            last_failure_at: None,
            failed_control_negotiation_started_at: None,
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
            presentation_retry_at: None,
        }
    }
}
impl DocumentSyncCursor {
    #[cfg(test)]
    pub(in crate::node) fn control_test_page(after: Option<AccountDocumentKey>) -> Self {
        let mut cursor = Self::control();
        cursor.visited = cursor.scope.mask();
        cursor.pending = after.is_some();
        cursor.up_to_date = after.is_none();
        cursor.after = after;
        cursor.page_completed = true;
        cursor
    }

    #[cfg(test)]
    pub(in crate::node) fn control_test_timeout(failed_at: Instant) -> Self {
        Self::control_test_timeout_window(failed_at, failed_at)
    }

    #[cfg(test)]
    pub(in crate::node) fn control_test_timeout_window(
        started_at: Instant,
        failed_at: Instant,
    ) -> Self {
        Self {
            failed_control_negotiation_started_at: Some(started_at),
            last_failure_at: Some(failed_at),
            error: Some("account document exchange interrupted".into()),
            error_domain: Some(AccountDocumentDomain::GroupControl),
            retry_at: Some(failed_at + Duration::from_secs(5)),
            ..Self::control()
        }
    }

    #[cfg(test)]
    fn client_trace_state(&self) -> String {
        format!(
            "domain={:?} pending={} after={} deferred={} retry_ms={}",
            DOMAINS[self.domain],
            self.pending,
            self.after.is_some(),
            self.admission_deferred,
            self.retry_at.map_or(0, |retry| retry
                .saturating_duration_since(Instant::now())
                .as_millis())
        )
    }

    /// Start one new comparison sweep after an external wake. A partial page or
    /// cycle must finish first; its owner retains one coalesced follow-up wake.
    pub(in crate::node) fn begin_refresh(&mut self) -> bool {
        if self.pending || self.after.is_some() || self.admission_deferred {
            return false;
        }
        self.visited = 0;
        self.settled = 0;
        self.up_to_date = false;
        true
    }

    pub(in crate::node) fn refresh_timed_out_control(&mut self, wake_received_at: Instant) -> bool {
        if self.scope != DocumentSyncScope::Control
            || self.domain != AccountDocumentDomain::GroupControl as usize
            || self.after.is_some()
            || self.pending
            || self.admission_deferred
            || self.group_retry_at.is_some()
            || self.error_domain != Some(AccountDocumentDomain::GroupControl)
            || self
                .failed_control_negotiation_started_at
                .is_none_or(|started| wake_received_at <= started)
        {
            return false;
        }
        self.failed_control_negotiation_started_at = None;
        self.retry_at = None;
        self.error = None;
        self.error_domain = None;
        self.page_completed = false;
        self.begin_refresh()
    }

    /// Fork only the new comparison; the controller moves the original into
    /// its bounded parked owner without changing any original checkpoint field.
    pub(in crate::node) fn fresh_control_comparison(&self) -> Self {
        debug_assert!(self.scope == DocumentSyncScope::Control);
        let mut fresh = self.clone();
        fresh.after = None;
        fresh.visited = 0;
        fresh.settled = 0;
        fresh.pending = false;
        fresh.up_to_date = false;
        fresh.page_completed = false;
        fresh
    }
    pub(in crate::node) fn control_page_completed(&self) -> bool {
        self.page_completed
    }
    pub(in crate::node) fn control_comparison_complete(&self) -> bool {
        debug_assert!(self.scope == DocumentSyncScope::Control);
        self.after.is_none()
            && !self.pending
            && !self.admission_deferred
            && self.error.is_none()
            && self.visited & self.scope.mask() == self.scope.mask()
    }
    pub(in crate::node) fn observe_group_protocol(&mut self, supported_at: Option<Instant>) {
        if (self.group_upgrade
            || self.inventory_upgrade
            || self.custody_upgrade
            || self.control_upgrade
            || self.requires_upgrade)
            && self
                .last_failure_at
                .zip(supported_at)
                .is_some_and(|(failed, supported)| supported > failed)
        {
            self.group_supported();
            if self.scope != DocumentSyncScope::Optional {
                self.domain = if self.scope == DocumentSyncScope::Directory {
                    AccountDocumentDomain::GroupInventory as usize
                } else {
                    AccountDocumentDomain::GroupControl as usize
                };
                self.after = None;
            }
        }
    }

    pub(in crate::node) fn control_retry_deadline(&self) -> Option<Instant> {
        self.retry_at.into_iter().chain(self.group_retry_at).max()
    }

    pub(in crate::node) fn admission_deferred(&self) -> bool {
        self.admission_deferred
    }

    #[cfg(test)]
    pub(in crate::node) fn mandatory() -> Self {
        Self {
            scope: DocumentSyncScope::Mandatory,
            ..Self::default()
        }
    }
    pub(in crate::node) fn control() -> Self {
        Self {
            scope: DocumentSyncScope::Control,
            ..Self::default()
        }
    }
    pub(in crate::node) fn directory() -> Self {
        Self {
            scope: DocumentSyncScope::Directory,
            domain: AccountDocumentDomain::GroupInventory as usize,
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
    fn presentation_unavailable(&mut self) {
        self.presentation_retry_at = Some(Instant::now() + Duration::from_secs(300));
        self.after = None;
        self.domain = 0;
        self.pending = true;
        self.up_to_date = false;
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
        let upgrade = (self.personal_required
            && self.personal_upgrade
            && self.scope.mask() & (1 << AccountDocumentDomain::GroupControl as usize) != 0)
            || self.requires_upgrade
                && ((policy.history || policy.appearance) && !self.control_upgrade
                    || policy.history && self.control_upgrade)
            || self.custody_upgrade && policy.history && self.scope.mask() & 64 != 0
            || self.inventory_upgrade && self.scope.mask() & 128 != 0
            || self.group_upgrade
                && self.scope.mask() & (1 << AccountDocumentDomain::GroupControl as usize) != 0;
        if let Some(error) = &self.error
            && (policy.history
                || policy.appearance
                || self
                    .error_domain
                    .is_some_and(|domain| document_enabled(domain, policy))
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

const DOMAINS: [AccountDocumentDomain; 12] = [
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
    AccountDocumentDomain::PublicProfile,
    AccountDocumentDomain::GroupActivity,
];
fn category(domain: AccountDocumentDomain) -> SyncCategory {
    match domain {
        AccountDocumentDomain::PublicProfile => SyncCategory::Profile,
        AccountDocumentDomain::Draft
        | AccountDocumentDomain::ReadReceipt
        | AccountDocumentDomain::DmReaction
        | AccountDocumentDomain::DmRecall
        | AccountDocumentDomain::ChannelCustody
        | AccountDocumentDomain::GroupInventory
        | AccountDocumentDomain::GroupControl
        | AccountDocumentDomain::GroupActivity => SyncCategory::History,
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
        if let Some(asset) = request
            .operation
            .as_ref()
            .filter(|probe| probe.supported())
            .and_then(|probe| probe.asset.as_ref())
        {
            return self.serve_group_operation_asset(channel, asset).await;
        }
        let (policy, revision) = self.device_sync_policy_snapshot();
        let personal_cut = self.capture_personal_history_cut(&self.identity.public());
        let enabled = document_enabled(request.domain, policy);
        let domain = request.domain;
        let known_roles = request.known_digest;
        let operation = request
            .operation
            .filter(|probe| domain == AccountDocumentDomain::GroupControl && probe.supported());
        let include_operations = operation.is_some();
        let include_legacy = operation.as_ref().is_some_and(|probe| probe.legacy_leave);
        let include_personal =
            request.version == 7 && operation.as_ref().is_some_and(|probe| probe.personal);
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
            supported_kinds: Some(local_operation_capabilities()),
            legacy_leave: probe.legacy_leave,
            personal: probe.personal,
        });
        let Ok((items, digest)) = self
            .account_blocking(move |node| {
                let store = node.account_documents.lock().expect("documents mutex");
                let digest = if enabled {
                    store.digest_exchange_scope(
                        domain,
                        include_operations,
                        include_legacy,
                        include_personal,
                    )
                } else {
                    [0; 32]
                };
                let rows = if !enabled || (after.is_none() && known != [0; 32] && known == digest) {
                    Vec::new()
                } else if include_operations {
                    store.list_operation_patch(
                        after.as_ref(),
                        64,
                        include_legacy,
                        include_personal,
                        known_roles,
                    )
                } else {
                    store.list_exchange_scope(
                        domain,
                        after.as_ref(),
                        64,
                        include_operations,
                        include_legacy,
                        include_personal,
                    )
                };
                Ok((rows, digest))
            })
            .await
        else {
            return false;
        };
        let mut selected: Vec<AccountDocumentMeta> = Vec::new();
        let mut bytes = 0usize;
        let mut next = None;
        for item in items {
            if include_operations
                && matches!(item.key, AccountDocumentKey::GroupLegacyOperation { .. })
                && selected
                    .iter()
                    .any(|meta| matches!(meta.key, AccountDocumentKey::GroupOperation { .. }))
            {
                // Complete the interactive operation prefix before loading a
                // legacy body. The full digest and strictly increasing key
                // continuation still cover every remaining legacy/personal row.
                next = selected.last().map(|meta| meta.key.clone());
                break;
            }
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
            let more = {
                let store = self.account_documents.lock().expect("documents mutex");
                if include_operations {
                    store.list_operation_patch(
                        Some(&key),
                        1,
                        include_legacy,
                        include_personal,
                        known_roles,
                    )
                } else {
                    store.list_exchange_scope(
                        domain,
                        Some(&key),
                        1,
                        include_operations,
                        include_legacy,
                        include_personal,
                    )
                }
            };
            if !more.is_empty() {
                next = Some(key);
            }
        }
        let work = async {
            let mut emitted_bytes = 0usize;
            let mut last_emitted = None;
            for (emitted_rows, selected_meta) in selected.into_iter().enumerate() {
                let key = selected_meta.key;
                let remaining = MAX_PAGE_BYTES.saturating_sub(emitted_bytes);
                let Ok(current) = self
                    .account_blocking(move |node| {
                        let store = node.account_documents.lock().expect("documents mutex");
                        // Publication of another role row must not abort a stable
                        // operation result later in this bounded page. Select the
                        // current signed metadata and its matching bytes together.
                        let meta = store.meta(&key).ok_or_else(failure)?;
                        if meta.content_len as usize > remaining {
                            return Ok(None);
                        }
                        let payload = store.get(&key).map_err(NodeError::Log)?;
                        if !meta.tombstone && payload.is_none() {
                            return Err(failure());
                        }
                        Ok(Some((meta, payload)))
                    })
                    .await
                else {
                    return false;
                };
                let Some((meta, payload)) = current else {
                    // A valid first row always fits the page by its owning key
                    // limit. Never emit an empty page with a non-advancing cursor.
                    if last_emitted.is_none() {
                        return false;
                    }
                    next = last_emitted.clone();
                    break;
                };
                if emitted_rows >= 64 {
                    next = last_emitted.clone();
                    break;
                }
                if let AccountDocumentKey::Draft { conversation }
                | AccountDocumentKey::ReadReceipt { conversation, .. } = &meta.key
                    && !personal_cut.allows(self, conversation)
                {
                    continue;
                }
                emitted_bytes += meta.content_len as usize;
                last_emitted = Some(meta.key.clone());
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
                        if let AccountDocumentKey::Draft { conversation }
                        | AccountDocumentKey::ReadReceipt { conversation, .. } = &meta.key
                            && !personal_cut.allows(self, conversation)
                        {
                            return false;
                        }
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
        cursor.page_completed = false;
        #[cfg(test)]
        let started = {
            let enabled = self.account_client_test_hook.lock().unwrap().is_some();
            enabled.then(|| {
                self.trace_account_client(&peer.public, || {
                    format!("page_begin {}", cursor.client_trace_state())
                });
                Instant::now()
            })
        };
        let result = self.pull_account_documents_page_inner(peer, cursor).await;
        #[cfg(test)]
        if let Some(started) = started {
            if result.is_ok()
                && cursor.page_completed
                && matches!(
                    cursor.scope,
                    DocumentSyncScope::Control | DocumentSyncScope::Directory
                )
                && cursor.status(self.device_sync_policy())
                    == (super::super::SyncPhase::UpToDate, None, false)
            {
                self.trace_account_client(&peer.public, || {
                    format!("document_scope_settled scope={:?}", cursor.scope)
                });
            }
            self.trace_account_client(&peer.public, || {
                format!(
                    "page_end elapsed_ms={} outcome={} {}",
                    started.elapsed().as_millis(),
                    super::super::account_session::client_trace_outcome(&result),
                    cursor.client_trace_state()
                )
            });
        }
        result
    }

    async fn pull_account_documents_page_inner(
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
        cursor.observe_group_protocol(supported_at);
        if cursor.retry_at.is_some_and(|at| Instant::now() < at) {
            return Ok(());
        }
        cursor.admission_deferred = false;
        cursor.failed_control_negotiation_started_at = None;
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
        if cursor
            .presentation_retry_at
            .is_some_and(|at| at <= Instant::now())
        {
            cursor.presentation_retry_at = None;
        }
        let policy = self.device_sync_policy();
        let mut eligible = false;
        for _ in 0..DOMAINS.len() {
            if cursor.scope.mask() & (1 << cursor.domain) == 0 {
                cursor.domain = (cursor.domain + 1) % DOMAINS.len();
                cursor.after = None;
                continue;
            }
            if (cursor.domain >= 10 && cursor.presentation_retry_at.is_some())
                || ((4..6).contains(&cursor.domain) && cursor.control_retry_at.is_some())
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
        let operation_digest = cursor.operation_digest;
        let had_continuation = cursor.after.is_some();
        let attempted_at = Instant::now();
        let result = self.pull_document_exchange(peer, cursor).await;
        if result
            .as_ref()
            .is_err_and(super::super::account_sync::local_sync_capacity_busy)
        {
            // No bytes were exchanged: preserve the page, digest, and verified
            // peer capabilities. Release the worker and retry on a later tick.
            cursor.pending = false;
            cursor.admission_deferred = true;
            cursor.operation_digest = operation_digest;
            cursor.up_to_date = false;
            cursor.retry_at = Some(Instant::now() + Duration::from_secs(5));
            return result;
        }
        if result.is_err() {
            let failed_at = Instant::now();
            cursor.last_failure_at = Some(failed_at);
            if cursor.scope == DocumentSyncScope::Control
                && attempted_domain == AccountDocumentDomain::GroupControl
                && !had_continuation
                && result
                    .as_ref()
                    .is_err_and(super::super::account_session::account_dial_timed_out)
            {
                cursor.failed_control_negotiation_started_at = Some(attempted_at);
            }
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
                AccountDocumentDomain::PublicProfile | AccountDocumentDomain::GroupActivity => {
                    cursor.presentation_retry_at.is_some()
                }
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
        if operation_probe
            && result
                .as_ref()
                .is_err_and(|error| !super::super::account_sync::local_sync_capacity_busy(error))
        {
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
        let personal_cut = self.capture_personal_history_cut(&peer.public);
        cursor.personal_required = !self
            .account_documents
            .lock()
            .expect("documents mutex")
            .personal
            .is_empty();
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
                asset: None,
                version: 1,
                nonce,
                after: match &cursor.after {
                    Some(AccountDocumentKey::GroupOperation {
                        channel,
                        source_device,
                    }) => Some(OperationContinuation {
                        channel: channel.clone(),
                        source_device: source_device.clone(),
                        legacy_leave: false,
                    }),
                    Some(AccountDocumentKey::GroupLegacyOperation {
                        channel,
                        source_device,
                    }) => Some(OperationContinuation {
                        channel: channel.clone(),
                        source_device: source_device.clone(),
                        legacy_leave: true,
                    }),
                    _ => None,
                },
                known_digest: std::mem::take(&mut cursor.operation_digest),
                legacy_leave: true,
                personal: true,
            })
        } else {
            None
        };
        let request = DocumentRequest {
            version: match domain {
                AccountDocumentDomain::PublicProfile | AccountDocumentDomain::GroupActivity => 6,
                AccountDocumentDomain::DeviceHostname => 5,
                AccountDocumentDomain::GroupInventory => 4,
                AccountDocumentDomain::GroupControl => {
                    if cursor.personal_supported {
                        7
                    } else {
                        5
                    }
                }
                AccountDocumentDomain::ChannelCustody => 3,
                AccountDocumentDomain::DmReaction | AccountDocumentDomain::DmRecall => 2,
                _ => 1,
            },
            account_id: self.account_id(),
            domain,
            after: cursor.after.clone().filter(|key| {
                !matches!(
                    key,
                    AccountDocumentKey::GroupOperation { .. }
                        | AccountDocumentKey::GroupLegacyOperation { .. }
                )
            }),
            known_digest: cursor.digests[cursor.domain],
            operation,
        };
        #[cfg(test)]
        if domain == AccountDocumentDomain::GroupControl {
            self.trace_account_client(&peer.public, || {
                format!("control_request_body={}", hex::encode(request.encode()))
            });
        }
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
                if request.version == 7 {
                    cursor.personal_supported = false;
                }
                if request.version >= 2
                    && matches!(&error,NodeError::Session(super::super::session::SessionError::Transport(TransportError::Io(e))) if matches!(e.kind(),std::io::ErrorKind::UnexpectedEof|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::BrokenPipe))
                {
                    if request.version == 6 {
                        cursor.presentation_unavailable();
                    } else if domain == AccountDocumentDomain::GroupControl {
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
                    if request.version == 7 {
                        cursor.personal_supported = false;
                    }
                    let pre_reply_close = matches!(&error,TransportError::Io(e) if matches!(e.kind(),std::io::ErrorKind::UnexpectedEof|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::BrokenPipe));
                    if request.version >= 2 && pre_reply_close {
                        if request.version == 6 {
                            if records == 0 {
                                cursor.presentation_unavailable();
                            }
                        } else if domain == AccountDocumentDomain::GroupControl {
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
                                    self.commit_imported_document_at_cut(
                                        meta,
                                        std::mem::take(&mut *payload),
                                        revision,
                                        peer.public.clone(),
                                        personal_cut.clone(),
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
                            self.commit_imported_document_at_cut(
                                meta,
                                std::mem::take(&mut *payload),
                                revision,
                                peer.public.clone(),
                                personal_cut.clone(),
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
                        self.commit_imported_document_at_cut(
                            meta,
                            std::mem::take(&mut *payload),
                            revision,
                            peer.public.clone(),
                            personal_cut.clone(),
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
                            self.commit_imported_document_at_cut(
                                meta,
                                std::mem::take(&mut *payload),
                                revision,
                                peer.public.clone(),
                                personal_cut.clone(),
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
                    let personal_capability = complete
                        .operation
                        .as_ref()
                        .is_some_and(|capability| capability.personal);
                    if request.version == 7 && (!include_operations || !personal_capability) {
                        return Err(failure());
                    }
                    let include_personal = request.version == 7 && personal_capability;
                    let promote_personal = domain == AccountDocumentDomain::GroupControl
                        && request.version == 5
                        && include_operations
                        && personal_capability;
                    if domain == AccountDocumentDomain::GroupControl {
                        cursor.personal_supported = include_operations && personal_capability;
                        cursor.personal_upgrade = !cursor.personal_supported;
                    }
                    let include_legacy = include_operations
                        && request
                            .operation
                            .as_ref()
                            .is_some_and(|probe| probe.legacy_leave)
                        && complete
                            .operation
                            .as_ref()
                            .is_some_and(|capability| capability.legacy_leave);
                    if complete
                        .operation
                        .as_ref()
                        .is_some_and(|capability| capability.legacy_leave)
                        && !request
                            .operation
                            .as_ref()
                            .is_some_and(|probe| probe.legacy_leave)
                    {
                        return Err(failure());
                    }
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
                                (!include_personal
                                    && matches!(
                                        meta.key,
                                        AccountDocumentKey::ConversationPersonal { .. }
                                    ))
                                    || (!include_operations
                                        && matches!(
                                            meta.key,
                                            AccountDocumentKey::GroupOperation { .. }
                                        ))
                                    || (!include_legacy
                                        && matches!(
                                            meta.key,
                                            AccountDocumentKey::GroupLegacyOperation { .. }
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
                            self.commit_imported_document_at_cut(
                                meta,
                                std::mem::take(&mut *payload),
                                revision,
                                peer.public.clone(),
                                personal_cut.clone(),
                            )
                            .await?;
                        }
                    }
                    let local = self
                        .account_documents
                        .lock()
                        .expect("documents mutex")
                        .digest_exchange_scope(
                            domain,
                            include_operations,
                            include_legacy,
                            include_personal,
                        );
                    cursor.page_completed = true;
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
                                    GroupOperationSupport {
                                        public: peer.public.clone(),
                                        observed_at: Instant::now(),
                                        kinds: complete
                                            .operation
                                            .as_ref()
                                            .expect("matched operation capability")
                                            .capabilities(),
                                    },
                                );
                        }
                    }
                    if promote_personal {
                        cursor.visited &= !(1 << AccountDocumentDomain::GroupControl as usize);
                        cursor.settled &= !(1 << AccountDocumentDomain::GroupControl as usize);
                        cursor.domain = AccountDocumentDomain::GroupControl as usize;
                        cursor.after = None;
                        cursor.operation_digest = [0; 32];
                        cursor.digests[cursor.domain] = [0; 32];
                    }
                    let mut supported_mask = cursor.scope.mask();
                    if cursor.presentation_retry_at.is_some() {
                        supported_mask &= !((1 << AccountDocumentDomain::PublicProfile as usize)
                            | (1 << AccountDocumentDomain::GroupActivity as usize));
                    }
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
                        && (!cursor.personal_required || !cursor.personal_upgrade)
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
    #[cfg(test)]
    pub(in crate::node) async fn commit_imported_document_for_test(
        self: &Arc<Self>,
        meta: AccountDocumentMeta,
        payload: Vec<u8>,
        revision: u64,
        peer: crate::identity::device::PublicIdentity,
    ) -> Result<(), NodeError> {
        self.commit_imported_document(meta, payload, revision, peer)
            .await
    }
    #[cfg(test)]
    async fn commit_imported_document(
        self: &Arc<Self>,
        meta: AccountDocumentMeta,
        payload: Vec<u8>,
        revision: u64,
        peer: crate::identity::device::PublicIdentity,
    ) -> Result<(), NodeError> {
        let cut = self.capture_personal_history_cut(&peer);
        self.commit_imported_document_at_cut(meta, payload, revision, peer, cut)
            .await
    }
    async fn commit_imported_document_at_cut(
        self: &Arc<Self>,
        meta: AccountDocumentMeta,
        payload: Vec<u8>,
        revision: u64,
        peer: crate::identity::device::PublicIdentity,
        personal_cut: personal::PersonalHistoryCut,
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
            if let AccountDocumentKey::Draft { conversation }
            | AccountDocumentKey::ReadReceipt { conversation, .. } = &meta.key
                && !personal_cut.allows(&node, conversation)
            {
                return Ok(());
            }
            let public_profile = meta.key.domain() == AccountDocumentDomain::PublicProfile;
            let private_control = meta.key.is_private_control();
            let changed = node
                .account_documents
                .lock()
                .expect("documents mutex")
                .merge(meta, &payload)
                .map_err(NodeError::Log)?;
            if public_profile {
                let profile = crate::node::AccountProfile::decode(&payload).ok_or_else(failure)?;
                if profile.account_id() != node.account_id() {
                    node.receive_account_profile(&profile)?;
                }
            }
            if changed {
                node.documents_imported();
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
    fn mandatory_lanes_partition_domains_and_preserve_partial_refreshes() {
        let mut control = DocumentSyncCursor::control();
        let mut directory = DocumentSyncCursor::directory();
        assert_eq!(control.scope.mask() & directory.scope.mask(), 0);
        assert_eq!(
            control.scope.mask() | directory.scope.mask(),
            DocumentSyncCursor::mandatory().scope.mask()
        );
        assert_eq!(DOMAINS[control.domain], AccountDocumentDomain::GroupControl);
        assert_eq!(
            DOMAINS[directory.domain],
            AccountDocumentDomain::GroupInventory
        );
        control.pending = true;
        directory.pending = true;
        directory.digests[AccountDocumentDomain::GroupInventory as usize] = [7; 32];
        assert!(!control.begin_refresh());
        assert!(!directory.begin_refresh());
        assert_eq!(
            directory.digests[AccountDocumentDomain::GroupInventory as usize],
            [7; 32]
        );
    }

    #[test]
    fn foreground_fork_keeps_original_partial_checkpoint_and_failure_knowledge() {
        let after = AccountDocumentKey::GroupOperation {
            channel: "01".repeat(32),
            source_device: "02".repeat(16),
        };
        let mut original = DocumentSyncCursor::control_test_page(Some(after.clone()));
        original.digests[8] = [7; 32];
        original.operation_digest = [8; 32];
        original.retry_at = Some(Instant::now() + Duration::from_secs(5));
        original.group_retry_at = Some(Instant::now() + Duration::from_secs(10));
        original.group_upgrade = true;
        let saved = original.clone();
        let fresh = original.fresh_control_comparison();
        assert_eq!(
            original, saved,
            "forking must not mutate the saved checkpoint"
        );
        assert!(fresh.after.is_none() && !fresh.control_comparison_complete());
        assert_eq!(fresh.digests, original.digests);
        assert_eq!(fresh.operation_digest, original.operation_digest);
        assert_eq!(
            fresh.control_retry_deadline(),
            original.control_retry_deadline()
        );
        assert_eq!(fresh.group_upgrade, original.group_upgrade);
        let mut complete = DocumentSyncCursor::control_test_page(None);
        assert!(complete.control_comparison_complete());
        complete.admission_deferred = true;
        assert!(!complete.control_comparison_complete());
        complete.admission_deferred = false;
        complete.error = Some("test transport failure".into());
        assert!(!complete.control_comparison_complete());
    }

    #[test]
    fn refresh_preserves_partial_pages_and_cycles_and_retains_peer_hints() {
        let mut cursor = DocumentSyncCursor::mandatory();
        cursor.domain = AccountDocumentDomain::GroupInventory as usize;
        cursor.visited = 1 << AccountDocumentDomain::GroupControl as usize;
        cursor.settled = cursor.visited;
        cursor.pending = true;
        cursor.digests[cursor.domain] = [7; 32];
        cursor.operation_digest = [8; 32];
        cursor.group_retry_at = Some(Instant::now() + Duration::from_secs(3));
        let retry = cursor.group_retry_at;
        let visited = cursor.visited;
        assert!(!cursor.begin_refresh());
        assert_eq!(
            cursor.visited, visited,
            "a partial cycle retains its completed domains"
        );
        cursor.pending = false;
        cursor.after = Some(AccountDocumentKey::DeviceHostname {
            device_id: "a".repeat(32),
        });
        assert!(!cursor.begin_refresh());
        assert!(
            cursor.after.is_some(),
            "a partial page retains its continuation key"
        );
        cursor.after = None;
        cursor.up_to_date = true;
        assert!(cursor.begin_refresh());
        assert_eq!(cursor.visited, 0);
        assert_eq!(cursor.settled, 0);
        assert!(!cursor.pending && !cursor.up_to_date);
        assert_eq!(
            cursor.domain,
            AccountDocumentDomain::GroupInventory as usize
        );
        assert_eq!(cursor.digests[cursor.domain], [7; 32]);
        assert_eq!(cursor.operation_digest, [8; 32]);
        assert_eq!(cursor.group_retry_at, retry);
    }

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
    #[test]
    fn timed_out_control_refresh_requires_newer_wake_and_preserves_partial_failures() {
        let failed = Instant::now();
        let original = DocumentSyncCursor::control_test_timeout(failed);
        let fresh = failed + Duration::from_millis(1);
        let mut cursor = original.clone();
        for _ in 0..64 {
            assert!(!cursor.refresh_timed_out_control(failed));
            assert_eq!(
                cursor, original,
                "old wake/polling cannot retire the cooldown"
            );
        }
        assert!(cursor.refresh_timed_out_control(fresh));
        assert!(cursor.retry_at.is_none() && cursor.error.is_none());
        assert!(
            !cursor.refresh_timed_out_control(fresh),
            "one timeout owns at most one recovery"
        );
        for case in 0..7 {
            let mut protected = original.clone();
            match case {
                0 => {
                    protected.after = Some(AccountDocumentKey::GroupControl {
                        channel: "1".repeat(64),
                        source_device: "2".repeat(32),
                    })
                }
                1 => protected.pending = true,
                2 => protected.admission_deferred = true,
                3 => protected.group_retry_at = Some(failed + Duration::from_secs(300)),
                4 => protected.failed_control_negotiation_started_at = None,
                5 => protected.scope = DocumentSyncScope::Directory,
                _ => protected.error_domain = Some(AccountDocumentDomain::GroupInventory),
            }
            let before = protected.clone();
            assert!(
                !protected.refresh_timed_out_control(fresh),
                "protected failure case {case}"
            );
            assert_eq!(
                protected, before,
                "partial/upgrade/admission/authentication failures retain their exact checkpoint"
            );
        }
    }

    #[tokio::test]
    async fn queued_operation_probe_retains_its_verified_digest() {
        let mut fixture = UpgradeFixture::new().await;
        seed_operation_support(&fixture);
        let held = fixture
            .target
            .sessions
            .authentication_admission(
                fixture
                    .target
                    .peer_endpoint_key(fixture.peer.addr, &fixture.peer.public),
            )
            .await
            .unwrap();
        let mut cursor = DocumentSyncCursor::for_domain(AccountDocumentDomain::GroupControl);
        cursor.operation_digest = [9; 32];
        let error = fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap_err();
        assert!(super::super::super::account_sync::local_sync_capacity_busy(
            &error
        ));
        assert_eq!(cursor.operation_digest, [9; 32]);
        assert!(!cursor.begin_refresh());
        drop(held);
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn fresh_refresh_during_backoff_does_not_create_a_no_io_continuation() {
        let mut fixture = UpgradeFixture::new().await;
        let mut cursor = DocumentSyncCursor::mandatory();
        cursor.visited = cursor.scope.mask();
        cursor.settled = cursor.visited;
        cursor.retry_at = Some(Instant::now() + Duration::from_secs(5));
        let retry = cursor.retry_at;
        assert!(cursor.begin_refresh());
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(
            !cursor.pending,
            "a backoff-only turn must release its worker instead of self-requeuing"
        );
        assert_eq!(cursor.retry_at, retry);
        assert_eq!(cursor.visited, 0);
        fixture.servers.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn coalesced_refresh_wakes_cannot_restart_a_partial_mandatory_sweep() {
        let mut fixture = UpgradeFixture::new().await;
        let mut cursor = DocumentSyncCursor::mandatory();
        // The first authenticated v5 response negotiates the personal v7 scope;
        // its extra GroupControl comparison must complete before it can settle.
        // Later sweeps already know that scope and visit the three domains once.
        for expected_pages in [4, 3] {
            assert!(cursor.begin_refresh());
            let mut pages = 0;
            loop {
                fixture
                    .target
                    .pull_account_documents_page(&fixture.peer, &mut cursor)
                    .await
                    .unwrap();
                pages += 1;
                if !cursor.pending {
                    break;
                }
                let visited = cursor.visited;
                let domain = cursor.domain;
                // Many external notifications coalesce behind the same partial
                // sweep; they cannot move it back to its first domain.
                for _ in 0..8 {
                    assert!(!cursor.begin_refresh());
                }
                assert_eq!(cursor.visited, visited);
                assert_eq!(cursor.domain, domain);
                assert!(
                    pages < expected_pages,
                    "mandatory domains and the initial negotiated scope must make bounded progress"
                );
            }
            assert_eq!(cursor.visited & cursor.scope.mask(), cursor.scope.mask());
            assert_eq!(pages, expected_pages);
        }
        fixture.servers.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn settled_mandatory_refresh_revisits_committed_results_after_a_precommit_page() {
        let mut fixture = UpgradeFixture::new().await;
        let group = fixture
            .source
            .create_channel("refresh result", vec![])
            .await
            .unwrap();
        fixture.source.publish_account_group_access_for(group);
        fixture.source.publish_account_group_operations_for(group);
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let mut cursor = DocumentSyncCursor::mandatory();
        for _ in 0..16 {
            fixture
                .target
                .pull_account_documents_page(&fixture.peer, &mut cursor)
                .await
                .unwrap();
            if !cursor.pending {
                break;
            }
        }
        assert!(!cursor.pending && cursor.after.is_none());
        let before = fixture.target.account_document_meta(&key).unwrap();
        // A request publication wake sees the source before its final effect.
        cursor.begin_refresh();
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert_ne!(cursor.domain, AccountDocumentDomain::GroupControl as usize);
        // Finish that refresh before the final publication wake. The old
        // saturated cursor incorrectly declared completion after its first page.
        for _ in 0..16 {
            if !cursor.pending {
                break;
            }
            fixture
                .target
                .pull_account_documents_page(&fixture.peer, &mut cursor)
                .await
                .unwrap();
        }
        assert!(!cursor.pending && cursor.after.is_none());
        let id = fixture
            .source
            .new_account_group_operation_id(&fixture.source.account_id())
            .unwrap();
        fixture
            .source
            .submit_account_group_operation(
                &fixture.source.account_id(),
                id,
                group,
                crate::node::AccountGroupOperation::Rename {
                    name: "committed refresh".into(),
                },
            )
            .unwrap();
        fixture.source.maintain_account_group_operation_effects();
        let committed = fixture.source.account_document_meta(&key).unwrap();
        assert_ne!(before.content_hash, committed.content_hash);
        // A final commit wake must compare every mandatory domain even when
        // its next page is not GroupControl.
        cursor.begin_refresh();
        for _ in 0..16 {
            fixture
                .target
                .pull_account_documents_page(&fixture.peer, &mut cursor)
                .await
                .unwrap();
            if !cursor.pending {
                break;
            }
        }
        assert_eq!(
            fixture
                .target
                .account_document_meta(&key)
                .unwrap()
                .content_hash,
            committed.content_hash,
            "one refresh must import the committed source result without waiting for the periodic tick"
        );
        fixture.servers.shutdown().await;
    }

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
    async fn stable_operation_result_survives_an_unrelated_selected_role_change() {
        use crate::{
            channel::HistoryPolicy,
            node::{
                AccountGroupOperation, AccountGroupOperationPhase, AccountGroupRole, SyncCategory,
            },
        };
        let mut fixture = UpgradeFixture::new().await;
        fixture
            .target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let first = fixture
            .source
            .create_channel("first selected group", vec![])
            .await
            .unwrap();
        let second = fixture
            .source
            .create_channel("second selected group", vec![])
            .await
            .unwrap();
        fixture
            .source
            .maintain_account_channels(
                &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
            )
            .await;
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
        let lower_group = match &first_key {
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
            _ => panic!("expected a real selected group role"),
        };
        let changed_group = if lower_group == first { second } else { first };
        let account_id = fixture.source.account_id();
        let operation_id = fixture
            .source
            .new_account_group_operation_id(&account_id)
            .unwrap();
        fixture
            .source
            .submit_account_group_operation(
                &account_id,
                operation_id,
                lower_group,
                AccountGroupOperation::SetHistoryPolicy {
                    policy: HistoryPolicy::LastTen,
                },
            )
            .unwrap();
        fixture.source.maintain_account_group_operations().await;
        let committed = fixture
            .source
            .account_group_operation_status(&account_id, operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(committed.phase, AccountGroupOperationPhase::Committed);
        assert_eq!(committed.committed_events.len(), 1);
        fixture.source.publish_account_group_access(&mut None);
        fixture
            .source
            .publish_account_group_operations_for(lower_group);
        let operation_key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(lower_group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let original_operation = fixture
            .source
            .account_document_meta(&operation_key)
            .unwrap();
        let selected_second = fixture.source.account_document_meta(&second_key).unwrap();
        let (blocked, blocked_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let blocked = Arc::new(std::sync::Mutex::new(Some(blocked)));
        let release_rx = Arc::new(std::sync::Mutex::new(Some(release_rx)));
        let exact_peer = fixture.target.identity.public();
        *fixture.source.account_session_test_hook.lock().unwrap() =
            Some(Arc::new(move |peer, frame| {
                let blocked = blocked.clone();
                let release_rx = release_rx.clone();
                let first_key = first_key.clone();
                let exact_peer = exact_peer.clone();
                Box::pin(async move {
                    if peer != exact_peer {
                        return;
                    }
                    let AccountHistoryFrame::DocumentHeader { body } = frame else {
                        return;
                    };
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    if meta.key != first_key {
                        return;
                    }
                    let signal = blocked.lock().unwrap().take();
                    if let Some(signal) = signal {
                        signal.send(()).unwrap();
                        let release = release_rx.lock().unwrap().take().unwrap();
                        release.await.unwrap();
                    }
                })
            }));
        let target = fixture.target.clone();
        let peer = fixture.peer.clone();
        let pulling = tokio::spawn(async move {
            target
                .pull_document_exchange(&peer, &mut DocumentSyncCursor::default())
                .await
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
        fixture
            .source
            .publish_account_group_access_for(changed_group);
        let updated = fixture.source.account_document_meta(&second_key).unwrap();
        assert!(updated.revision > selected_second.revision);
        assert_ne!(updated.content_hash, selected_second.content_hash);
        assert_eq!(
            fixture
                .source
                .account_document_meta(&operation_key)
                .unwrap(),
            original_operation,
            "the real unrelated dissolve must leave the original committed operation document unchanged"
        );
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), pulling)
            .await
            .unwrap()
            .unwrap();
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        result.expect("a stable signed operation result must survive an unrelated selected role-row replacement");
        fixture.target.maintain_account_group_operation_effects();
        let imported = fixture
            .target
            .account_group_operation_status(&account_id, operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(imported.phase, AccountGroupOperationPhase::Committed);
        assert_eq!(imported.committed_events, committed.committed_events);
        assert_eq!(
            fixture
                .target
                .account_document_meta(&operation_key)
                .unwrap(),
            original_operation
        );
        // A second, ordinary page learns the newer negative observation. The
        // stable earlier page cannot override this later signed source revision.
        fixture
            .target
            .pull_document_exchange(&fixture.peer, &mut DocumentSyncCursor::default())
            .await
            .unwrap();
        assert_eq!(
            fixture.target.account_document_meta(&second_key).unwrap(),
            updated
        );
        assert_eq!(
            fixture
                .target
                .account_group_access(changed_group)
                .unwrap()
                .role,
            AccountGroupRole::Dissolved
        );
        fixture.servers.shutdown().await;
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
        let (result, cursor) = tokio::time::timeout(Duration::from_secs(5), pulling)
            .await
            .unwrap()
            .unwrap();
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        result.expect("a changed selected role must refresh to its current signed observation");
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
            "the first independently valid role observation remains available"
        );
        assert!(
            fixture
                .target
                .account_documents
                .lock()
                .unwrap()
                .meta(&second_key)
                .is_some_and(|meta| meta == updated),
            "the changed row must refresh to the current negative observation before completion"
        );
        assert_eq!(
            fixture
                .target
                .account_group_access(changed_group)
                .unwrap()
                .role,
            AccountGroupRole::Dissolved
        );
        fixture
            .target
            .pull_document_exchange(&fixture.peer, &mut DocumentSyncCursor::default())
            .await
            .unwrap();
        assert_eq!(
            fixture
                .target
                .account_group_access(changed_group)
                .unwrap()
                .role,
            AccountGroupRole::Dissolved
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn selected_row_growth_truncates_at_the_last_emitted_key() {
        use crate::node::SyncCategory;
        let mut fixture = UpgradeFixture::new().await;
        for node in [&fixture.source, &fixture.target] {
            node.set_device_sync_category(SyncCategory::Appearance, true)
                .unwrap();
        }
        let first = AccountDocumentKey::CustomizationPack {
            pack_id: "a-selected".into(),
        };
        let second = AccountDocumentKey::CustomizationPack {
            pack_id: "b-growing".into(),
        };
        fixture
            .source
            .put_account_document_if(first.clone(), vec![1], None, |commit| commit())
            .unwrap();
        let old = fixture
            .source
            .put_account_document_if(second.clone(), vec![2], None, |commit| commit())
            .unwrap();
        let (blocked, blocked_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let blocked = Arc::new(std::sync::Mutex::new(Some(blocked)));
        let release_rx = Arc::new(std::sync::Mutex::new(Some(release_rx)));
        let first_key = first.clone();
        let exact_peer = fixture.target.identity.public();
        *fixture.source.account_session_test_hook.lock().unwrap() =
            Some(Arc::new(move |peer, frame| {
                let blocked = blocked.clone();
                let release_rx = release_rx.clone();
                let first_key = first_key.clone();
                let exact_peer = exact_peer.clone();
                Box::pin(async move {
                    if peer != exact_peer {
                        return;
                    }
                    let AccountHistoryFrame::DocumentHeader { body } = frame else {
                        return;
                    };
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    if meta.key != first_key {
                        return;
                    }
                    let signal = blocked.lock().unwrap().take();
                    if let Some(signal) = signal {
                        signal.send(()).unwrap();
                        let release = release_rx.lock().unwrap().take().unwrap();
                        release.await.unwrap();
                    }
                })
            }));
        let target = fixture.target.clone();
        let peer = fixture.peer.clone();
        let pulling = tokio::spawn(async move {
            let mut cursor =
                DocumentSyncCursor::for_domain(AccountDocumentDomain::CustomizationPack);
            let result = target.pull_document_exchange(&peer, &mut cursor).await;
            (result, cursor)
        });
        tokio::time::timeout(Duration::from_secs(5), blocked_rx)
            .await
            .unwrap()
            .unwrap();
        let grown = fixture
            .source
            .put_account_document_if(
                second.clone(),
                vec![7; MAX_PAGE_BYTES],
                Some(old.version()),
                |commit| commit(),
            )
            .unwrap();
        release.send(()).unwrap();
        let (result, mut cursor) = tokio::time::timeout(Duration::from_secs(5), pulling)
            .await
            .unwrap()
            .unwrap();
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        result.expect(
            "growth must truncate safely before the refreshed row exceeds the actual page budget",
        );
        assert_eq!(cursor.after, Some(first.clone()));
        assert!(cursor.pending && !cursor.up_to_date);
        assert_eq!(
            fixture
                .target
                .account_document(&first)
                .unwrap()
                .unwrap()
                .payload(),
            &[1]
        );
        assert!(fixture.target.account_document_meta(&second).is_none());
        fixture
            .target
            .pull_document_exchange(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(cursor.after.is_none());
        assert_eq!(
            fixture.target.account_document_meta(&second).unwrap(),
            grown
        );
        let document = fixture.target.account_document(&second).unwrap().unwrap();
        assert_eq!(document.payload().len(), MAX_PAGE_BYTES);
        assert!(document.payload().iter().all(|byte| *byte == 7));
        fixture.servers.shutdown().await;
    }

    #[tokio::test]
    async fn a_full_page_first_row_is_emitted_before_any_continuation() {
        use crate::node::SyncCategory;
        let mut fixture = UpgradeFixture::new().await;
        for node in [&fixture.source, &fixture.target] {
            node.set_device_sync_category(SyncCategory::Appearance, true)
                .unwrap();
        }
        let first = AccountDocumentKey::CustomizationPack {
            pack_id: "a-full-page".into(),
        };
        let second = AccountDocumentKey::CustomizationPack {
            pack_id: "b-next-page".into(),
        };
        fixture
            .source
            .put_account_document_if(first.clone(), vec![9; MAX_PAGE_BYTES], None, |commit| {
                commit()
            })
            .unwrap();
        fixture
            .source
            .put_account_document_if(second.clone(), vec![3], None, |commit| commit())
            .unwrap();
        let mut cursor = DocumentSyncCursor::for_domain(AccountDocumentDomain::CustomizationPack);
        fixture
            .target
            .pull_document_exchange(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            cursor.after,
            Some(first.clone()),
            "a nonempty continuation must name the row actually emitted"
        );
        let document = fixture.target.account_document(&first).unwrap().unwrap();
        assert_eq!(document.payload().len(), MAX_PAGE_BYTES);
        assert!(fixture.target.account_document_meta(&second).is_none());
        fixture
            .target
            .pull_document_exchange(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(cursor.after.is_none());
        assert_eq!(
            fixture
                .target
                .account_document(&second)
                .unwrap()
                .unwrap()
                .payload(),
            &[3]
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn a_selected_row_replaced_by_a_signed_tombstone_refreshes_without_payload() {
        use crate::node::SyncCategory;
        let mut fixture = UpgradeFixture::new().await;
        for node in [&fixture.source, &fixture.target] {
            node.set_device_sync_category(SyncCategory::Appearance, true)
                .unwrap();
        }
        let first = AccountDocumentKey::CustomizationPack {
            pack_id: "a-retained".into(),
        };
        let second = AccountDocumentKey::CustomizationPack {
            pack_id: "b-removed".into(),
        };
        fixture
            .source
            .put_account_document_if(first.clone(), vec![1], None, |commit| commit())
            .unwrap();
        let old = fixture
            .source
            .put_account_document_if(second.clone(), vec![2], None, |commit| commit())
            .unwrap();
        let (blocked, blocked_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let blocked = Arc::new(std::sync::Mutex::new(Some(blocked)));
        let release_rx = Arc::new(std::sync::Mutex::new(Some(release_rx)));
        let first_key = first.clone();
        let exact_peer = fixture.target.identity.public();
        *fixture.source.account_session_test_hook.lock().unwrap() =
            Some(Arc::new(move |peer, frame| {
                let blocked = blocked.clone();
                let release_rx = release_rx.clone();
                let first_key = first_key.clone();
                let exact_peer = exact_peer.clone();
                Box::pin(async move {
                    if peer != exact_peer {
                        return;
                    }
                    let AccountHistoryFrame::DocumentHeader { body } = frame else {
                        return;
                    };
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    if meta.key != first_key {
                        return;
                    }
                    let signal = blocked.lock().unwrap().take();
                    if let Some(signal) = signal {
                        signal.send(()).unwrap();
                        let release = release_rx.lock().unwrap().take().unwrap();
                        release.await.unwrap();
                    }
                })
            }));
        let target = fixture.target.clone();
        let peer = fixture.peer.clone();
        let pulling = tokio::spawn(async move {
            target
                .pull_document_exchange(
                    &peer,
                    &mut DocumentSyncCursor::for_domain(AccountDocumentDomain::CustomizationPack),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), blocked_rx)
            .await
            .unwrap()
            .unwrap();
        let tombstone = fixture
            .source
            .remove_account_document_if(second.clone(), Some(old.version()), |commit| commit())
            .unwrap();
        assert!(tombstone.tombstone);
        assert_eq!(tombstone.content_len, 0);
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), pulling)
            .await
            .unwrap()
            .unwrap();
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        result
            .expect("a selected row removed mid-page must refresh to its current signed tombstone");
        assert_eq!(
            fixture
                .target
                .account_document(&first)
                .unwrap()
                .unwrap()
                .payload(),
            &[1]
        );
        assert_eq!(
            fixture.target.account_document_meta(&second).unwrap(),
            tombstone
        );
        assert!(fixture.target.account_document(&second).unwrap().is_none());
        fixture
            .target
            .pull_document_exchange(
                &fixture.peer,
                &mut DocumentSyncCursor::for_domain(AccountDocumentDomain::CustomizationPack),
            )
            .await
            .unwrap();
        assert_eq!(
            fixture.target.account_document_meta(&second).unwrap(),
            tombstone
        );
        assert!(fixture.target.account_document(&second).unwrap().is_none());
        fixture.servers.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn known_role_digest_does_not_block_a_new_terminal_result_behind_repeated_role_headers() {
        use crate::node::{AccountGroupOperationPhase, AccountGroupTextInput, SyncCategory};
        let mut fixture = UpgradeFixture::new().await;
        for node in [&fixture.source, &fixture.target] {
            node.set_device_sync_category(SyncCategory::History, false)
                .unwrap();
        }
        let group = fixture
            .source
            .create_channel("known role result", vec![])
            .await
            .unwrap();
        fixture
            .source
            .maintain_account_channels(
                &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
            )
            .await;
        fixture.source.publish_account_group_access_for(group);
        fixture.source.publish_account_group_operations_for(group);
        let mut cursor = DocumentSyncCursor::control();
        for _ in 0..16 {
            fixture
                .target
                .pull_account_documents_page(&fixture.peer, &mut cursor)
                .await
                .unwrap();
            if !cursor.pending {
                break;
            }
        }
        assert!(!cursor.pending && cursor.after.is_none());
        let roles = fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .digest_scope(AccountDocumentDomain::GroupControl, false);
        assert_ne!(roles, [0; 32]);
        assert_eq!(
            cursor.digests[AccountDocumentDomain::GroupControl as usize],
            roles
        );
        let account = fixture.source.account_id();
        let id = fixture
            .source
            .new_account_group_operation_id(&account)
            .unwrap();
        fixture
            .source
            .submit_account_group_text_if(
                &account,
                id,
                group,
                AccountGroupTextInput {
                    text: b"new terminal copy".to_vec(),
                    reply_to: None,
                    expires_at_ms: None,
                    protection: None,
                },
                |commit| commit(),
            )
            .unwrap();
        fixture.source.maintain_account_group_operation_effects();
        let status = fixture
            .source
            .account_group_operation_status(&account, id)
            .unwrap()
            .unwrap();
        assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
        assert_eq!(status.committed_events.len(), 1);
        fixture.source.publish_account_group_operations_for(group);
        assert_eq!(
            fixture
                .source
                .account_documents
                .lock()
                .unwrap()
                .digest_scope(AccountDocumentDomain::GroupControl, false),
            roles,
            "a real message result must not invent a different role observation"
        );
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let expected = fixture.source.account_document_meta(&key).unwrap();
        assert_ne!(
            fixture.target.account_document_meta(&key).as_ref(),
            Some(&expected)
        );
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let repeated_roles = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let exact = fixture.target.identity.public();
        *fixture.source.account_session_test_hook.lock().unwrap() = Some(Arc::new({
            let release = release.clone();
            let repeated_roles = repeated_roles.clone();
            move |peer, frame| {
                let release = release.clone();
                let repeated_roles = repeated_roles.clone();
                let exact = exact.clone();
                Box::pin(async move {
                    if peer != exact {
                        return;
                    }
                    let AccountHistoryFrame::DocumentHeader { body } = frame else {
                        return;
                    };
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    if matches!(meta.key, AccountDocumentKey::GroupControl { .. }) {
                        repeated_roles.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _ = release.acquire().await;
                    }
                })
            }
        }));
        assert!(cursor.begin_refresh());
        let target = fixture.target.clone();
        let peer = fixture.peer.clone();
        let mut pulling =
            tokio::spawn(
                async move { target.pull_account_documents_page(&peer, &mut cursor).await },
            );
        let before_release = tokio::time::timeout(Duration::from_secs(3), &mut pulling).await;
        let received = fixture.target.account_document_meta(&key);
        release.close();
        match before_release {
            Ok(result) => result
                .unwrap()
                .expect("the authenticated result page must remain valid"),
            Err(_) => tokio::time::timeout(Duration::from_secs(5), pulling)
                .await
                .unwrap()
                .unwrap()
                .expect("the released baseline page must still complete correctly"),
        }
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        fixture.servers.shutdown().await;
        assert!(
            received.as_ref() == Some(&expected),
            "already known role rows must not delay a new authenticated terminal result"
        );
        assert_eq!(repeated_roles.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_operation_page_commits_before_a_held_legacy_header_then_replays_all_rows() {
        use crate::node::{AccountGroupOperationPhase, AccountGroupTextInput, SyncCategory};
        let mut fixture = UpgradeFixture::new().await;
        for node in [&fixture.source, &fixture.target] {
            node.set_device_sync_category(SyncCategory::History, false)
                .unwrap();
        }
        let group = fixture
            .source
            .create_channel("operation prefix result", vec![])
            .await
            .unwrap();
        fixture
            .source
            .maintain_account_channels(
                &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
            )
            .await;
        fixture.source.publish_account_group_access_for(group);
        fixture.source.publish_account_group_operations_for(group);
        let mut cursor = DocumentSyncCursor::control();
        for _ in 0..16 {
            fixture
                .target
                .pull_account_documents_page(&fixture.peer, &mut cursor)
                .await
                .unwrap();
            if !cursor.pending {
                break;
            }
        }
        assert!(!cursor.pending && cursor.after.is_none());
        let roles = fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .digest_scope(AccountDocumentDomain::GroupControl, false);
        assert_ne!(roles, [0; 32]);
        assert_eq!(
            cursor.digests[AccountDocumentDomain::GroupControl as usize],
            roles
        );
        let account = fixture.source.account_id();
        let id = fixture
            .source
            .new_account_group_operation_id(&account)
            .unwrap();
        fixture
            .source
            .submit_account_group_text_if(
                &account,
                id,
                group,
                AccountGroupTextInput {
                    text: b"new terminal copy".to_vec(),
                    reply_to: None,
                    expires_at_ms: None,
                    protection: None,
                },
                |commit| commit(),
            )
            .unwrap();
        fixture.source.maintain_account_group_operation_effects();
        let status = fixture
            .source
            .account_group_operation_status(&account, id)
            .unwrap()
            .unwrap();
        assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
        assert_eq!(status.committed_events.len(), 1);
        fixture.source.publish_account_group_operations_for(group);
        assert_eq!(
            fixture
                .source
                .account_documents
                .lock()
                .unwrap()
                .digest_scope(AccountDocumentDomain::GroupControl, false),
            roles,
            "a real message result must not invent a different role observation"
        );
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let expected = fixture.source.account_document_meta(&key).unwrap();
        assert_ne!(
            fixture.target.account_document_meta(&key).as_ref(),
            Some(&expected)
        );
        let legacy_keys: Vec<_> = (1..=66)
            .map(|value| {
                seed_legacy_control_page_for_test(
                    &fixture.source,
                    crate::eventlog::ConversationId::new([value; 32]),
                )
            })
            .collect();
        let full_digest = fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .digest_exchange_scope(AccountDocumentDomain::GroupControl, true, true, true);
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let held = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        *fixture.source.account_session_test_hook.lock().unwrap() = Some(Arc::new({
            let exact = fixture.target.identity.public();
            let release = release.clone();
            let held = held.clone();
            move |peer, frame| {
                let exact = exact.clone();
                let release = release.clone();
                let held = held.clone();
                Box::pin(async move {
                    if peer == exact
                        && let AccountHistoryFrame::DocumentHeader { body } = frame
                    {
                        let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                        if matches!(meta.key, AccountDocumentKey::GroupLegacyOperation { .. }) {
                            held.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let _ = release.acquire().await;
                        }
                    }
                })
            }
        }));
        assert!(cursor.begin_refresh());
        let target = fixture.target.clone();
        let peer = fixture.peer.clone();
        let mut pulling = tokio::spawn(async move {
            let result = target.pull_account_documents_page(&peer, &mut cursor).await;
            (result, cursor)
        });
        let before_release = tokio::time::timeout(Duration::from_secs(8), &mut pulling).await;
        let received = fixture.target.account_document_meta(&key);
        let held_before_release = held.load(std::sync::atomic::Ordering::SeqCst);
        release.close();
        let (result, mut cursor) = match before_release {
            Ok(result) => result.unwrap(),
            Err(_) => tokio::time::timeout(Duration::from_secs(5), pulling)
                .await
                .expect("released baseline page completes")
                .unwrap(),
        };
        result.expect("the authenticated page remains valid");
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        if received.as_ref() != Some(&expected) {
            fixture.servers.shutdown().await;
            assert_eq!(
                received,
                Some(expected),
                "normal signed terminal operation must commit atomically before the first legacy header is released"
            );
            return;
        }
        assert_eq!(
            held_before_release, 0,
            "normal prefix does not load or emit legacy payloads"
        );
        assert_eq!(
            cursor.after.as_ref(),
            Some(&key),
            "Complete continues strictly after the normal operation prefix"
        );
        assert!(
            !cursor.up_to_date && cursor.operation_digest != full_digest,
            "partial prefix cannot claim whole-scope convergence"
        );
        let frame = fixture.target.account_document(&key).unwrap().unwrap();
        let frame = crate::node::account_group::operations::wire::OperationFrame::decode(
            frame.payload(),
            &fixture.source.account.public().ed25519_pub,
        )
        .unwrap();
        assert!(
            frame.proof.public() == fixture.source.identity.public()
                && frame
                    .projection
                    .results
                    .iter()
                    .any(|row| row.reference.id == id
                        && row.status.phase == AccountGroupOperationPhase::Committed
                        && row.status.committed_events == status.committed_events)
        );
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            legacy_keys
                .iter()
                .filter(|key| fixture.target.account_document_meta(key).is_some())
                .count(),
            64,
            "legacy-only continuation keeps its full64-row ceiling"
        );
        assert!(
            cursor
                .after
                .as_ref()
                .is_some_and(|key| matches!(key, AccountDocumentKey::GroupLegacyOperation { .. }))
        );
        (cursor, _) = repair_control_scope(&fixture.target, &fixture.peer, cursor).await;
        assert_control_scope_settled(&fixture.source, &fixture.target, &cursor);
        assert!(
            legacy_keys
                .iter()
                .all(|key| fixture.target.account_document_meta(key).is_some())
        );
        fixture.servers.shutdown().await;
    }

    fn control_scope_metadata(node: &Node) -> Vec<AccountDocumentMeta> {
        let store = node.account_documents.lock().unwrap();
        let mut rows = Vec::new();
        loop {
            let page = store.list_exchange_scope(
                AccountDocumentDomain::GroupControl,
                rows.last().map(|meta: &AccountDocumentMeta| &meta.key),
                64,
                true,
                true,
                true,
            );
            if page.is_empty() {
                return rows;
            }
            rows.extend(page);
        }
    }
    fn control_scope_differences(source: &Node, target: &Node) -> [[usize; 4]; 3] {
        let source = control_scope_metadata(source);
        let target = control_scope_metadata(target);
        let class = |key: &AccountDocumentKey| match key {
            AccountDocumentKey::GroupControl { .. } => 0,
            AccountDocumentKey::GroupOperation { .. } => 1,
            AccountDocumentKey::GroupLegacyOperation { .. } => 2,
            AccountDocumentKey::ConversationPersonal { .. } => 3,
            _ => unreachable!("the negotiated control scope contains only its four key classes"),
        };
        let mut differences = [[0; 4]; 3];
        for meta in &source {
            match target.iter().find(|other| other.key == meta.key) {
                None => differences[0][class(&meta.key)] += 1,
                Some(other) if other != meta => differences[2][class(&meta.key)] += 1,
                _ => {}
            }
        }
        for meta in &target {
            if !source.iter().any(|other| other.key == meta.key) {
                differences[1][class(&meta.key)] += 1;
            }
        }
        differences
    }
    async fn repair_control_scope(
        target: &Arc<Node>,
        peer: &PeerRecord,
        mut cursor: DocumentSyncCursor,
    ) -> (DocumentSyncCursor, usize) {
        let mut completed_unsettled = 0;
        for _ in 0..16 {
            if !cursor.pending && !cursor.up_to_date {
                assert!(
                    cursor.begin_refresh(),
                    "only a completed scope starts a fresh comparison"
                );
            }
            target
                .pull_account_documents_page(peer, &mut cursor)
                .await
                .unwrap();
            if !cursor.pending && cursor.up_to_date {
                break;
            }
            if !cursor.pending {
                completed_unsettled += 1;
            }
        }
        (cursor, completed_unsettled)
    }
    fn assert_control_scope_settled(source: &Node, target: &Node, cursor: &DocumentSyncCursor) {
        assert!(
            !cursor.pending && cursor.up_to_date && cursor.after.is_none(),
            "repair must finish a fully settled scope: pending={} after={} settled={} visited={} up_to_date={} error={} deferred={} retry={} group_upgrade={} personal_upgrade={} differences(source-only,target-only,changed;role,operation,legacy,personal)={:?}",
            cursor.pending,
            cursor.after.is_some(),
            cursor.settled,
            cursor.visited,
            cursor.up_to_date,
            cursor.error.is_some(),
            cursor.admission_deferred,
            cursor.retry_at.is_some(),
            cursor.group_upgrade,
            cursor.personal_upgrade,
            control_scope_differences(source, target),
        );
        assert!(
            control_scope_metadata(source) == control_scope_metadata(target),
            "settled repair must retain the complete exact source metadata set"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn completed_control_scope_recompares_a_selected_role_that_grew_after_its_snapshot() {
        let mut fixture = UpgradeFixture::new().await;
        let first = fixture
            .source
            .create_channel("first snapshot role", vec![])
            .await
            .unwrap();
        let second = fixture
            .source
            .create_channel("second snapshot role", vec![])
            .await
            .unwrap();
        fixture
            .source
            .maintain_account_channels(
                &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
            )
            .await;
        fixture.source.publish_account_group_access(&mut None);
        let (cursor, _) = repair_control_scope(
            &fixture.target,
            &fixture.peer,
            DocumentSyncCursor::control(),
        )
        .await;
        assert!(
            !cursor.pending && cursor.up_to_date && cursor.after.is_none(),
            "fixture warmup must settle before controlled source growth"
        );
        let source_warmup = control_scope_metadata(&fixture.source);
        let target_warmup = control_scope_metadata(&fixture.target);
        assert_eq!(
            source_warmup, target_warmup,
            "fixture warmup must contain the exact source metadata before controlled growth"
        );
        let rows = control_scope_metadata(&fixture.source);
        assert_eq!(rows.len(), 2);
        let first_key = rows[0].key.clone();
        let lower = if matches!(&first_key, AccountDocumentKey::GroupControl { channel, .. } if *channel == hex::encode(first.as_bytes()))
        {
            first
        } else {
            second
        };
        let higher = if lower == first { second } else { first };
        fixture.source.dissolve_channel(lower).await.unwrap();
        fixture.source.publish_account_group_access_for(lower);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let entered_tx = Arc::new(std::sync::Mutex::new(Some(entered_tx)));
        let release_rx = Arc::new(std::sync::Mutex::new(Some(release_rx)));
        let exact_peer = fixture.target.identity.public();
        *fixture.source.account_session_test_hook.lock().unwrap() =
            Some(Arc::new(move |peer, frame| {
                let first_key = first_key.clone();
                let entered_tx = entered_tx.clone();
                let release_rx = release_rx.clone();
                let exact_peer = exact_peer.clone();
                Box::pin(async move {
                    if peer != exact_peer {
                        return;
                    }
                    let AccountHistoryFrame::DocumentHeader { body } = frame else {
                        return;
                    };
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    if meta.key != first_key {
                        return;
                    }
                    let entered = entered_tx.lock().unwrap().take();
                    if let Some(entered) = entered {
                        entered.send(()).unwrap();
                        let release = release_rx.lock().unwrap().take().unwrap();
                        release.await.unwrap();
                    }
                })
            }));
        let source_before = control_scope_metadata(&fixture.source);
        let target = fixture.target.clone();
        let peer = fixture.peer.clone();
        let mut cursor = cursor;
        assert!(cursor.begin_refresh());
        let pulling =
            tokio::spawn(async move { repair_control_scope(&target, &peer, cursor).await });
        tokio::time::timeout(Duration::from_secs(5), entered_rx)
            .await
            .unwrap()
            .unwrap();
        fixture.source.dissolve_channel(higher).await.unwrap();
        fixture.source.publish_account_group_access_for(higher);
        assert!(
            control_scope_metadata(&fixture.source) != source_before,
            "the real signed source role must advance after the page snapshot"
        );
        release_tx.send(()).unwrap();
        let (cursor, completed_unsettled) = tokio::time::timeout(Duration::from_secs(5), pulling)
            .await
            .unwrap()
            .unwrap();
        *fixture.source.account_session_test_hook.lock().unwrap() = None;
        fixture.servers.shutdown().await;
        assert!(
            completed_unsettled > 0,
            "the controlled publication must exercise a completed but unsettled scope before fresh repair"
        );
        assert_control_scope_settled(&fixture.source, &fixture.target, &cursor);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn known_role_patch_preserves_64_row_pages_and_repairs_roles_changed_between_pages() {
        use crate::eventlog::ConversationId;
        use crate::node::account_group::operations::legacy_wire::{
            LegacyOperationFrame, LegacyOperationProjection,
        };
        let mut fixture = UpgradeFixture::new().await;
        let group = fixture
            .source
            .create_channel("known page prefix", vec![])
            .await
            .unwrap();
        fixture
            .source
            .maintain_account_channels(
                &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
            )
            .await;
        fixture.source.publish_account_group_access_for(group);
        let mut cursor = DocumentSyncCursor::control();
        for _ in 0..16 {
            fixture
                .target
                .pull_account_documents_page(&fixture.peer, &mut cursor)
                .await
                .unwrap();
            if !cursor.pending {
                break;
            }
        }
        assert!(!cursor.pending);
        let roles = cursor.digests[AccountDocumentDomain::GroupControl as usize];
        let mut operation_keys = Vec::new();
        for value in 1..=65 {
            operation_keys.push(install_operation_head_at(
                &fixture,
                ConversationId::new([value; 32]),
            ));
        }
        let retained_operation = {
            let mut store = fixture.source.account_documents.lock().unwrap();
            let old = store.meta(&operation_keys[0]).unwrap();
            let meta = AccountDocumentMeta::new(
                &fixture.source.account,
                fixture.source.user_id(),
                operation_keys[0].clone(),
                old.revision + 1,
                &[],
                true,
            );
            assert!(
                store.merge(meta, &[]).is_err(),
                "typed operation heads cannot be replaced by generic tombstones"
            );
            assert_eq!(store.meta(&operation_keys[0]), Some(old.clone()));
            old
        };
        let legacy_group = ConversationId::new([90; 32]);
        let legacy_key = AccountDocumentKey::GroupLegacyOperation {
            channel: hex::encode(legacy_group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let legacy = LegacyOperationFrame::new(
            &fixture.source,
            legacy_group,
            1,
            LegacyOperationProjection::default(),
        )
        .encode();
        let legacy_meta = AccountDocumentMeta::new(
            &fixture.source.account,
            fixture.source.user_id(),
            legacy_key.clone(),
            1,
            &legacy,
            false,
        );
        fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .merge(legacy_meta, &legacy)
            .unwrap();
        fixture
            .source
            .delete_account_contact_if(
                &fixture.source.account_id(),
                &Account::generate().account_id(),
                false,
                |commit| commit(),
            )
            .unwrap();
        {
            let store = fixture.source.account_documents.lock().unwrap();
            let full = store.list_exchange_scope(
                AccountDocumentDomain::GroupControl,
                None,
                64,
                true,
                true,
                true,
            );
            assert!(matches!(
                full[0].key,
                AccountDocumentKey::GroupControl { .. }
            ));
            assert_eq!(
                store.list_operation_patch(None, 64, true, true, [0; 32]),
                full,
                "an absent digest cannot suppress role observations"
            );
            assert_eq!(
                store.list_operation_patch(None, 64, true, true, [99; 32]),
                full,
                "a mismatched digest cannot suppress unknown role observations"
            );
            let patch = store.list_operation_patch(None, 64, true, true, roles);
            assert_eq!(
                patch.len(),
                64,
                "known rows must be filtered before the row ceiling"
            );
            assert!(
                patch
                    .iter()
                    .all(|meta| !matches!(meta.key, AccountDocumentKey::GroupControl { .. }))
            );
            assert!(patch.windows(2).all(|rows| rows[0].key < rows[1].key));
        }
        assert!(cursor.begin_refresh());
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(
            cursor.after.is_some(),
            "the real 64-row page must retain its continuation"
        );
        let copied = operation_keys
            .iter()
            .filter(|key| fixture.target.account_document_meta(key).is_some())
            .count();
        assert_eq!(
            copied, 64,
            "a redundant role prefix cannot consume an operation slot"
        );
        assert_eq!(
            fixture.target.account_document_meta(&operation_keys[0]),
            Some(retained_operation)
        );
        let lower_operation = install_operation_head_at(&fixture, ConversationId::new([0; 32]));
        let added = fixture
            .source
            .create_channel("unknown between pages", vec![])
            .await
            .unwrap();
        fixture
            .source
            .maintain_account_channels(
                &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
            )
            .await;
        fixture.source.publish_account_group_access_for(added);
        let added_key = AccountDocumentKey::GroupControl {
            channel: hex::encode(added.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let added_meta = fixture.source.account_document_meta(&added_key).unwrap();
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            cursor.after.as_ref(),
            operation_keys.last(),
            "last normal operation has its own complete prefix before legacy"
        );
        assert!(fixture.target.account_document_meta(&legacy_key).is_none());
        assert!(
            fixture
                .target
                .account_document_meta(&lower_operation)
                .is_none(),
            "new lower operation cannot jump behind an existing continuation"
        );
        assert!(
            !cursor.up_to_date,
            "a normal prefix cannot falsely settle fullscope"
        );
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert!(cursor.after.is_none());
        assert!(
            !cursor.up_to_date,
            "a changed complete scope cannot falsely settle the old roles"
        );
        assert!(
            fixture.target.account_document_meta(&added_key).is_none(),
            "a lower role key is repaired by a fresh scope, never hidden behind the operation continuation"
        );
        assert!(fixture.target.account_document_meta(&legacy_key).is_some());
        assert!(
            fixture
                .target
                .account_documents
                .lock()
                .unwrap()
                .personal
                .keys()
                .any(|key| matches!(key, AccountDocumentKey::ConversationPersonal { .. }))
        );
        (cursor, _) = repair_control_scope(&fixture.target, &fixture.peer, cursor).await;
        assert_control_scope_settled(&fixture.source, &fixture.target, &cursor);
        assert_eq!(
            fixture.target.account_document_meta(&lower_operation),
            fixture.source.account_document_meta(&lower_operation),
            "fresh comparison repairs the new lower operation exactly"
        );
        let current_added = fixture.source.account_document_meta(&added_key).unwrap();
        assert!(
            current_added.revision >= added_meta.revision,
            "repair must retain the role introduced behind the continuation"
        );
        assert!(
            fixture.target.account_document_meta(&added_key).as_ref() == Some(&current_added),
            "repair must retain the exact current signed source role"
        );
        assert!(
            operation_keys
                .iter()
                .all(|key| fixture.target.account_document_meta(key).is_some())
        );
        fixture.servers.shutdown().await;
    }

    #[tokio::test]
    async fn access_only_v5_keeps_its_role_scope_when_operation_rows_exist() {
        let mut fixture = UpgradeFixture::new().await;
        let group = fixture
            .source
            .create_channel("legacy known roles", vec![])
            .await
            .unwrap();
        fixture.source.publish_account_group_access_for(group);
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
                    body: serde_json::to_vec(&request).unwrap(),
                },
            )
            .await
            .unwrap();
        let mut roles = 0;
        loop {
            let bytes = channel.recv().await.unwrap();
            match AccountHistoryFrame::decode_versioned(&bytes).unwrap().1 {
                AccountHistoryFrame::DocumentHeader { body } => {
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    assert!(matches!(meta.key, AccountDocumentKey::GroupControl { .. }));
                    roles += 1;
                }
                AccountHistoryFrame::DocumentsComplete { body } => {
                    let complete: DocumentComplete = serde_json::from_slice(&body).unwrap();
                    assert!(complete.operation.is_none());
                    assert_eq!(
                        complete.digest,
                        fixture
                            .source
                            .account_documents
                            .lock()
                            .unwrap()
                            .digest_scope(AccountDocumentDomain::GroupControl, false)
                    );
                    break;
                }
                AccountHistoryFrame::DocumentChunk { .. } => {}
                _ => panic!("the legacy role scope must retain its existing frame kinds"),
            }
        }
        assert!(roles > 0);
        channel.recycle(&fixture.target);
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
    #[tokio::test]
    async fn legacy_child_documents_require_the_negotiated_operation_scope_over_noise() {
        use crate::node::account_group::operations::legacy_wire::{
            LegacyOperationFrame, LegacyOperationProjection,
        };
        let mut fixture = UpgradeFixture::new().await;
        let group = crate::eventlog::ConversationId::new([74; 32]);
        let key = AccountDocumentKey::GroupLegacyOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: fixture.source.user_id(),
        };
        let payload = LegacyOperationFrame::new(
            &fixture.source,
            group,
            1,
            LegacyOperationProjection::default(),
        )
        .encode();
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
            .merge(meta.clone(), &payload)
            .unwrap();
        let old_digest = fixture
            .source
            .account_documents
            .lock()
            .unwrap()
            .digest_scope(AccountDocumentDomain::GroupControl, true);
        let mut channel = fixture
            .target
            .begin_account_exchange(
                &fixture.peer,
                AccountSyncVersion::V2,
                AccountHistoryFrame::DocumentsRequest {
                    body: DocumentRequest {
                        version: 5,
                        account_id: fixture.target.account_id(),
                        domain: AccountDocumentDomain::GroupControl,
                        after: None,
                        known_digest: [0; 32],
                        operation: Some(OperationProbe {
                            version: 1,
                            nonce: [7; 32],
                            after: None,
                            known_digest: [0; 32],
                            asset: None,
                            legacy_leave: false,
                            personal: false,
                        }),
                    }
                    .encode(),
                },
            )
            .await
            .unwrap();
        loop {
            let bytes = channel.recv().await.unwrap();
            let (_, frame) = AccountHistoryFrame::decode_versioned(&bytes).unwrap();
            match frame {
                AccountHistoryFrame::DocumentHeader { body } => {
                    let meta: AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                    assert!(!matches!(
                        meta.key,
                        AccountDocumentKey::GroupLegacyOperation { .. }
                    ));
                }
                AccountHistoryFrame::DocumentChunk { .. } => {}
                AccountHistoryFrame::DocumentsComplete { body } => {
                    let complete: DocumentComplete = serde_json::from_slice(&body).unwrap();
                    assert_eq!(complete.digest, old_digest);
                    assert!(!complete.operation.unwrap().legacy_leave);
                    break;
                }
                _ => panic!("unexpected account document frame"),
            }
        }
        channel.recycle(&fixture.target);
        assert!(fixture.target.account_document_meta(&key).is_none());
        let mut cursor = DocumentSyncCursor::mandatory();
        fixture
            .target
            .pull_account_documents_page(&fixture.peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(fixture.target.account_document_meta(&key).unwrap(), meta);
        assert!(
            fixture
                .target
                .group_operation_versions
                .lock()
                .unwrap()
                .get(&fixture.source.user_id())
                .is_some_and(|support| support.kinds
                    & crate::node::account_group::operations::legacy_wire::LEGACY_LEAVE_CAPABILITY
                    != 0)
        );
        fixture.servers.shutdown().await;
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
                .is_some_and(|support| support.public == fixture.source.identity.public()
                    && support.kinds == (local_operation_capabilities() | crate::node::account_group::operations::legacy_wire::LEGACY_LEAVE_CAPABILITY)),
            "an operation-capable authenticated exchange must establish its own support evidence"
        );
        fixture.servers.shutdown().await;
    }
    #[tokio::test]
    async fn legacy_operation_completion_proves_only_original_control_kinds() {
        let mut fixture = UpgradeFixture::new().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = fixture.peer.clone();
        peer.addr = listener.local_addr().unwrap();
        let source = fixture.source.clone();
        let server = tokio::spawn(async move {
            let (mut channel, request) = receive_document_request(&source, listener).await;
            let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
            let complete = serde_json::json!({ "enabled":true, "domain":"group_control",
                "next":null, "digest":([0u8;32]), "operation": {
                    "version":1, "nonce":request.pointer("/operation/nonce").unwrap() } });
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
        });
        fixture
            .target
            .pull_document_exchange(&peer, &mut DocumentSyncCursor::default())
            .await
            .unwrap();
        server.await.unwrap();
        {
            let supports = fixture.target.group_operation_versions.lock().unwrap();
            let support = supports.get(&fixture.source.user_id()).unwrap();
            assert_eq!(support.public, fixture.source.identity.public());
            assert_eq!(
                support.kinds,
                crate::node::AccountGroupOperationKind::legacy_capabilities()
            );
            assert_eq!(
                support.kinds & crate::node::AccountGroupOperationKind::Text.capability_bit(),
                0
            );
            assert_ne!(
                support.kinds & crate::node::AccountGroupOperationKind::Rename.capability_bit(),
                0
            );
        }
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
                GroupOperationSupport {
                    public: fixture.source.identity.public(),
                    observed_at: Instant::now(),
                    kinds: crate::node::AccountGroupOperationKind::legacy_capabilities(),
                },
            );
    }
    #[tokio::test]
    async fn authenticated_group_eof_backoff_keeps_pending_work_out_of_runnable_queue() {
        use crate::node::account_sync::{mandatory_retry_deadline, take_ready_mandatory_peer};
        use std::collections::VecDeque;
        let mut fixture = UpgradeFixture::new().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = fixture.peer.clone();
        peer.addr = listener.local_addr().unwrap();
        let key = install_operation_head(&fixture);
        let meta = fixture.source.account_document_meta(&key).unwrap();
        let source = fixture.source.clone();
        let server = tokio::spawn(async move {
            let (mut channel, _) = receive_document_request(&source, listener).await;
            channel
                .send(
                    &AccountHistoryFrame::DocumentHeader {
                        body: serde_json::to_vec(&meta).unwrap(),
                    }
                    .encode_version(AccountSyncVersion::V2)
                    .unwrap(),
                )
                .await
                .unwrap();
            // Drop after a real signed header, before its body and completion.
        });
        let mut cursor = DocumentSyncCursor::mandatory();
        let error = fixture
            .target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .unwrap_err();
        assert!(transport_closed(&error));
        server.await.unwrap();
        let deadline = cursor
            .retry_at
            .expect("authenticated EOF has a short retry deadline");
        assert!(cursor.pending && !cursor.up_to_date && cursor.error.is_none());
        assert!(
            !cursor.begin_refresh(),
            "an incoming wake must retain the pending page"
        );
        let id = peer.public.user_id();
        let mut queue = VecDeque::from([(id.clone(), peer.clone())]);
        for _ in 0..32 {
            assert!(
                take_ready_mandatory_peer(
                    &mut queue,
                    |_| cursor.retry_at,
                    deadline - Duration::from_nanos(1)
                )
                .is_none(),
                "queued pending work must not create maintenance turns during its retry backoff"
            );
        }
        queue.push_back(("ready peer".into(), peer.clone()));
        assert_eq!(
            take_ready_mandatory_peer(
                &mut queue,
                |key| (key == &id).then_some(deadline),
                deadline - Duration::from_nanos(1)
            )
            .unwrap()
            .0,
            "ready peer",
            "a deferred peer cannot block healthy work"
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(
            mandatory_retry_deadline(&queue, |_| cursor.retry_at),
            Some(deadline),
            "the runner must wake at the short retry deadline rather than the periodic tick"
        );
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        let (resumed, _) =
            take_ready_mandatory_peer(&mut queue, |_| cursor.retry_at, deadline).unwrap();
        assert_eq!(resumed, id);
        assert!(mandatory_retry_deadline(&queue, |_| cursor.retry_at).is_none());
        assert!(cursor.pending && !cursor.up_to_date);
        fixture.servers.shutdown().await;
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
        install_operation_head_at(fixture, crate::eventlog::ConversationId::new([71; 32]))
    }
    fn install_operation_head_at(
        fixture: &UpgradeFixture,
        group: crate::eventlog::ConversationId,
    ) -> AccountDocumentKey {
        use crate::node::account_group::operations::wire::{OperationFrame, OperationProjection};
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
        fixture
            .target
            .set_device_sync_category(SyncCategory::Profile, false)
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
        assert_eq!(cursor.digests.len(), DOMAINS.len());
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
    // Paste this nested module INSIDE sync.rs::group_control_tests, alongside
    // UpgradeFixture and receive_document_request. It reuses their private helpers.
    mod presentation_compat_tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        async fn legacy_peer(fixture: &mut UpgradeFixture) -> (PeerRecord, Arc<AtomicUsize>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut peer = fixture.peer.clone();
            peer.addr = listener.local_addr().unwrap();
            let source = fixture.source.clone();
            let attempts = Arc::new(AtomicUsize::new(0));
            let counted = attempts.clone();
            fixture.servers.spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let source = source.clone();
                let counted = counted.clone();
                children.spawn(async move {
                    let mut channel = source.privacy_accept(stream).await.unwrap();
                    while let Ok(bytes) = channel.recv().await {
                        let Some((_, AccountHistoryFrame::SessionRequest {
                            request_id,
                            request: super::super::super::super::account_sync_wire::AccountSessionRequest::Documents { body },
                        })) = AccountHistoryFrame::decode_versioned(&bytes) else {
                            break;
                        };
                        let request: DocumentRequest = serde_json::from_slice(&body).unwrap();
                        if request.version > 5 {
                            // An older enum parser closes on the unknown domain
                            // before sending SessionReady or any document header.
                            counted.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                        if channel.send(&AccountHistoryFrame::SessionReady { request_id }
                            .encode_version(AccountSyncVersion::V2).unwrap()).await.is_err()
                        {
                            break;
                        }
                        if !source.serve_account_documents(&mut channel, request).await {
                            break;
                        }
                    }
                });
            }
        });
            (peer, attempts)
        }

        #[tokio::test]
        async fn old_peer_v6_probe_cools_both_domains_without_blocking_drafts_and_retries_after_expiry()
         {
            let mut fixture = UpgradeFixture::new().await;
            let (peer, attempts) = legacy_peer(&mut fixture).await;
            let key = AccountDocumentKey::Draft {
                conversation: DocumentConversation::Account("d".repeat(32)),
            };
            let first = fixture
                .source
                .put_account_document_if(
                    key.clone(),
                    b"before presentation probe".to_vec(),
                    None,
                    |commit| commit(),
                )
                .unwrap();
            let mut cursor = DocumentSyncCursor::optional();
            cursor.domain = AccountDocumentDomain::PublicProfile as usize;
            assert!(
                fixture
                    .target
                    .pull_account_documents_page(&peer, &mut cursor)
                    .await
                    .is_err()
            );
            let cooldown = cursor.presentation_retry_at.unwrap();
            assert!(cooldown > Instant::now() + Duration::from_secs(240));
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            assert!(cursor.retry_at.is_none());
            assert!(cursor.error.is_none());

            fixture
                .source
                .put_account_document_if(
                    key.clone(),
                    b"after presentation probe".to_vec(),
                    Some(first.version()),
                    |commit| commit(),
                )
                .unwrap();
            for _ in 0..16 {
                fixture
                    .target
                    .pull_account_documents_page(&peer, &mut cursor)
                    .await
                    .unwrap();
                if !cursor.pending {
                    break;
                }
            }
            assert!(
                !cursor.pending,
                "legacy optional work must finish its bounded sweep"
            );
            assert_eq!(
                fixture
                    .target
                    .account_document(&key)
                    .unwrap()
                    .unwrap()
                    .payload(),
                b"after presentation probe"
            );
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                1,
                "both v6 domains share the compatibility cooldown"
            );
            assert_eq!(cursor.presentation_retry_at, Some(cooldown));

            // No clock delay: expire only this cursor's domain capability backoff.
            cursor.presentation_retry_at = Some(Instant::now() - Duration::from_secs(1));
            cursor.domain = AccountDocumentDomain::GroupActivity as usize;
            assert!(
                fixture
                    .target
                    .pull_account_documents_page(&peer, &mut cursor)
                    .await
                    .is_err()
            );
            // A retired pooled session may retry once on a fresh authenticated
            // connection; this remains one bounded logical capability probe.
            assert!((2..=3).contains(&attempts.load(Ordering::SeqCst)));
            assert!(cursor.presentation_retry_at.unwrap() > cooldown);
            fixture.servers.shutdown().await;
        }

        #[tokio::test]
        async fn valid_v6_header_then_eof_is_an_interruption_not_legacy_downgrade() {
            let mut fixture = UpgradeFixture::new().await;
            let group = fixture
                .source
                .create_channel("bounded observed activity", vec![])
                .await
                .unwrap();
            fixture.source.publish_group_inventory_for(group);
            fixture.source.publish_group_activity_for(group);
            let key = AccountDocumentKey::GroupActivity {
                channel: hex::encode(group.as_bytes()),
                source_device: fixture.source.user_id(),
            };
            let document = fixture.source.account_document(&key).unwrap().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut peer = fixture.peer.clone();
            peer.addr = listener.local_addr().unwrap();
            let source = fixture.source.clone();
            let responder = tokio::spawn(async move {
                let (mut channel, body) = receive_document_request(&source, listener).await;
                let request: DocumentRequest = serde_json::from_slice(&body).unwrap();
                assert_eq!(request.version, 6);
                assert_eq!(request.domain, AccountDocumentDomain::GroupActivity);
                channel
                    .send(
                        &AccountHistoryFrame::DocumentHeader {
                            body: serde_json::to_vec(&document.meta).unwrap(),
                        }
                        .encode_version(AccountSyncVersion::V2)
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                // A valid header proves the peer understands this domain. Truncate
                // the body, rather than disguising a partial transfer as old support.
            });
            let mut cursor = DocumentSyncCursor::for_domain(AccountDocumentDomain::GroupActivity);
            assert!(
                fixture
                    .target
                    .pull_account_documents_page(&peer, &mut cursor)
                    .await
                    .is_err()
            );
            responder.await.unwrap();
            assert!(cursor.presentation_retry_at.is_none());
            assert!(cursor.retry_at.is_some());
            assert!(cursor.error.is_some());
            assert_eq!(
                cursor.error_domain,
                Some(AccountDocumentDomain::GroupActivity)
            );
            assert!(!cursor.up_to_date);
            assert!(fixture.target.account_document(&key).unwrap().is_none());
            fixture.servers.shutdown().await;
        }
    }
}
