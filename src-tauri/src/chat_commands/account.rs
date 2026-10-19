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
    Ok(rt.peers().into_iter().map(PeerInfo::from).collect())
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

/// Durable signed profiles, including `null` for a cleared avatar. The frontend uses
/// tombstones so an old local copy cannot reappear after a linked device clears it.
#[tauri::command]
pub async fn peer_avatars(
    state: tauri::State<'_, NodeState>,
) -> Result<std::collections::HashMap<String, Option<String>>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peer_profile_states()
        .into_iter()
        .map(|(id, bytes)| {
            (
                id,
                bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
            )
        })
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
    app: tauri::State<'_, crate::state::AppState>,
    policy: tauri::State<'_, crate::contact_policy::HiddenContactsState>,
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<AccountInfo>, CommandError> {
    let session = app.session().clone();
    let owner = session
        .with_owner(|owner| {
            owner
                .map(str::to_owned)
                .ok_or_else(|| "Missing contact owner".to_owned())
        })
        .map_err(CommandError::Authorization)?;
    let node = state.0.clone();
    #[cfg(test)]
    let captured = app.owner_command_captured.clone();
    let policy = policy.inner().clone();
    tokio::task::spawn_blocking(move || {
        // Runtime first, matching startup and runtime admission. Never wait for
        // it while holding the session needed by an installation or producer.
        #[cfg(test)]
        if let Some(entered) = captured.lock().unwrap().as_ref() {
            let _ = entered.send("contact-runtime-wait");
        }
        let guard = node.blocking_lock();
        policy.transaction(&session, &owner, || {
            // Capture fresh presence inside the policy transaction so an older
            // poll cannot undo a later contact deletion.
            let rt = guard.as_ref().ok_or("Node not started")?;
            if rt.host_account_id() != Some(owner.as_str()) {
                return Err("Contact roster requires this account's running node".into());
            }
            let peers = rt.peers();
            let online = online_account_ids(peers.clone());
            let live = peers
                .into_iter()
                .filter_map(|p| p.account_id.map(|id| (id, p.name, p.post_office)))
                .collect();
            let mut rows = merge_account_rows(live, rt.known_account_ids());
            let accounts = rows.iter().map(|row| row.account_id.clone()).collect();
            let visible = policy.visible_accounts(&owner, &online, accounts)?;
            rows.retain(|row| visible.contains(&row.account_id));
            Ok(rows)
        })
    })
    .await
    .map_err(|_| CommandError::Internal("Contact policy task failed".into()))?
    .map_err(CommandError::from)
}

pub(crate) fn online_account_ids(
    peers: Vec<mesh_talk_core::discovery::roster::PeerRecord>,
) -> Vec<String> {
    let now = std::time::Instant::now();
    peers
        .into_iter()
        .filter(|p| !p.post_office && !p.presence_evidence(now).is_empty())
        .filter_map(|p| p.account_id)
        .collect()
}

fn merge_account_rows(live: Vec<(String, String, bool)>, known: Vec<String>) -> Vec<AccountInfo> {
    use std::collections::BTreeMap;
    let mut by_account: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for account_id in known {
        // A durable conversation remains a contact even if its only device currently
        // discovered is a relay. Live relay discovery alone never creates a row.
        by_account.entry(account_id).or_default();
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
    fn deletion_presence_uses_the_same_source_evidence_as_ui() {
        use mesh_talk_core::{
            discovery::roster::{PeerEndpoint, PeerRecord, RouteSource},
            identity::device::DeviceIdentity,
        };
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let fresh = PeerRecord {
            public: DeviceIdentity::generate().public(),
            addr: "127.0.0.1:1234".parse().unwrap(),
            routes: vec![PeerEndpoint {
                addr: "127.0.0.1:1234".parse().unwrap(),
                source: RouteSource::Lan,
                last_seen: now,
                last_authenticated: None,
                presence_expires_at: None,
            }],
            name: "Alice".into(),
            post_office: false,
            account_id: Some("alice".into()),
            last_seen: now,
        };
        let mut stale = fresh.clone();
        stale.routes[0].last_seen -= Duration::from_secs(31);
        assert!(super::online_account_ids(vec![stale.clone()]).is_empty());
        assert_eq!(
            super::online_account_ids(vec![stale, fresh.clone()]),
            vec!["alice"]
        );

        let mut routed = fresh.clone();
        routed.last_seen -= Duration::from_secs(60);
        routed.routes[0].last_seen -= Duration::from_secs(60);
        routed.routes[0].source = RouteSource::RoutedQuery;
        assert_eq!(super::online_account_ids(vec![routed]), vec!["alice"]);

        for source in [RouteSource::Gateway, RouteSource::PostOffice] {
            let mut indirect = fresh.clone();
            indirect.routes[0].source = source;
            indirect.routes[0].presence_expires_at = Some(now - Duration::from_secs(1));
            assert!(super::online_account_ids(vec![indirect]).is_empty());
        }
        let mut relay = fresh.clone();
        relay.post_office = true;
        assert!(super::online_account_ids(vec![relay]).is_empty());
        let mut route_only = fresh;
        route_only.routes.clear();
        assert!(super::online_account_ids(vec![route_only]).is_empty());
    }

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
            vec![],
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

    #[test]
    fn history_contact_remains_when_only_its_relay_device_is_online() {
        let rows = merge_account_rows(
            vec![("shared-account".into(), "Relay".into(), true)],
            vec!["shared-account".into()],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account_id, "shared-account");
        assert_eq!(rows[0].device_count, 0);
        assert!(rows[0].names.is_empty());
    }
}
