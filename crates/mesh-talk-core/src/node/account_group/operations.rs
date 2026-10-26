//! Account-owned operation admission and body-free lifecycle queries.
mod action_preparation;
pub(in crate::node) use action_preparation::persist_action_recall_completion;
mod execution;
pub(in crate::node) mod file_preparation;
mod file_work;
mod legacy_leave;
pub(in crate::node) mod legacy_wire;
mod message_copy;
mod preparation;
mod source_work;
pub use preparation::{AccountGroupStickerInput, AccountGroupTextInput};
pub(in crate::node) mod journal;
pub(in crate::node) mod wire;
use super::*;
use crate::node::channels::{
    account_self_control::AccountSelfControlInput, owner_control::OwnerControlInput,
};
use crate::{
    channel::HistoryPolicy,
    node::{
        Node, NodeError,
        channels::{SimpleControl, normalize_simple_name},
    },
};
pub(in crate::node) use wire::valid_payload;
use wire::{LookupProjection, OperationProjection, RequestProjection, ResultProjection};

#[cfg(test)]
type LookupAdmissionHook = Box<dyn FnOnce(&Node) + Send>;
#[cfg(test)]
static LOOKUP_ADMISSION_HOOKS: std::sync::Mutex<
    Vec<(AccountGroupOperationId, LookupAdmissionHook)>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(in crate::node) fn before_lookup_commit_for_test(
    id: AccountGroupOperationId,
    hook: impl FnOnce(&Node) + Send + 'static,
) {
    LOOKUP_ADMISSION_HOOKS
        .lock()
        .unwrap()
        .push((id, Box::new(hook)));
}

#[cfg(test)]
pub(in crate::node) fn finish_imported_result_for_test(
    node: &Node,
    result: ResultProjection,
) -> Result<(), NodeError> {
    node.group_operations
        .lock()
        .unwrap()
        .adopt(journal::Entry {
            reference: result.reference,
            body: None,
            status: result.status,
            file_copy: result
                .file_descriptor
                .map(|descriptor| journal::GroupFileCopy {
                    descriptor,
                    complete: false,
                }),
            source: false,
            cancel: false,
            sequence: 0,
        })
        .map_err(NodeError::Log)
}

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
    fn message_action_input(&self) -> Option<crate::node::message_actions::MessageActionInput> {
        use crate::node::message_actions::MessageActionInput;
        match self {
            Self::React {
                target,
                emoji,
                remove,
            } => Some(MessageActionInput::React {
                target: *target,
                emoji: emoji.clone(),
                remove: *remove,
            }),
            Self::Recall { target } => Some(MessageActionInput::Recall { target: *target }),
            _ => None,
        }
    }
    fn invitation_input(
        &self,
    ) -> Option<crate::node::channels::account_invitation::InvitationInput> {
        match self {
            Self::Invite { target_account_id } => {
                Some(crate::node::channels::account_invitation::InvitationInput {
                    target_account_id: target_account_id.clone(),
                })
            }
            _ => None,
        }
    }
    fn self_control_input(&self) -> Option<AccountSelfControlInput> {
        match self {
            Self::AcceptInvitation => Some(AccountSelfControlInput::Accept),
            Self::DeclineInvitation => Some(AccountSelfControlInput::Decline),
            Self::LeaveAccount => Some(AccountSelfControlInput::Leave),
            _ => None,
        }
    }
    fn owner_control_input(&self) -> Option<OwnerControlInput> {
        match self {
            Self::RemoveAccount { target_account_id } => {
                Some(OwnerControlInput::RemoveAccount(target_account_id.clone()))
            }
            Self::TransferOwnership { target_account_id } => {
                Some(OwnerControlInput::Transfer(target_account_id.clone()))
            }
            _ => None,
        }
    }
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
    fn valid_execution_payload(&self) -> bool {
        if let Some(input) = self.invitation_input() {
            return input.canonical();
        }
        if self.self_control_input().is_some() {
            return true;
        }
        if self.message_action_input().is_some() {
            return serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= wire::MAX_BODY);
        }
        if let Self::Send { message } = self {
            if message.kind == AccountGroupOperationKind::File {
                return file_preparation::GroupFilePayload::decode(message.encoded_body())
                    .is_some_and(|payload| message.staged_assets == [payload.logical.checksum])
                    && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= wire::MAX_BODY);
            }
            return message.staged_assets.is_empty()
                && PreparedAccountGroupMessage::from_plaintext(
                    message.kind,
                    message.encoded_body.clone(),
                )
                .is_some();
        }
        self.owner_control_input()
            .map_or_else(|| self.valid_simple_control(), |input| input.canonical())
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
impl AccountGroupOperationKind {
    /// Stable capability positions are independent of serde's enum layout.
    pub(in crate::node) const fn capability_bit(self) -> u16 {
        1 << match self {
            Self::Text => 0,
            Self::Sticker => 1,
            Self::File => 2,
            Self::Reaction => 3,
            Self::Recall => 4,
            Self::Invite => 5,
            Self::AcceptInvitation => 6,
            Self::DeclineInvitation => 7,
            Self::Rename => 8,
            Self::RemoveAccount => 9,
            Self::TransferOwnership => 10,
            Self::Dissolve => 11,
            Self::HistoryPolicy => 12,
            Self::LeaveAccount => 13,
        }
    }

    /// An older operation-v1 completion advertised exactly these controls.
    pub(in crate::node) const fn legacy_capabilities() -> u16 {
        Self::Rename.capability_bit()
            | Self::HistoryPolicy.capability_bit()
            | Self::Dissolve.capability_bit()
    }

    pub(in crate::node) const fn supported_here(self) -> bool {
        matches!(
            self,
            Self::Text
                | Self::Sticker
                | Self::File
                | Self::Rename
                | Self::HistoryPolicy
                | Self::Dissolve
                | Self::RemoveAccount
                | Self::TransferOwnership
                | Self::Reaction
                | Self::Recall
                | Self::Invite
                | Self::AcceptInvitation
                | Self::DeclineInvitation
                | Self::LeaveAccount
        )
    }

    pub(in crate::node) fn admits_role(self, role: AccountGroupRole) -> bool {
        match self {
            Self::Text | Self::Sticker | Self::File | Self::Reaction | Self::Recall => {
                matches!(role, AccountGroupRole::Member | AccountGroupRole::Owner)
            }
            Self::AcceptInvitation | Self::DeclineInvitation => role == AccountGroupRole::Invited,
            Self::LeaveAccount => role == AccountGroupRole::Member,
            Self::Invite
            | Self::Rename
            | Self::RemoveAccount
            | Self::TransferOwnership
            | Self::Dissolve
            | Self::HistoryPolicy => role == AccountGroupRole::Owner,
        }
    }
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
            operation @ (AccountGroupOperation::Send { .. }
            | AccountGroupOperation::React { .. }
            | AccountGroupOperation::Recall { .. }
            | AccountGroupOperation::Invite { .. }
            | AccountGroupOperation::RemoveAccount { .. }
            | AccountGroupOperation::TransferOwnership { .. }
            | AccountGroupOperation::AcceptInvitation
            | AccountGroupOperation::DeclineInvitation
            | AccountGroupOperation::LeaveAccount)
                if operation.valid_execution_payload() =>
            {
                operation
            }
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
            if id.matches_origin(&self.identity.public())
                && let AccountGroupOperation::Send { message } = &operation
            {
                message.validate_new_admission()?;
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
                let lookup_fits = self.preflight_group_operation_lookup(LookupProjection {
                    id,
                    group,
                    kind: operation.kind(),
                    digest,
                    cancel: false,
                    nonce: self.operation_query_nonce(id),
                });
                #[cfg(test)]
                {
                    let hook = {
                        let mut hooks = LOOKUP_ADMISSION_HOOKS.lock().unwrap();
                        hooks
                            .iter()
                            .position(|(operation, _)| *operation == id)
                            .map(|index| hooks.remove(index).1)
                    };
                    if let Some(hook) = hook {
                        hook(self);
                    }
                }
                let mut journal = self
                    .group_operations
                    .lock()
                    .expect("group operation journal mutex");
                // Result import finishes outside the delivery gate. It may have
                // adopted the original request since the first journal read;
                // resolve that identity atomically instead of replacing it.
                let existing = journal
                    .entries
                    .get(&id)
                    .map(|entry| (entry.reference.group, entry.reference.digest, &entry.status))
                    .or_else(|| {
                        journal
                            .lookups
                            .get(&id)
                            .map(|lookup| (lookup.group, lookup.digest, &lookup.status))
                    });
                if let Some((old_group, old_digest, status)) = existing {
                    if old_group != group || old_digest != digest {
                        return Err(NodeError::InvalidInput(
                            "operation ID belongs to another request".into(),
                        ));
                    }
                    result = Some(status.clone());
                    return Ok(());
                }
                if !lookup_fits {
                    return Err(NodeError::InvalidInput(
                        "account group operation transport capacity exhausted".into(),
                    ));
                }
                journal
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
            if !operation.kind().admits_role(access.role) {
                return Err(NodeError::Authorization(
                    "account is not authorized for this group operation".into(),
                ));
            }
            let mut sources: Vec<_> = access
                .sources
                .into_iter()
                .filter(|source| operation.kind().admits_role(source.role))
                .collect();
            if operation.self_control_input().is_some()
                && !matches!(operation, AccountGroupOperation::LeaveAccount)
            {
                sources.retain(|source| source.authority.authority.is_some());
                if sources.is_empty() {
                    // Nothing has entered the journal. The host must report a
                    // definite rejection, not keep observing an absent intent.
                    return Err(NodeError::InvalidInput(
                        "group account-membership format update required for this operation".into(),
                    ));
                }
            }
            if let AccountGroupOperation::Recall { target } = &operation {
                let author = self
                    .account_group_original_message_author(group, *target)
                    .ok_or_else(|| {
                        NodeError::Channel("original message author is reconciling".into())
                    })?;
                if author.ed25519_pub() == &self.identity.public().ed25519_pub {
                    self.account_group_action_target(
                        group,
                        &crate::node::message_actions::MessageActionInput::Recall {
                            target: *target,
                        },
                    )?;
                }
                sources.retain(|source| &source.device.ed25519_pub == author.ed25519_pub());
            }
            sources.sort_by_key(|source| {
                (
                    !self.account_group_source_ready_for(source, operation.kind()),
                    source.device != self.identity.public(),
                    source.device.user_id(),
                )
            });
            let executor = sources
                .into_iter()
                .next()
                .ok_or_else(|| NodeError::Channel("owner source is reconciling".into()))?;
            let ready = self.account_group_source_ready_for(&executor, operation.kind());
            let reference =
                journal::OperationReference::new(self, id, group, &operation, executor.clone());
            if let AccountGroupOperation::Send { message } = &operation
                && message.kind == AccountGroupOperationKind::File
            {
                let payload = file_preparation::GroupFilePayload::decode(message.encoded_body())
                    .ok_or_else(|| {
                        NodeError::InvalidInput("invalid original File intent".into())
                    })?;
                if payload
                    .descriptor
                    .as_ref()
                    .is_none_or(|descriptor| !descriptor.valid_for(&reference, &payload.logical))
                {
                    return Err(NodeError::InvalidInput(
                        "invalid original File preparation".into(),
                    ));
                }
            }

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
                    file_copy: None,
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
                .filter(|_| !journal.legacy_children.contains_key(&id))
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
            let advanced = query.1.elapsed() >= std::time::Duration::from_secs(1);
            if advanced {
                query.0 = query.0.wrapping_add(1);
                query.1 = std::time::Instant::now();
            }
            drop(queries);
            // Polling the same pending query cannot create fresh reconciliation
            // work; only an advanced retry nonce needs another sync turn.
            if advanced {
                self.account_sync_notify.notify_one();
            }
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
                .filter(|entry| !journal.legacy_children.contains_key(&entry.reference.id))
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
        // Never retain the journal while querying capability or public-log owners.
        let (pending, results, lookups) = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            let mut entries: Vec<_> = journal
                .entries
                .values()
                .filter(|entry| {
                    entry.reference.group == group
                        && !journal.legacy_children.contains_key(&entry.reference.id)
                })
                .collect();
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.sequence));
            // The journal admits at most 128 pending intents. Do not clone all
            // retained terminal history or unrelated plaintext for a wire page.
            let pending: Vec<_> = entries
                .iter()
                .filter(|entry| !entry.source && !entry.terminal())
                .take(128)
                .map(|entry| (*entry).clone())
                .collect();
            let results: Vec<_> = entries
                .iter()
                .filter(|entry| entry.source)
                .take(16)
                .map(|entry| {
                    (
                        entry.reference.clone(),
                        entry.status.clone(),
                        entry.file_copy.as_ref().map(|copy| copy.descriptor.clone()),
                    )
                })
                .collect();
            let lookups: Vec<_> = journal
                .lookups
                .values()
                .filter(|lookup| lookup.group == group)
                .take(8)
                .map(|lookup| {
                    (
                        lookup.id,
                        lookup.group,
                        lookup.body.kind(),
                        lookup.digest,
                        lookup.cancel,
                    )
                })
                .collect();
            (pending, results, lookups)
        };
        let requests = pending
            .iter()
            .filter_map(|entry| {
                let ready = self.account_group_operation_ready_for(
                    &entry.reference.executor.device,
                    entry.reference.kind,
                ) && (entry.reference.kind != AccountGroupOperationKind::LeaveAccount
                    || entry.reference.executor.authority.authority.is_some()
                    || self.legacy_group_leave_ready(&entry.reference.executor.device));
                (ready || entry.cancel).then(|| RequestProjection {
                    reference: entry.reference.clone(),
                    body: if ready { entry.body.clone() } else { None },
                    cancel: entry.cancel,
                    nonce: self.operation_query_nonce(entry.reference.id),
                })
            })
            .take(8)
            .collect();
        let results = results
            .into_iter()
            .map(|(reference, status, file_descriptor)| {
                let mut message_event =
                    self.account_group_committed_message_event(&reference, &status);
                let file_descriptor = if reference.kind == AccountGroupOperationKind::File {
                    match (message_event.as_ref(), file_descriptor) {
                        (Some(_), Some(descriptor)) => Some(descriptor),
                        _ => {
                            message_event = None;
                            None
                        }
                    }
                } else {
                    file_descriptor
                };
                let recall_mark = message_event
                    .as_ref()
                    .and_then(|event| self.account_group_committed_recall_mark(&reference, event));
                ResultProjection {
                    file_descriptor,
                    recall_mark,
                    reference,
                    status,
                    message_event,
                }
            })
            .collect();
        let lookups = lookups
            .iter()
            .take(8)
            .map(|(id, group, kind, digest, cancel)| LookupProjection {
                id: *id,
                group: *group,
                kind: *kind,
                digest: *digest,
                cancel: *cancel,
                nonce: self.operation_query_nonce(*id),
            })
            .collect();
        let mut projection = OperationProjection {
            requests,
            results,
            lookups,
        };
        // A touched source result is first; older terminal results remain in the
        // durable journal and can be republished by their original status query.
        // Reuse the existing frame admission allowance for its bounded envelope.
        while !projection.results.is_empty()
            && wire::OperationFrame::new(self, group, u64::MAX, projection.clone())
                .encode()
                .len()
                .saturating_add(512)
                > wire::MAX_HEAD
        {
            projection.results.pop();
        }
        projection
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
    fn account_group_source_ready_for(
        &self,
        source: &AccountGroupSource,
        kind: AccountGroupOperationKind,
    ) -> bool {
        matches!(
            source.execution,
            AccountGroupExecution::Local | AccountGroupExecution::ViaDevice(_)
        ) && self.account_group_operation_ready_for(&source.device, kind)
            && (kind != AccountGroupOperationKind::LeaveAccount
                || source.authority.authority.is_some()
                || self.legacy_group_leave_ready(&source.device))
    }

    pub(in crate::node) fn legacy_group_leave_ready(&self, device: &PublicIdentity) -> bool {
        device == &self.identity.public()
            || self
                .group_operation_versions
                .lock()
                .expect("operation versions mutex")
                .get(&device.user_id())
                .is_some_and(|support| {
                    support.public == *device
                        && support.kinds & legacy_wire::LEGACY_LEAVE_CAPABILITY != 0
                        && support.observed_at.elapsed() < std::time::Duration::from_secs(60)
                })
    }

    pub(in crate::node) fn account_group_operation_ready_for(
        &self,
        device: &PublicIdentity,
        kind: AccountGroupOperationKind,
    ) -> bool {
        device == &self.identity.public() && kind.supported_here()
            || self
                .group_operation_versions
                .lock()
                .expect("operation versions mutex")
                .get(&device.user_id())
                .is_some_and(|support| {
                    support.public == *device
                        && support.kinds & kind.capability_bit() != 0
                        && support.observed_at.elapsed() < std::time::Duration::from_secs(60)
                })
    }
}
