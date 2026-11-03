use super::{
    ChannelMemberInfo, ChannelMembersInfo, HistoryItem, NodeState, parse_channel_id, parse_event_id,
};
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission, state::AppState};
use serde::Serialize;
use tauri::Manager;

/// Account permission and physical execution availability have separate meanings.
#[derive(Serialize)]
pub struct AccountGroupAccessInfo {
    account_id: String,
    role: mesh_talk_core::node::AccountGroupRole,
    execution: AccountGroupExecutionInfo,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AccountGroupExecutionInfo {
    Local,
    ViaDevice { device_id: String },
    WaitingForDevice,
    RequiresUpgrade,
    Reconciling,
}

impl AccountGroupAccessInfo {
    fn read(
        node: &mesh_talk_core::node::Node,
        group: mesh_talk_core::eventlog::ConversationId,
    ) -> Self {
        use mesh_talk_core::node::{AccountGroupExecution, AccountGroupRole};
        let Ok(access) = node.account_group_access(group) else {
            // A failed observation must not fall back to cached positive device
            // authority. Other groups remain available while this one reconciles.
            return Self {
                account_id: node.account_id(),
                role: AccountGroupRole::Unknown,
                execution: AccountGroupExecutionInfo::Reconciling,
            };
        };
        let execution = match access.execution {
            AccountGroupExecution::Local => AccountGroupExecutionInfo::Local,
            AccountGroupExecution::ViaDevice(device) => AccountGroupExecutionInfo::ViaDevice {
                device_id: device.user_id(),
            },
            AccountGroupExecution::WaitingForDevice => AccountGroupExecutionInfo::WaitingForDevice,
            AccountGroupExecution::RequiresUpgrade => AccountGroupExecutionInfo::RequiresUpgrade,
            AccountGroupExecution::Reconciling => AccountGroupExecutionInfo::Reconciling,
        };
        Self {
            account_id: access.account_id,
            role: access.role,
            execution,
        }
    }
}

/// A channel as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelInfo {
    pub metadata: Option<mesh_talk_core::node::ChannelMetadata>,
    pub channel_id: String, // hex
    pub name: String,
    pub member_count: usize,
    /// Legacy owner device identifier, retained for provenance and compatibility.
    /// Management controls use the verified `can_manage` result below.
    pub owner: String,
    /// Account authority when upgraded; the legacy device owner remains above.
    pub owner_account_id: Option<String>,
    /// Verified for this running device, rather than inferred from UI presence.
    pub can_manage: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_access: Option<AccountGroupAccessInfo>,
    /// Retained local history whose modern membership belongs to another account.
    pub previous_account: Option<String>,
    /// Account-private display reference without a local membership/key grant.
    pub shared_reference: bool,
    pub legacy_membership: bool,
    pub reference_members_complete: bool,
    /// A retained descriptive snapshot is awaiting causal reconciliation.
    pub directory_reconciling: bool,
    pub archived: bool,
    pub dissolved: bool,
    pub left: bool,
    pub pending_invite: bool,
    pub recovery: Option<mesh_talk_core::node::ChannelRecovery>,
}

#[tauri::command]
pub async fn list_channels(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<ChannelInfo>, CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .blocking(move |rt, _| {
            let node = rt.handle();
            rt.list_channels()?
                .into_iter()
                .filter(|channel| {
                    let personal = node.conversation_personal_state(
                        &mesh_talk_core::node::DocumentConversation::Channel(hex::encode(
                            channel.id.as_bytes(),
                        )),
                    );
                    !(personal.hidden && personal.local_cleanup_complete)
                })
                .map(|c| {
                    let reference = node.channel_shared_reference(c.id);
                    Ok(ChannelInfo {
                        metadata: node.channel_metadata(c.id).ok().map(|mut metadata| {
                            // The summary serves recent publication cards; detailed IPC retains the bounded history.
                            let remove = metadata.announcement_history.len().saturating_sub(20);
                            metadata.announcement_history.drain(..remove);
                            metadata
                        }),
                        channel_id: hex::encode(c.id.as_bytes()),
                        name: c.name,
                        member_count: c.member_count,
                        owner: c.owner,
                        owner_account_id: reference
                            .as_ref()
                            .and_then(|r| r.owner_account_id.clone())
                            .or_else(|| node.channel_owner_account_id(c.id)),
                        can_manage: reference.is_none() && node.channel_can_manage(c.id)?,
                        account_access: Some(AccountGroupAccessInfo::read(&node, c.id)),
                        previous_account: node.channel_previous_account(c.id)?,
                        shared_reference: reference.is_some(),
                        reference_members_complete: reference
                            .as_ref()
                            .is_none_or(|reference| reference.members_complete),
                        directory_reconciling: reference
                            .as_ref()
                            .is_some_and(|r| r.directory_reconciling),
                        legacy_membership: reference.as_ref().map_or_else(
                            || node.channel_legacy_membership(c.id),
                            |reference| reference.legacy_membership,
                        ),
                        archived: c.archived,
                        dissolved: c.dissolved,
                        left: c.left,
                        pending_invite: c.pending_invite,
                        recovery: rt.channel_recovery(c.id),
                    })
                })
                .collect()
        })
        .await
}

struct ChannelMemberFact {
    public: mesh_talk_core::identity::device::PublicIdentity,
    account_id: Option<String>,
    pending: bool,
    replica_enabled: bool,
    reference_name: Option<String>,
    post_office: bool,
}

/// Both actual and borrowed legacy rosters use the same display population as
/// the core summary count. Infrastructure facts remain outside authorization.
fn display_member_facts(
    legacy_membership: bool,
    mut facts: Vec<ChannelMemberFact>,
) -> Vec<ChannelMemberFact> {
    if legacy_membership {
        facts.retain(|member| !member.post_office);
    }
    facts
}

#[tauri::command]
pub async fn channel_members(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<ChannelMembersInfo, CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .with_runtime(move |rt| {
            let channel = parse_channel_id(&channel_id)?;
            let node = rt.handle();
            let reference = node.channel_shared_reference(channel);
            let own_account = rt.account_id();
            let known_names = rt.known_account_names()?;
            let pending: std::collections::HashSet<_> =
                rt.channel_pending_members(channel).into_iter().collect();
            // Borrowed facts are projected here only. Core member queries and every
            // admission/sender-key path continue to use the actual local group book.
            let facts: Vec<_> = if let Some(reference) = &reference {
                reference
                    .members
                    .iter()
                    .map(|member| ChannelMemberFact {
                        public: member.public.clone(),
                        account_id: member.account_id.clone(),
                        pending: member.pending,
                        replica_enabled: false,
                        reference_name: member.name.clone(),
                        post_office: member.post_office,
                    })
                    .collect()
            } else {
                rt.channel_members(channel)
                    .into_iter()
                    .map(|public| {
                        let account = rt.channel_member_account_binding(channel, &public);
                        let user = public.user_id();
                        ChannelMemberFact {
                            post_office: node.device_is_post_office(&public),
                            public,
                            account_id: account,
                            pending: pending.contains(&user),
                            replica_enabled: rt.channel_replica_enabled(channel, &user),
                            reference_name: None,
                        }
                    })
                    .collect()
            };
            let legacy_membership = reference.as_ref().map_or_else(
                || node.channel_legacy_membership(channel),
                |reference| reference.legacy_membership,
            );
            let members = display_member_facts(legacy_membership, facts)
                .into_iter()
                .map(
                    |ChannelMemberFact {
                         public,
                         account_id,
                         pending,
                         replica_enabled,
                         reference_name,
                         ..
                     }| {
                        let user_id = public.user_id();
                        let name = if account_id.as_deref() == Some(own_account) {
                            rt.display_name().to_string()
                        } else {
                            account_id
                                .as_ref()
                                .and_then(|id| known_names.get(id))
                                .and_then(|names| names.first())
                                .cloned()
                                .or_else(|| {
                                    (account_id.is_none()
                                        || rt.account_id_for_device(&public) == account_id)
                                        .then(|| rt.display_name_for(&user_id))
                                        .flatten()
                                })
                                .or(reference_name)
                                .unwrap_or_else(|| user_id.clone())
                        };
                        ChannelMemberInfo {
                            file_sharing: (reference.is_none() && user_id == node.user_id())
                                .then(|| node.group_file_sharing_status()),
                            user_id,
                            name,
                            account_id,
                            pending,
                            replica_enabled,
                        }
                    },
                )
                .collect();
            Ok(ChannelMembersInfo {
                history_policy: match reference.as_ref().and_then(|r| r.observed_history_policy()) {
                    Some(policy) => policy,
                    None => node.channel_history_policy(channel)?,
                },
                history_policy_known: reference
                    .as_ref()
                    .is_none_or(|r| r.observed_history_policy().is_some()),
                directory_reconciling: reference.as_ref().is_some_and(|r| r.directory_reconciling),
                owner: reference
                    .as_ref()
                    .map(|r| r.owner.clone())
                    .unwrap_or_else(|| rt.channel_owner(channel)),
                owner_account_id: reference
                    .as_ref()
                    .and_then(|r| r.owner_account_id.clone())
                    .or_else(|| node.channel_owner_account_id(channel)),
                can_manage: reference.is_none() && node.channel_can_manage(channel)?,
                account_access: Some(AccountGroupAccessInfo::read(&node, channel)),
                members,
            })
        })
        .await
}

#[tauri::command]
pub async fn set_channel_history_policy(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    policy: mesh_talk_core::channel::HistoryPolicy,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.set_channel_history_policy_if(channel, policy, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn set_channel_replica_enabled(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    enabled: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.set_channel_replica_enabled_if(channel, enabled, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

/// Temporary permission for this device's optional group attachment assistance.
#[tauri::command]
pub async fn set_group_file_sharing_override(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    network_cost: mesh_talk_core::node::NetworkCost,
    enabled: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    admission
        .with_runtime(move |runtime| {
            if runtime
                .handle()
                .set_group_file_sharing_override(network_cost, enabled)
            {
                Ok(())
            } else {
                Err(CommandError::InvalidInput(
                    "Network cost changed; refresh before allowing file sharing".into(),
                ))
            }
        })
        .await
}

#[derive(Serialize)]
pub struct ChannelCustodyInfo {
    id: String,
    replicas: usize,
    post_offices: usize,
}

/// Historical, authenticated durable-copy confirmations for this account's own
/// channel messages. These counts do not imply recipient delivery or reading.
#[tauri::command]
pub async fn channel_custody_statuses<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    ids: Vec<String>,
) -> Result<Vec<ChannelCustodyInfo>, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    if ids.len() > 256 {
        return Err(CommandError::InvalidInput(
            "at most 256 message ids are allowed".into(),
        ));
    }
    let ids = ids
        .iter()
        .map(|id| parse_event_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    let admission = RuntimeAdmission::capture_current(app.state::<AppState>().inner(), &state)?;
    admission
        .with_runtime(move |runtime| {
            let node = runtime.handle();
            let mut counts = Vec::new();
            for id in ids {
                let Some(replicas) = node.channel_custody_count(channel, id)? else {
                    continue;
                };
                let Some(post_offices) = node.channel_post_office_custody_count(channel, id)?
                else {
                    continue;
                };
                counts.push(ChannelCustodyInfo {
                    id: id.to_hex(),
                    replicas,
                    post_offices,
                });
            }
            Ok(counts)
        })
        .await
}

#[derive(Serialize)]
pub struct ChannelActivityInfo {
    pub kind: String,
    pub actor: String,
    pub target: Option<String>,
    pub wall_clock: u64,
}

#[tauri::command]
pub async fn channel_activity(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<Vec<ChannelActivityInfo>, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.read().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .channel_activity(channel)?
        .into_iter()
        .map(|entry| ChannelActivityInfo {
            kind: entry.kind.to_string(),
            actor: entry.actor,
            target: entry.target,
            wall_clock: entry.wall_clock,
        })
        .collect())
}

#[tauri::command]
pub async fn create_channel(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    name: String,
    member_account_ids: Vec<String>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = admission.checked(runtime, || Ok(runtime.handle()))?;
                let id = node
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
pub async fn add_channel_member(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let (node, member) = admission.checked(runtime, || {
                    let rt = runtime;
                    let member = rt.channel_invitee_public(&member_id).ok_or_else(|| {
                        CommandError::Validation(format!("unknown or ineligible peer: {member_id}"))
                    })?;
                    Ok((rt.handle(), member))
                })?;
                node.add_channel_member_if(channel, member, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn remove_channel_member(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.remove_channel_member_if(channel, &member_id, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

/// Remove every certified device of one account in a single signed membership change.
#[tauri::command]
pub async fn remove_channel_account_members(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    account_id: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.remove_channel_account_members_if(channel, &account_id, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn transfer_channel_owner(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.transfer_channel_owner_if(channel, &member_id, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn leave_channel(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.leave_channel_if(channel, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn accept_channel_invite(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.accept_channel_invite_if(channel, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn rename_channel(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    name: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.rename_channel_if(channel, &name, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn dissolve_channel(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                node.dissolve_channel_if(channel, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn send_channel_message(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    text: String,
    reply_to: Option<String>,
    expires_at_ms: Option<u64>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = parse_channel_id(&channel_id)?;
                let reply = match reply_to {
                    Some(h) => Some(parse_event_id(&h)?),
                    None => None,
                };
                let node = runtime.handle();
                node.send_channel_message_reply_expiring_if(
                    id,
                    text.as_bytes(),
                    reply,
                    expires_at_ms,
                    |commit| admission.authorize_accept(runtime, commit),
                )
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

/// Map the UI's "sent via the media button?" flag to a manifest file kind, so the receiver
/// categorizes media-vs-attachment by INTENT rather than the file extension.
#[tauri::command]
pub async fn channel_history<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    let limit = limit.min(500);
    let id = parse_channel_id(&channel_id)?;
    let guard = state.0.read().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let history = rt
        .channel_history(id, limit)?
        .into_iter()
        .map(HistoryItem::from)
        .collect();
    crate::app_lock::reject_if_locked(&app)?;
    Ok(history)
}

#[tauri::command]
pub async fn react_channel(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let channel = parse_channel_id(&channel_id)?;
                let id = parse_event_id(&target)?;
                let node = runtime.handle();
                node.react_channel_if(channel, id, &emoji, remove, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[cfg(test)]
mod inventory_display_tests {
    use super::*;
    use mesh_talk_core::identity::device::DeviceIdentity;

    #[test]
    fn group_inventory_display_roster_matches_legacy_people_and_preserves_modern_members() {
        let owner = DeviceIdentity::generate().public();
        let member = DeviceIdentity::generate().public();
        let relay = DeviceIdentity::generate().public();
        let facts = || {
            [
                (owner.clone(), false),
                (member.clone(), false),
                (relay.clone(), true),
            ]
            .into_iter()
            .map(|(public, post_office)| ChannelMemberFact {
                account_id: Some(public.user_id()),
                public,
                pending: false,
                replica_enabled: false,
                reference_name: None,
                post_office,
            })
            .collect()
        };
        let legacy = display_member_facts(true, facts());
        assert_eq!(
            legacy
                .iter()
                .map(|member| &member.public)
                .collect::<Vec<_>>(),
            vec![&owner, &member]
        );
        let modern = display_member_facts(false, facts());
        assert_eq!(
            modern
                .iter()
                .map(|member| &member.public)
                .collect::<Vec<_>>(),
            vec![&owner, &member, &relay]
        );
    }
}
