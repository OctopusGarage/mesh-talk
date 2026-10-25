//! Account-owned operation admission and body-free lifecycle queries.
mod execution;
pub(in crate::node) mod journal;
pub(in crate::node) mod wire;
use super::*;
use crate::{
    channel::HistoryPolicy,
    node::{
        Node, NodeError,
        channels::{SimpleControl, normalize_simple_name},
    },
};
pub(in crate::node) use wire::valid_payload;
use wire::{LookupProjection, OperationProjection, RequestProjection, ResultProjection};

/// A caller-generated logical operation ID, distinct from every public event ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct AccountGroupOperationId([u8; 32]);
impl AccountGroupOperationId {
    pub fn for_origin(origin: &PublicIdentity, nonce: [u8; 16]) -> Self {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-account-group-operation-origin-v1");
        hash.update(origin.ed25519_pub);
        hash.update(origin.x25519_pub);
        let prefix = hash.finalize();
        let mut bytes = [0; 32];
        bytes[..16].copy_from_slice(&prefix[..16]);
        bytes[16..].copy_from_slice(&nonce);
        Self(bytes)
    }
    pub(in crate::node) fn matches_origin(&self, origin: &PublicIdentity) -> bool {
        let expected = Self::for_origin(origin, [0; 16]);
        self.0[..16] == expected.0[..16]
    }
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
    pub fn from_hex(value: &str) -> Result<Self, NodeError> {
        let bytes = hex::decode(value)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .filter(|bytes: &[u8; 32]| *bytes != [0; 32])
            .ok_or_else(|| NodeError::InvalidInput("invalid account group operation ID".into()))?;
        Ok(Self(bytes))
    }
}

/// Constructed by the message/file owners; no remote filesystem path is an instruction.
#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedAccountGroupMessage {
    pub(super) kind: AccountGroupOperationKind,
    pub(super) encoded_body: Vec<u8>,
    pub(super) staged_assets: Vec<[u8; 32]>,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum AccountGroupOperation {
    Send {
        message: PreparedAccountGroupMessage,
    },
    React {
        target: EventId,
        emoji: String,
        remove: bool,
    },
    Recall {
        target: EventId,
    },
    Invite {
        target_account_id: String,
    },
    AcceptInvitation,
    DeclineInvitation,
    Rename {
        name: String,
    },
    RemoveAccount {
        target_account_id: String,
    },
    TransferOwnership {
        target_account_id: String,
    },
    Dissolve,
    SetHistoryPolicy {
        policy: HistoryPolicy,
    },
    LeaveAccount,
}
impl AccountGroupOperation {
    fn simple_control(&self) -> Option<SimpleControl> {
        match self {
            Self::Rename { name } => Some(SimpleControl::Rename(name.clone())),
            Self::SetHistoryPolicy { policy } => Some(SimpleControl::HistoryPolicy(*policy)),
            Self::Dissolve => Some(SimpleControl::Dissolve),
            _ => None,
        }
    }
    fn valid_simple_control(&self) -> bool {
        match self {
            Self::Rename { name } => normalize_simple_name(name).is_ok_and(|value| value == *name),
            Self::SetHistoryPolicy { .. } | Self::Dissolve => true,
            _ => false,
        }
    }
    pub fn kind(&self) -> AccountGroupOperationKind {
        match self {
            Self::Send { message } => message.kind,
            Self::React { .. } => AccountGroupOperationKind::Reaction,
            Self::Recall { .. } => AccountGroupOperationKind::Recall,
            Self::Invite { .. } => AccountGroupOperationKind::Invite,
            Self::AcceptInvitation => AccountGroupOperationKind::AcceptInvitation,
            Self::DeclineInvitation => AccountGroupOperationKind::DeclineInvitation,
            Self::Rename { .. } => AccountGroupOperationKind::Rename,
            Self::RemoveAccount { .. } => AccountGroupOperationKind::RemoveAccount,
            Self::TransferOwnership { .. } => AccountGroupOperationKind::TransferOwnership,
            Self::Dissolve => AccountGroupOperationKind::Dissolve,
            Self::SetHistoryPolicy { .. } => AccountGroupOperationKind::HistoryPolicy,
            Self::LeaveAccount => AccountGroupOperationKind::LeaveAccount,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountGroupOperationKind {
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
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountGroupOperationRejection {
    NotAuthorized,
    OwnerMustTransfer,
    MembershipChanged,
    OriginalSignerNoLongerAuthorized,
    InvalidTarget,
    Expired,
    InvalidPayload,
    OperationIdConflict,
    ResourceLimit,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountGroupOperationPhase {
    WaitingForExecutor,
    AwaitingAcceptance,
    Accepted,
    Executing,
    Reconciling,
    ActionRequired {
        reason: AccountGroupOperationRejection,
    },
    CancellationRequested,
    Cancelled,
    Committed,
    Rejected {
        reason: AccountGroupOperationRejection,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountGroupOperationExecution {
    pub device_id: String,
    pub phase: AccountGroupOperationPhase,
    pub committed_events: Vec<EventId>,
}
/// Contains no request body, file name, keys or protected content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountGroupOperationStatus {
    pub id: AccountGroupOperationId,
    pub account_id: String,
    pub group: ConversationId,
    pub kind: AccountGroupOperationKind,
    pub phase: AccountGroupOperationPhase,
    pub executions: Vec<AccountGroupOperationExecution>,
    pub committed_events: Vec<EventId>,
    pub primary_event: Option<EventId>,
}
#[derive(Clone, Debug)]
pub struct AccountGroupOperationQuery {
    pub group: Option<ConversationId>,
    pub pending_only: bool,
    pub after: Option<AccountGroupOperationId>,
    pub limit: usize,
}

impl Node {
    fn ensure_group_operation_account(&self, expected_account: &str) -> Result<(), NodeError> {
        if expected_account != self.account_id() {
            return Err(NodeError::Authorization("account changed".into()));
        }
        Ok(())
    }
    /// Mint an ID without admitting work or writing account/device storage.
    pub fn new_account_group_operation_id(
        &self,
        expected_account: &str,
    ) -> Result<AccountGroupOperationId, NodeError> {
        use rand_core::RngCore;
        self.ensure_group_operation_account(expected_account)?;
        let mut nonce = [0; 16];
        rand_core::OsRng.fill_bytes(&mut nonce);
        Ok(AccountGroupOperationId::for_origin(
            &self.identity.public(),
            nonce,
        ))
    }
    pub(in crate::node) fn complete_account_group_operation(
        &self,
        reference: &journal::OperationReference,
        events: &[crate::eventlog::Event],
    ) -> Result<(), crate::eventlog::LogError> {
        self.group_operations
            .lock()
            .expect("group operation journal mutex")
            .complete(reference, events)
    }
    pub fn submit_account_group_operation(
        &self,
        expected_account: &str,
        id: AccountGroupOperationId,
        group: ConversationId,
        operation: AccountGroupOperation,
    ) -> Result<AccountGroupOperationStatus, NodeError> {
        self.submit_account_group_operation_if(expected_account, id, group, operation, |commit| {
            commit()
        })
    }
    /// The host retains its current runtime/session guard across durable admission.
    pub fn submit_account_group_operation_if(
        &self,
        expected_account: &str,
        id: AccountGroupOperationId,
        group: ConversationId,
        operation: AccountGroupOperation,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountGroupOperationStatus, NodeError> {
        self.ensure_group_operation_account(expected_account)?;
        let operation = match operation {
            AccountGroupOperation::Rename { name } => AccountGroupOperation::Rename {
                name: normalize_simple_name(&name)?,
            },
            operation @ (AccountGroupOperation::SetHistoryPolicy { .. }
            | AccountGroupOperation::Dissolve) => operation,
            _ => {
                return Err(NodeError::InvalidInput(
                    "account group operation kind unavailable".into(),
                ));
            }
        };
        let digest = journal::operation_digest(group, &operation);
        let mut result = None;
        let authorized = authorize(&mut || {
            // Account replacement closes admission while holding this same
            // gate. Preserve the host's session→delivery→journal lock order.
            let _delivery = self.delivery.lock().expect("delivery mutex");
            if self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(NodeError::Authorization(
                    "restart node after account adoption".into(),
                ));
            }
            let existing = {
                let journal = self
                    .group_operations
                    .lock()
                    .expect("group operation journal mutex");
                journal
                    .entries
                    .get(&id)
                    .map(|entry| {
                        (
                            entry.reference.group,
                            entry.reference.digest,
                            entry.status.clone(),
                        )
                    })
                    .or_else(|| {
                        journal
                            .lookups
                            .get(&id)
                            .map(|entry| (entry.group, entry.digest, entry.status.clone()))
                    })
            };
            if let Some((old_group, old_digest, status)) = existing {
                if old_group != group || old_digest != digest {
                    return Err(NodeError::InvalidInput(
                        "operation ID belongs to another request".into(),
                    ));
                }
                result = Some(status);
                return Ok(());
            }
            let status = AccountGroupOperationStatus {
                id,
                account_id: self.account_id(),
                group,
                kind: operation.kind(),
                phase: AccountGroupOperationPhase::Reconciling,
                executions: Vec::new(),
                committed_events: Vec::new(),
                primary_event: None,
            };
            if !id.matches_origin(&self.identity.public()) {
                if !self.preflight_group_operation_lookup(LookupProjection {
                    id,
                    group,
                    kind: operation.kind(),
                    digest,
                    cancel: false,
                    nonce: self.operation_query_nonce(id),
                }) {
                    return Err(NodeError::InvalidInput(
                        "account group operation transport capacity exhausted".into(),
                    ));
                }
                self.group_operations
                    .lock()
                    .expect("group operation journal mutex")
                    .put_lookup(journal::Lookup {
                        id,
                        group,
                        body: operation.clone(),
                        digest,
                        status: status.clone(),
                        cancel: false,
                        sequence: 0,
                    })
                    .map_err(NodeError::Log)?;
                result = Some(status);
                return Ok(());
            }
            let access = self.account_group_access(group)?;
            if access.role != AccountGroupRole::Owner {
                return Err(NodeError::Authorization(
                    "account is not the group owner".into(),
                ));
            }
            let mut sources: Vec<_> = access
                .sources
                .into_iter()
                .filter(|source| source.role == AccountGroupRole::Owner)
                .collect();
            sources.sort_by_key(|source| {
                (
                    source.device != self.identity.public(),
                    !matches!(
                        source.execution,
                        AccountGroupExecution::ViaDevice(_) | AccountGroupExecution::Local
                    ),
                    source.device.user_id(),
                )
            });
            let executor = sources
                .into_iter()
                .next()
                .ok_or_else(|| NodeError::Channel("owner source is reconciling".into()))?;
            let ready = self.account_group_operation_ready(&executor.device);
            let reference =
                journal::OperationReference::new(self, id, group, &operation, executor.clone());
            if !self.preflight_group_operation_request(RequestProjection {
                reference: reference.clone(),
                body: Some(operation.clone()),
                cancel: false,
                nonce: self.operation_query_nonce(id),
            }) {
                return Err(NodeError::InvalidInput(
                    "account group operation transport capacity exhausted".into(),
                ));
            }
            let mut status = status;
            status.phase = if ready {
                AccountGroupOperationPhase::AwaitingAcceptance
            } else {
                AccountGroupOperationPhase::WaitingForExecutor
            };
            status.executions = vec![AccountGroupOperationExecution {
                device_id: executor.device.user_id(),
                phase: status.phase.clone(),
                committed_events: Vec::new(),
            }];
            self.group_operations
                .lock()
                .expect("group operation journal mutex")
                .put(journal::Entry {
                    reference,
                    body: Some(operation.clone()),
                    status: status.clone(),
                    source: false,
                    cancel: false,
                    sequence: 0,
                })
                .map_err(NodeError::Log)?;
            result = Some(status);
            Ok(())
        });
        // Once durable admission happened, a later adapter failure cannot report
        // a clean rejection while the same accepted account intent may execute.
        if let Some(status) = result {
            self.account_sync_notify.notify_one();
            return Ok(status);
        }
        authorized?;
        Err(NodeError::Authorization(
            "authorization did not execute admission".into(),
        ))
    }

    pub fn account_group_operation_status(
        &self,
        expected_account: &str,
        id: AccountGroupOperationId,
    ) -> Result<Option<AccountGroupOperationStatus>, NodeError> {
        self.ensure_group_operation_account(expected_account)?;
        let status = {
            let journal = self
                .group_operations
                .lock()
                .expect("group operation journal mutex");
            journal
                .entries
                .get(&id)
                .map(|entry| entry.status.clone())
                .or_else(|| journal.lookups.get(&id).map(|entry| entry.status.clone()))
        };
        if status.as_ref().is_some_and(|status| {
            !matches!(
                status.phase,
                AccountGroupOperationPhase::Committed
                    | AccountGroupOperationPhase::Cancelled
                    | AccountGroupOperationPhase::Rejected { .. }
            )
        }) {
            let mut queries = self
                .operation_queries
                .lock()
                .expect("operation query mutex");
            let query = queries.entry(id).or_insert((
                0,
                std::time::Instant::now() - std::time::Duration::from_secs(2),
            ));
            if query.1.elapsed() >= std::time::Duration::from_secs(1) {
                query.0 = query.0.wrapping_add(1);
                query.1 = std::time::Instant::now();
            }
            drop(queries);
            self.account_sync_notify.notify_one();
        }
        Ok(status)
    }
    pub fn list_account_group_operations(
        &self,
        expected_account: &str,
        query: AccountGroupOperationQuery,
    ) -> Result<Vec<AccountGroupOperationStatus>, NodeError> {
        self.ensure_group_operation_account(expected_account)?;
        let mut statuses: Vec<_> = {
            let journal = self
                .group_operations
                .lock()
                .expect("group operation journal mutex");
            journal
                .entries
                .values()
                .map(|entry| entry.status.clone())
                .chain(journal.lookups.values().map(|entry| entry.status.clone()))
                .collect()
        };
        statuses.retain(|status| {
            query.group.is_none_or(|group| status.group == group)
                && query.after.is_none_or(|after| status.id > after)
                && (!query.pending_only
                    || !matches!(
                        status.phase,
                        AccountGroupOperationPhase::Committed
                            | AccountGroupOperationPhase::Cancelled
                            | AccountGroupOperationPhase::Rejected { .. }
                    ))
        });
        statuses.sort_by_key(|status| status.id);
        statuses.truncate(query.limit.clamp(1, 64));
        Ok(statuses)
    }
    pub fn cancel_account_group_operation(
        &self,
        expected_account: &str,
        id: AccountGroupOperationId,
    ) -> Result<AccountGroupOperationStatus, NodeError> {
        self.cancel_account_group_operation_if(expected_account, id, |commit| commit())
    }
    pub fn cancel_account_group_operation_if(
        &self,
        expected_account: &str,
        id: AccountGroupOperationId,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountGroupOperationStatus, NodeError> {
        self.ensure_group_operation_account(expected_account)?;
        let mut result = None;
        let authorized = authorize(&mut || {
            // Match the host's established session→delivery→journal order.
            let mut delivery = self.delivery.lock().expect("delivery lock");
            // A recovery failure cannot make an already prepared intent look
            // cancelled. Record only the cancellation request; the source will
            // settle it under this same gate after exact prepared recovery.
            let _ = self.recover_delivery(&mut delivery);
            let mut journal = self
                .group_operations
                .lock()
                .expect("group operation journal mutex");
            if let Some(mut entry) = journal.entries.get(&id).cloned() {
                if entry.terminal() {
                    result = Some(entry.status);
                    return Ok(());
                }
                entry.cancel = true;
                entry.status.phase = AccountGroupOperationPhase::CancellationRequested;
                journal.put(entry.clone()).map_err(NodeError::Log)?;
                result = Some(entry.status);
            } else if let Some(mut lookup) = journal.lookups.get(&id).cloned() {
                lookup.cancel = true;
                lookup.status.phase = AccountGroupOperationPhase::CancellationRequested;
                journal.put_lookup(lookup.clone()).map_err(NodeError::Log)?;
                result = Some(lookup.status);
            } else {
                return Err(NodeError::InvalidInput(
                    "unknown account group operation".into(),
                ));
            }
            Ok(())
        });
        if let Some(status) = result {
            self.account_sync_notify.notify_one();
            return Ok(status);
        }
        authorized?;
        Err(NodeError::Authorization(
            "authorization did not execute cancellation".into(),
        ))
    }
    pub(in crate::node) fn operation_projection(
        &self,
        group: crate::eventlog::ConversationId,
    ) -> OperationProjection {
        let journal = self
            .group_operations
            .lock()
            .expect("operation journal mutex");
        let mut entries: Vec<_> = journal
            .entries
            .values()
            .filter(|entry| entry.reference.group == group)
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.sequence));
        let requests = entries
            .iter()
            .filter(|entry| !entry.source && !entry.terminal())
            .take(8)
            .map(|entry| RequestProjection {
                reference: entry.reference.clone(),
                body: entry.body.clone(),
                cancel: entry.cancel,
                nonce: self.operation_query_nonce(entry.reference.id),
            })
            .collect();
        let mut results: Vec<_> = entries.iter().filter(|entry| entry.source).collect();
        // A status query touches the original source journal entry. Its exact
        // terminal result must remain publishable while other work is pending.
        results.sort_by_key(|entry| std::cmp::Reverse(entry.sequence));
        let results = results
            .into_iter()
            .take(16)
            .map(|entry| ResultProjection {
                reference: entry.reference.clone(),
                status: entry.status.clone(),
            })
            .collect();
        let lookups = journal
            .lookups
            .values()
            .filter(|entry| entry.group == group)
            .take(8)
            .map(|entry| LookupProjection {
                id: entry.id,
                group,
                kind: entry.body.kind(),
                digest: entry.digest,
                cancel: entry.cancel,
                nonce: self.operation_query_nonce(entry.id),
            })
            .collect();
        OperationProjection {
            requests,
            results,
            lookups,
        }
    }
    pub(in crate::node) fn operation_groups(&self) -> Vec<[u8; 32]> {
        let journal = self
            .group_operations
            .lock()
            .expect("group operation journal mutex");
        let mut groups = std::collections::BTreeMap::<[u8; 32], u64>::new();
        for entry in journal.entries.values() {
            let seq = groups.entry(*entry.reference.group.as_bytes()).or_default();
            *seq = (*seq).max(entry.sequence);
        }
        for lookup in journal.lookups.values() {
            let seq = groups.entry(*lookup.group.as_bytes()).or_default();
            *seq = (*seq).max(lookup.sequence);
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by_key(|(group, seq)| (std::cmp::Reverse(*seq), *group));
        groups.into_iter().map(|(group, _)| group).collect()
    }
    pub(in crate::node) fn operation_query_nonce(&self, id: AccountGroupOperationId) -> [u8; 16] {
        let queries = self
            .operation_queries
            .lock()
            .expect("operation query mutex");
        let counter = queries.get(&id).map_or(0, |(counter, _)| *counter);
        let mut nonce = [0; 16];
        nonce[..8].copy_from_slice(&self.operation_epoch.to_be_bytes());
        nonce[8..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }
    fn account_group_operation_ready(&self, device: &PublicIdentity) -> bool {
        device == &self.identity.public()
            || self
                .group_operation_versions
                .lock()
                .expect("operation versions mutex")
                .get(&device.user_id())
                .is_some_and(|support| {
                    support.public == *device
                        && support.observed_at.elapsed() < std::time::Duration::from_secs(60)
                })
    }
}
