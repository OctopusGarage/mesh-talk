use super::{
    HistoryItem, NodeState, ReactionInfo, parse_channel_id, parse_event_id, to_reaction_infos,
};
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission, state::AppState};
use serde::Serialize;

#[tauri::command]
pub async fn send_dm(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    recipient: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                // Snapshot the node handle, then release the state lock before the .await send.
                let node = runtime.handle();
                let reply = match reply_to {
                    Some(h) => Some(parse_event_id(&h)?),
                    None => None,
                };
                node.send_dm_reply_if(&recipient, text.as_bytes(), reply, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

/// Send an opaque WebRTC call signal (SDP offer/answer / "bye") to a specific device
/// `target` (a peer user_id). Ephemeral and device-addressed — never logged, never an
/// account fan-out. Errors if the peer is offline/unknown (a live call needs both online).
#[tauri::command]
pub async fn send_call_signal(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    target: String,
    payload: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = runtime.handle();
                node.send_call_signal_if(&target, payload.as_bytes(), |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn history<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, NodeState>,
    peer: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    // Cap the page size so a frontend accident (e.g. a huge JS number) can't
    // request an unbounded scan; the node truncates to this anyway.
    let limit = limit.min(500);
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| CommandError::Validation(format!("unknown peer: {peer}")))?;
    let history = rt
        .history(&public, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect();
    crate::app_lock::reject_if_locked(&app)?;
    Ok(history)
}

#[tauri::command]
pub async fn send_to_account(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    account: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    let reply = reply_to.as_deref().map(parse_event_id).transpose()?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = runtime.handle();
                let id = node
                    .enqueue_to_account_if(&account, text.as_bytes(), reply, |accept| {
                        admission.authorize_accept(runtime, accept)
                    })
                    .await
                    .map_err(CommandError::from)?;
                node.flush_delivery(id).await;
                Ok(())
            })
        })
        .await
}

/// Send an animated sticker as its own message. `convId` is the channel id (hex) when
/// `isChannel`, else the peer account id. `stickerId` is the bundled sticker's codepoint id;
/// `fallback` is the emoji char shown if the recipient lacks that sticker.
#[tauri::command]
pub async fn send_sticker(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    conv_id: String,
    sticker_id: String,
    fallback: String,
    is_channel: bool,
    expires_at_ms: Option<u64>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    let channel = is_channel.then(|| parse_channel_id(&conv_id)).transpose()?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = runtime.handle();
                match channel {
                    Some(channel) => node
                        .send_sticker_channel_expiring_if(
                            channel,
                            &sticker_id,
                            fallback.as_bytes(),
                            expires_at_ms,
                            |commit| admission.authorize_accept(runtime, commit),
                        )
                        .await
                        .map_err(CommandError::from),
                    None => {
                        let id = node
                            .enqueue_sticker_to_account_if(
                                &conv_id,
                                &sticker_id,
                                fallback.as_bytes(),
                                |accept| admission.authorize_accept(runtime, accept),
                            )
                            .await
                            .map_err(CommandError::from)?;
                        node.flush_delivery(id).await;
                        Ok(())
                    }
                }
            })
        })
        .await
}

#[tauri::command]
pub async fn account_history<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, NodeState>,
    account: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    let limit = limit.min(500);
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let history = rt
        .account_history(&account, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect();
    crate::app_lock::reject_if_locked(&app)?;
    Ok(history)
}

#[tauri::command]
pub async fn react_dm(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    recipient: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = parse_event_id(&target)?;
                let node = runtime.handle();
                node.react_dm_if(&recipient, id, &emoji, remove, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn reactions(
    state: tauri::State<'_, NodeState>,
    peer: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| CommandError::Validation(format!("unknown peer: {peer}")))?;
    Ok(to_reaction_infos(rt.reactions_dm(&public)))
}

#[tauri::command]
pub async fn react_account(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    account: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = parse_event_id(&target)?;
                let node = runtime.handle();
                node.react_to_account_if(&account, id, &emoji, remove, |commit| {
                    admission.authorize_accept(runtime, commit)
                })
                .await
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn account_reactions(
    state: tauri::State<'_, NodeState>,
    account: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(to_reaction_infos(rt.account_reactions(&account)))
}

/// Delete one message from THIS device only (local; not propagated). `conv_id` is the
/// channel id (hex) when `is_channel`, else the peer account id. `target` is the message id
/// the UI holds. The file rewrite runs on the blocking pool.
#[tauri::command]
pub async fn delete_message(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    conv_id: String,
    target: String,
    is_channel: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;

    let id = parse_event_id(&target)?;
    let channel = if is_channel {
        Some(parse_channel_id(&conv_id)?)
    } else {
        None
    };
    admission
        .blocking(move |runtime, _| {
            let node = runtime.handle();
            match channel {
                Some(channel) => node.delete_message(channel, id, false),
                None => node.delete_account_message(&conv_id, id),
            }
            .map(|_| ())
            .map_err(CommandError::from)
        })
        .await
}

/// Recall (unsend) one of OUR OWN messages within the 2-minute window — propagates to peers.
#[tauri::command]
pub async fn recall_message(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    conv_id: String,
    target: String,
    is_channel: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = parse_event_id(&target)?;
                let node = runtime.handle();
                if is_channel {
                    let channel = parse_channel_id(&conv_id)?;
                    node.recall_channel_if(channel, id, |commit| {
                        admission.authorize_accept(runtime, commit)
                    })
                    .await
                    .map_err(CommandError::from)
                } else {
                    node.recall_account_if(&conv_id, id, |commit| {
                        admission.authorize_accept(runtime, commit)
                    })
                    .await
                    .map_err(CommandError::from)
                }
            })
        })
        .await
}

/// Clear all locally-stored history for a conversation (text + files). Local only.
#[tauri::command]
pub async fn clear_conversation(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    conv_id: String,
    is_channel: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;

    let channel = if is_channel {
        Some(parse_channel_id(&conv_id)?)
    } else {
        None
    };
    admission
        .blocking(move |runtime, _| {
            let node = runtime.handle();
            match channel {
                Some(channel) => node.clear_conversation(channel),
                None => node.clear_account_conversation(&conv_id),
            }
            .map(|_| ())
            .map_err(CommandError::from)
        })
        .await
}

/// A search result hit for display in the UI.
#[derive(Serialize)]
pub struct SearchHitInfo {
    pub is_channel: bool,
    pub account_id: Option<String>,
    pub target: String,
    pub label: String,
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
}

#[tauri::command]
pub async fn search<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, NodeState>,
    query: String,
) -> Result<Vec<SearchHitInfo>, CommandError> {
    // Snapshot the node handle and DROP the state lock before the scan, so a search can't
    // serialize all other node IPC behind the synchronous scan; run the scan on the
    // blocking pool (it reads/decrypts every conversation's stores).
    let node = state.node_handle().await?;
    let hits = tokio::task::spawn_blocking(move || node.search(&query))
        .await
        .map_err(|e| format!("join error: {e}"))?;
    crate::app_lock::reject_if_locked(&app)?;
    Ok(hits
        .into_iter()
        .map(|h| SearchHitInfo {
            is_channel: h.is_channel,
            account_id: h.account_id,
            target: h.target,
            label: h.label,
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
        })
        .collect())
}
