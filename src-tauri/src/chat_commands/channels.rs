use super::{
    parse_channel_id, parse_event_id, to_reaction_infos, ChannelMemberInfo, ChannelMembersInfo,
    HistoryItem, NodeState, ReactionInfo,
};
use crate::commands::CommandError;
use serde::Serialize;

/// A channel as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelInfo {
    pub channel_id: String, // hex
    pub name: String,
    pub member_count: usize,
    /// The owner's device `user_id` — only the owner may rename the channel. The UI gates
    /// the synced-rename action on this (non-owners fall back to a local alias).
    pub owner: String,
}

#[tauri::command]
pub async fn list_channels(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<ChannelInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .list_channels()
        .into_iter()
        .map(|c| ChannelInfo {
            channel_id: hex::encode(c.id.as_bytes()),
            name: c.name,
            member_count: c.member_count,
            owner: c.owner,
        })
        .collect())
}

#[tauri::command]
pub async fn channel_members(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<ChannelMembersInfo, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    // Resolve each member's display name via the runtime, which checks the LIVE roster
    // first and then the DURABLE name directory — so a member that's gone offline (and so
    // been evicted from the roster) still shows the last name we saw, not a raw hex id.
    // Ourselves: we're never in our own roster, so use our own advertised name.
    let self_uid = rt.user_id();
    let members = rt
        .channel_members(channel)
        .into_iter()
        .map(|p| {
            let user_id = p.user_id();
            let name = if user_id == self_uid {
                rt.display_name().to_string()
            } else {
                rt.display_name_for(&user_id)
                    .unwrap_or_else(|| user_id.clone())
            };
            let account_id = if user_id == self_uid {
                Some(rt.account_id().to_string())
            } else {
                rt.account_id_for_device(&p)
            };
            ChannelMemberInfo {
                user_id,
                name,
                account_id,
            }
        })
        .collect();
    Ok(ChannelMembersInfo {
        owner: rt.channel_owner(channel),
        members,
    })
}

#[tauri::command]
pub async fn create_channel(
    state: tauri::State<'_, NodeState>,
    name: String,
    member_ids: Vec<String>,
) -> Result<String, CommandError> {
    let (node, members) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let mut members = Vec::new();
        for uid in &member_ids {
            let p = rt
                .peer_public(uid)
                .ok_or_else(|| CommandError::Validation(format!("unknown peer: {uid}")))?;
            members.push(p);
        }
        (rt.handle(), members)
    };
    let id = node
        .create_channel(&name, members)
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn add_channel_member(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let (node, member) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let member = rt
            .peer_public(&member_id)
            .ok_or_else(|| CommandError::Validation(format!("unknown peer: {member_id}")))?;
        (rt.handle(), member)
    };
    node.add_channel_member(channel, member)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn remove_channel_member(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let node = state.node_handle().await?;
    node.remove_channel_member(channel, &member_id)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn rename_channel(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    name: String,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let node = state.node_handle().await?;
    node.rename_channel(channel, &name)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn send_channel_message(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    let id = parse_channel_id(&channel_id)?;
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    let node = state.node_handle().await?;
    node.send_channel_message_reply(id, text.as_bytes(), reply)
        .await
        .map_err(CommandError::from)
}

/// Map the UI's "sent via the media button?" flag to a manifest file kind, so the receiver
/// categorizes media-vs-attachment by INTENT rather than the file extension.
#[tauri::command]
pub async fn channel_history(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    let limit = limit.min(500);
    let id = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .channel_history(id, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect())
}

#[tauri::command]
pub async fn react_channel(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let id = parse_event_id(&target)?;
    let node = state.node_handle().await?;
    node.react_channel(channel, id, &emoji, remove)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn channel_reactions(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(to_reaction_infos(rt.channel_reactions(channel)))
}
