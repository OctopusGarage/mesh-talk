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
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let live = rt
        .peers()
        .into_iter()
        .filter_map(|p| {
            p.account_id
                .map(|account_id| (account_id, p.name, p.post_office))
        })
        .collect();
    Ok(merge_account_rows(live, rt.known_account_ids()))
}

fn merge_account_rows(live: Vec<(String, String, bool)>, known: Vec<String>) -> Vec<AccountInfo> {
    use std::collections::{BTreeMap, HashSet};
    let normal_accounts: HashSet<String> = live
        .iter()
        .filter_map(|(account_id, _, post_office)| (!post_office).then_some(account_id.clone()))
        .collect();
    let relay_only_accounts: HashSet<String> = live
        .iter()
        .filter_map(|(account_id, _, post_office)| {
            (*post_office && !normal_accounts.contains(account_id)).then_some(account_id.clone())
        })
        .collect();
    let mut by_account: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for account_id in known {
        if !relay_only_accounts.contains(&account_id) {
            by_account.entry(account_id).or_default();
        }
    }
    for (account_id, name, post_office) in live {
        if !post_office {
            by_account.entry(account_id).or_default().push(name);
        }
    }
    by_account
        .into_iter()
        .map(|(account_id, names)| AccountInfo {
            device_count: names.len(),
            names,
            account_id,
        })
        .collect()
}

#[cfg(test)]
mod recovery_accounts_tests {
    use super::merge_account_rows;

    #[test]
    fn offline_history_account_is_listed_with_zero_live_devices() {
        let rows = merge_account_rows(vec![], vec!["abcd".into()]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account_id, "abcd");
        assert_eq!(rows[0].device_count, 0);
        assert!(rows[0].names.is_empty());
    }

    #[test]
    fn live_and_history_accounts_are_not_duplicated() {
        let rows = merge_account_rows(
            vec![("abcd".into(), "Alice".into(), false)],
            vec!["abcd".into()],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].device_count, 1);
        assert_eq!(rows[0].names, vec!["Alice"]);
    }

    #[test]
    fn discovered_post_office_is_not_listed_as_a_dm_account() {
        let rows = merge_account_rows(
            vec![
                ("relay-account".into(), "mac2015-post-office".into(), true),
                ("friend-account".into(), "Alice".into(), false),
            ],
            vec!["relay-account".into()],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account_id, "friend-account");
        assert_eq!(rows[0].names, vec!["Alice"]);
    }

    #[test]
    fn normal_device_sharing_a_relay_account_remains_a_dm_contact() {
        let rows = merge_account_rows(
            vec![
                ("shared-account".into(), "Relay".into(), true),
                ("shared-account".into(), "Alice".into(), false),
            ],
            vec!["shared-account".into()],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account_id, "shared-account");
        assert_eq!(rows[0].device_count, 1);
        assert_eq!(rows[0].names, vec!["Alice"]);
    }
}
