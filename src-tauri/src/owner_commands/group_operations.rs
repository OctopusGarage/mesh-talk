//! Owner-scoped adapters for account-owned group operations.
use super::{parse_id, validate_account};
use crate::{
    chat_commands::{NodeState, parse_channel_id},
    commands::CommandError,
    runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::{
    channel::HistoryPolicy,
    node::{
        AccountGroupOperation, AccountGroupOperationId, AccountGroupOperationKind,
        AccountGroupOperationPhase, AccountGroupOperationQuery, AccountGroupOperationStatus,
        AccountGroupStickerInput, AccountGroupTextInput, MessageProtection,
    },
};
use serde::{Deserialize, Serialize};

/// Message and file bodies are prepared by their existing owners, not deserialized here.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountGroupControlRequest {
    Rename {
        name: String,
    },
    Invite {
        target_account_id: String,
    },
    RemoveAccount {
        target_account_id: String,
    },
    TransferOwnership {
        target_account_id: String,
    },
    SetHistoryPolicy {
        policy: HistoryPolicy,
    },
    AcceptInvitation,
    DeclineInvitation,
    LeaveAccount,
    Dissolve,
    React {
        target: String,
        emoji: String,
        remove: bool,
    },
    Recall {
        target: String,
    },
}

impl AccountGroupControlRequest {
    fn into_core(self) -> Result<AccountGroupOperation, CommandError> {
        Ok(match self {
            Self::Rename { name } => AccountGroupOperation::Rename { name },
            Self::Invite { target_account_id } => {
                validate_account(&target_account_id)?;
                AccountGroupOperation::Invite { target_account_id }
            }
            Self::RemoveAccount { target_account_id } => {
                validate_account(&target_account_id)?;
                AccountGroupOperation::RemoveAccount { target_account_id }
            }
            Self::TransferOwnership { target_account_id } => {
                validate_account(&target_account_id)?;
                AccountGroupOperation::TransferOwnership { target_account_id }
            }
            Self::SetHistoryPolicy { policy } => AccountGroupOperation::SetHistoryPolicy { policy },
            Self::AcceptInvitation => AccountGroupOperation::AcceptInvitation,
            Self::DeclineInvitation => AccountGroupOperation::DeclineInvitation,
            Self::LeaveAccount => AccountGroupOperation::LeaveAccount,
            Self::Dissolve => AccountGroupOperation::Dissolve,
            Self::React {
                target,
                emoji,
                remove,
            } => AccountGroupOperation::React {
                target: parse_id(&target)?,
                emoji,
                remove,
            },
            Self::Recall { target } => AccountGroupOperation::Recall {
                target: parse_id(&target)?,
            },
        })
    }
}

#[derive(Serialize)]
pub struct AccountGroupOperationExecutionInfo {
    device_id: String,
    phase: AccountGroupOperationPhase,
    committed_events: Vec<String>,
}

/// IDs are explicit hex strings; no request payload is exposed by lifecycle reads.
#[derive(Serialize)]
pub struct AccountGroupOperationInfo {
    id: String,
    account_id: String,
    group: String,
    kind: AccountGroupOperationKind,
    phase: AccountGroupOperationPhase,
    executions: Vec<AccountGroupOperationExecutionInfo>,
    committed_events: Vec<String>,
    primary_event: Option<String>,
}

impl From<AccountGroupOperationStatus> for AccountGroupOperationInfo {
    fn from(status: AccountGroupOperationStatus) -> Self {
        Self {
            id: status.id.to_hex(),
            account_id: status.account_id,
            group: hex::encode(status.group.as_bytes()),
            kind: status.kind,
            phase: status.phase,
            executions: status
                .executions
                .into_iter()
                .map(|execution| AccountGroupOperationExecutionInfo {
                    device_id: execution.device_id,
                    phase: execution.phase,
                    committed_events: execution
                        .committed_events
                        .into_iter()
                        .map(|id| hex::encode(id.as_bytes()))
                        .collect(),
                })
                .collect(),
            committed_events: status
                .committed_events
                .into_iter()
                .map(|id| hex::encode(id.as_bytes()))
                .collect(),
            primary_event: status.primary_event.map(|id| hex::encode(id.as_bytes())),
        }
    }
}

#[tauri::command]
pub async fn owner_create_channel(
    owner: String,
    expected_account: String,
    name: String,
    member_account_ids: Vec<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let handle = admission.checked(runtime, || {
                    if runtime.account_id() != expected_account {
                        return Err(CommandError::Authorization("account changed".into()));
                    }
                    Ok(runtime.handle())
                })?;
                let id = handle
                    .create_account_channel_invited_if(&name, member_account_ids, |commit| {
                        admission.authorize_accept(runtime, commit)
                    })
                    .await
                    .map_err(CommandError::from)?;
                Ok(hex::encode(id.as_bytes()))
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_new_account_group_operation_id(
    owner: String,
    expected_account: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .with_runtime(|runtime| {
            runtime
                .handle()
                .new_account_group_operation_id(&expected_account)
                .map(|id| id.to_hex())
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn owner_submit_account_group_control(
    owner: String,
    expected_account: String,
    operation_id: String,
    channel_id: String,
    operation: AccountGroupControlRequest,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountGroupOperationInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = AccountGroupOperationId::from_hex(&operation_id)?;
                let group = parse_channel_id(&channel_id)?;
                let operation = operation.into_core()?;
                runtime
                    .handle()
                    .submit_account_group_operation_if(
                        &expected_account,
                        id,
                        group,
                        operation,
                        |commit| admission.authorize_accept(runtime, commit),
                    )
                    .map(Into::into)
                    .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_submit_account_group_text(
    owner: String,
    expected_account: String,
    operation_id: String,
    channel_id: String,
    text: String,
    reply_to: Option<String>,
    expires_at_ms: Option<u64>,
    protection: Option<MessageProtection>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountGroupOperationInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = AccountGroupOperationId::from_hex(&operation_id)?;
                let group = parse_channel_id(&channel_id)?;
                let reply_to = reply_to.as_deref().map(parse_id).transpose()?;
                runtime
                    .handle()
                    .submit_account_group_text_if(
                        &expected_account,
                        id,
                        group,
                        AccountGroupTextInput {
                            text: text.into_bytes(),
                            reply_to,
                            expires_at_ms,
                            protection,
                        },
                        |commit| admission.authorize_accept(runtime, commit),
                    )
                    .map(Into::into)
                    .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_submit_account_group_file<R: tauri::Runtime>(
    owner: String,
    expected_account: String,
    operation_id: String,
    channel_id: String,
    path: String,
    media: bool,
    progress_key: Option<String>,
    expires_at_ms: Option<u64>,
    protection: Option<MessageProtection>,
    handle: tauri::AppHandle<R>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountGroupOperationInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let cleanup = handle.clone();
    let mut progress = crate::events::ProgressThrottle::new(
        handle,
        progress_key.unwrap_or_else(|| operation_id.clone()),
        "prepare",
    );
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                admission.checked(runtime, || {
                    if runtime.account_id() != expected_account {
                        return Err(CommandError::Authorization("account changed".into()));
                    }
                    Ok(())
                })?;
                let id = AccountGroupOperationId::from_hex(&operation_id)?;
                let group = parse_channel_id(&channel_id)?;
                let kind = if media {
                    mesh_talk_core::file::FileKind::Media
                } else {
                    mesh_talk_core::file::FileKind::File
                };
                let status = runtime
                    .handle()
                    .submit_account_group_file_if(
                        &expected_account,
                        id,
                        group,
                        std::path::Path::new(&path),
                        kind,
                        expires_at_ms,
                        protection,
                        move |p| progress.emit(p.done, p.total),
                        |commit| admission.authorize_accept(runtime, commit),
                    )
                    .await
                    .map_err(CommandError::from)?;
                // Pending and uncertain submissions keep the original managed path for retry.
                if matches!(
                    status.phase,
                    AccountGroupOperationPhase::Committed
                        | AccountGroupOperationPhase::Cancelled
                        | AccountGroupOperationPhase::Rejected { .. }
                ) {
                    crate::chat_commands::cleanup_pasted_file(&cleanup, &path);
                }
                Ok(status.into())
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_submit_account_group_sticker(
    owner: String,
    expected_account: String,
    operation_id: String,
    channel_id: String,
    sticker_id: String,
    fallback: String,
    expires_at_ms: Option<u64>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountGroupOperationInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = AccountGroupOperationId::from_hex(&operation_id)?;
                let group = parse_channel_id(&channel_id)?;
                runtime
                    .handle()
                    .submit_account_group_sticker_if(
                        &expected_account,
                        id,
                        group,
                        AccountGroupStickerInput {
                            sticker_id,
                            fallback: fallback.into_bytes(),
                            expires_at_ms,
                        },
                        |commit| admission.authorize_accept(runtime, commit),
                    )
                    .map(Into::into)
                    .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_account_group_operation_status(
    owner: String,
    expected_account: String,
    operation_id: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Option<AccountGroupOperationInfo>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .with_runtime(|runtime| {
            let id = AccountGroupOperationId::from_hex(&operation_id)?;
            runtime
                .handle()
                .account_group_operation_status(&expected_account, id)
                .map(|status| status.map(Into::into))
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn owner_list_account_group_operations(
    owner: String,
    expected_account: String,
    channel_id: Option<String>,
    pending_only: bool,
    after: Option<String>,
    limit: usize,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Vec<AccountGroupOperationInfo>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .with_runtime(|runtime| {
            if !(1..=100).contains(&limit) {
                return Err(CommandError::InvalidInput(
                    "operation list limit must be between 1 and 100".into(),
                ));
            }
            let query = AccountGroupOperationQuery {
                group: channel_id.as_deref().map(parse_channel_id).transpose()?,
                pending_only,
                after: after
                    .as_deref()
                    .map(AccountGroupOperationId::from_hex)
                    .transpose()?,
                limit,
            };
            runtime
                .handle()
                .list_account_group_operations(&expected_account, query)
                .map(|rows| rows.into_iter().map(Into::into).collect())
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn owner_cancel_account_group_operation(
    owner: String,
    expected_account: String,
    operation_id: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountGroupOperationInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = AccountGroupOperationId::from_hex(&operation_id)?;
                runtime
                    .handle()
                    .cancel_account_group_operation_if(&expected_account, id, |commit| {
                        admission.authorize_accept(runtime, commit)
                    })
                    .map(Into::into)
                    .map_err(CommandError::from)
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::eventlog::{ConversationId, EventId};

    #[test]
    fn lifecycle_projection_keeps_operation_and_public_event_ids_distinct() {
        let operation = AccountGroupOperationId::new([1; 32]);
        let event = EventId::new([2; 32]);
        let group = ConversationId::new([3; 32]);
        let info = AccountGroupOperationInfo::from(AccountGroupOperationStatus {
            id: operation,
            account_id: "ab".repeat(16),
            group,
            kind: AccountGroupOperationKind::Rename,
            phase: AccountGroupOperationPhase::Committed,
            executions: vec![mesh_talk_core::node::AccountGroupOperationExecution {
                device_id: "cd".repeat(16),
                phase: AccountGroupOperationPhase::Committed,
                committed_events: vec![event],
            }],
            committed_events: vec![event],
            primary_event: None,
        });
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({
                "id":"01".repeat(32), "account_id":"ab".repeat(16), "group":"03".repeat(32),
                "kind":"rename", "phase":{"kind":"committed"},
                "executions":[{"device_id":"cd".repeat(16), "phase":{"kind":"committed"}, "committed_events":["02".repeat(32)]}],
                "committed_events":["02".repeat(32)], "primary_event":null,
            })
        );
    }

    #[test]
    fn control_adapter_rejects_unprepared_messages_and_unrelated_payloads() {
        for request in [
            serde_json::json!({"kind":"send", "message":{"kind":"text", "encoded_body":[1,2]}}),
            serde_json::json!({"kind":"rename", "name":"Group", "path":"/remote/file"}),
            serde_json::json!({"kind":"recall", "target":"not-an-event-id"}),
            serde_json::json!({"kind":"transfer_ownership", "target_account_id":"not-an-account"}),
        ] {
            assert!(
                serde_json::from_value::<AccountGroupControlRequest>(request)
                    .map_err(|error| CommandError::InvalidInput(error.to_string()))
                    .and_then(AccountGroupControlRequest::into_core)
                    .is_err()
            );
        }
    }
}
