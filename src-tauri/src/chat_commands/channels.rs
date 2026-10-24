use super::{
    ChannelMemberInfo, ChannelMembersInfo, HistoryItem, NodeState, ReactionInfo, parse_channel_id,
    parse_event_id, to_reaction_infos,
};
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission, state::AppState};
use serde::Serialize;

/// A channel as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelInfo {
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
    pub archived: bool,
    pub dissolved: bool,
    pub left: bool,
    pub pending_invite: bool,
    pub recovery: Option<mesh_talk_core::node::ChannelRecovery>,
}

#[tauri::command]
pub async fn list_channels(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<ChannelInfo>, CommandError> {
    let guard = state.0.read().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .list_channels()
        .into_iter()
        .map(|c| ChannelInfo {
            channel_id: hex::encode(c.id.as_bytes()),
            name: c.name,
            member_count: c.member_count,
            owner: c.owner,
            owner_account_id: rt.handle().channel_owner_account_id(c.id),
            can_manage: rt.handle().channel_can_manage(c.id),
            archived: c.archived,
            dissolved: c.dissolved,
            left: c.left,
            pending_invite: c.pending_invite,
            recovery: rt.channel_recovery(c.id),
        })
        .collect())
}

#[tauri::command]
pub async fn channel_members(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<ChannelMembersInfo, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.read().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    // Resolve each member's display name via the runtime, which checks the LIVE roster
    // first and then the DURABLE name directory — so a member that's gone offline (and so
    // been evicted from the roster) still shows the last name we saw, not a raw hex id.
    // Ourselves: we're never in our own roster, so use our own advertised name.
    let own_account = rt.account_id();
    let known_names = rt.known_account_names();
    let pending: std::collections::HashSet<_> =
        rt.channel_pending_members(channel).into_iter().collect();
    let members = rt
        .channel_members(channel)
        .into_iter()
        .map(|p| {
            let user_id = p.user_id();
            let account_id = rt.channel_member_account_binding(channel, &p);
            let name = if account_id.as_deref() == Some(own_account) {
                rt.display_name().to_string()
            } else {
                account_id
                    .as_ref()
                    .and_then(|id| known_names.get(id))
                    .and_then(|names| names.first())
                    .cloned()
                    .or_else(|| {
                        // An adopted device may still appear in an older pinned
                        // grant. Its new account's nickname is not that member's name.
                        (account_id.is_none() || rt.account_id_for_device(&p) == account_id)
                            .then(|| rt.display_name_for(&user_id))
                            .flatten()
                    })
                    .unwrap_or_else(|| user_id.clone())
            };
            ChannelMemberInfo {
                pending: pending.contains(&user_id),
                replica_enabled: rt.channel_replica_enabled(channel, &user_id),
                user_id,
                name,
                account_id,
            }
        })
        .collect();
    Ok(ChannelMembersInfo {
        history_policy: rt.handle().channel_history_policy(channel),
        owner: rt.channel_owner(channel),
        owner_account_id: rt.handle().channel_owner_account_id(channel),
        can_manage: rt.handle().channel_can_manage(channel),
        members,
    })
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

#[derive(Serialize)]
pub struct ChannelCustodyInfo {
    id: String,
    replicas: usize,
    post_offices: usize,
}

/// Historical, authenticated durable-copy confirmations for this device's own
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
    let node = state.node_handle().await?;
    let statuses = ids
        .into_iter()
        .filter_map(|id| {
            node.channel_custody_count(channel, id)
                .map(|replicas| ChannelCustodyInfo {
                    id: id.to_hex(),
                    replicas,
                    post_offices: node
                        .channel_post_office_custody_count(channel, id)
                        .unwrap_or(0),
                })
        })
        .collect();
    crate::app_lock::reject_if_locked(&app)?;
    Ok(statuses)
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
        .channel_activity(channel)
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
    member_ids: Vec<String>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let (node, members) = admission.checked(runtime, || {
                    let rt = runtime;
                    let mut members = Vec::new();
                    for uid in &member_ids {
                        let p = rt.channel_invitee_public(uid).ok_or_else(|| {
                            CommandError::Validation(format!("unknown or ineligible peer: {uid}"))
                        })?;
                        members.push(p);
                    }
                    Ok((rt.handle(), members))
                })?;
                let id = node
                    .create_channel_invited_if(&name, members, |commit| {
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
        .channel_history(id, limit)
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

#[tauri::command]
pub async fn channel_reactions(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.read().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(to_reaction_infos(rt.channel_reactions(channel)))
}
