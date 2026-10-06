use super::{NodeState, PeerInfo};
use crate::commands::CommandError;
use serde::Serialize;

#[tauri::command]
pub async fn my_id(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt.user_id().to_string())
}

#[tauri::command]
pub async fn list_peers(state: tauri::State<'_, NodeState>) -> Result<Vec<PeerInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peers()
        .into_iter()
        .map(|p| PeerInfo {
            user_id: p.public.user_id(),
            name: p.name,
            addr: p.addr.to_string(),
            post_office: p.post_office,
            account_id: p.account_id,
        })
        .collect())
}

#[tauri::command]
pub async fn account_id(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt.account_id().to_string())
}

/// Publish (or clear) this user's OWN avatar to peers as a signed profile. Called by the
/// frontend when the user sets/removes their own photo (the local `avatars.json` mirror is
/// still written by `set_avatar` so the override-precedence logic is unchanged). `avatar`
/// is the small data-URL string; `None` clears it (propagates a "no avatar"). The node
/// bounds the size and signs it with the account key.
#[tauri::command]
pub async fn publish_avatar(
    state: tauri::State<'_, NodeState>,
    avatar: Option<String>,
) -> Result<(), CommandError> {
    let node = state.node_handle().await?;
    node.set_avatar(avatar.map(|s| s.into_bytes()))
        .await
        .map_err(CommandError::from)
}

/// Every avatar peers have propagated to us, as `account_id -> data-URL`. The frontend
/// merges these into its avatars store on startup so received avatars survive a relaunch.
#[tauri::command]
pub async fn peer_avatars(
    state: tauri::State<'_, NodeState>,
) -> Result<std::collections::HashMap<String, String>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peer_avatars()
        .into_iter()
        .map(|(id, bytes)| (id, String::from_utf8_lossy(&bytes).into_owned()))
        .collect())
}

#[tauri::command]
pub async fn start_linking(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt.start_linking())
}

#[tauri::command]
pub async fn stop_linking(state: tauri::State<'_, NodeState>) -> Result<(), CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.stop_linking();
    Ok(())
}

#[tauri::command]
pub async fn link_device(
    state: tauri::State<'_, NodeState>,
    peer: String,
    code: String,
) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.link_device(&peer, &code)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn rekey_account(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.rekey_account().map_err(CommandError::from)
}

/// An account (group of devices) as shown in the chat UI.
#[derive(Serialize)]
pub struct AccountInfo {
    pub account_id: String,
    pub device_count: usize,
    pub names: Vec<String>,
}

#[tauri::command]
pub async fn list_accounts(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<AccountInfo>, CommandError> {
    use std::collections::BTreeMap;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let mut by_account: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in rt.peers() {
        if let Some(acct) = p.account_id {
            by_account.entry(acct).or_default().push(p.name);
        }
    }
    Ok(by_account
        .into_iter()
        .map(|(account_id, names)| AccountInfo {
            device_count: names.len(),
            names,
            account_id,
        })
        .collect())
}
