use super::NodeState;
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission, state::AppState};
use mesh_talk_core::discovery::roster::RouteSource;
use serde::Serialize;

// --- Diagnostics / discovery ------------------------------------------------

/// A discovered peer as shown on the Diagnostics page.
#[derive(Serialize)]
pub struct DiagPeerInfo {
    pub user_id: String,
    pub name: String,
    pub ip: String,
    pub tcp_port: u16,
    /// The shown endpoint belongs to a local gateway, not the peer's host.
    pub via_gateway: bool,
    pub post_office: bool,
    pub account_id: Option<String>,
    /// Whole seconds since this peer was last heard from.
    pub last_seen_secs: u64,
}

/// This device's own identity + network facts, for the Diagnostics page.
#[derive(Serialize)]
pub struct DiagNetworkInfo {
    pub own_user_id: String,
    pub own_name: String,
    pub account_id: String,
    pub listen_tcp_port: u16,
    pub discovery_port: u16,
    pub multicast_group: String,
    pub interfaces: Vec<String>,
}

/// Snapshot the current roster for the Diagnostics page. Polled by the frontend.
#[tauri::command]
pub async fn diag_get_peers(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<DiagPeerInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peers()
        .into_iter()
        .map(|p| DiagPeerInfo {
            user_id: p.public.user_id(),
            name: p.name,
            ip: p.addr.ip().to_string(),
            tcp_port: p.addr.port(),
            via_gateway: p
                .routes
                .iter()
                .any(|route| route.addr == p.addr && route.source == RouteSource::Gateway),
            post_office: p.post_office,
            account_id: p.account_id,
            last_seen_secs: p.last_seen.elapsed().as_secs(),
        })
        .collect())
}

/// Force an immediate re-announce + /24 rescan (the manual "announce now" control on
/// the Diagnostics page). Helps converge first-contact when LAN discovery is flaky.
#[tauri::command]
pub async fn rescan_peers(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
) -> Result<(), CommandError> {
    RuntimeAdmission::capture_current(&app, &state)?
        .with_runtime(|runtime| {
            runtime.trigger_discovery();
            Ok(())
        })
        .await
}

/// Query one address supplied by the user. A successful send only means that a
/// query was queued; the normal signed-response and Noise checks still apply.
#[tauri::command]
pub async fn probe_peer_endpoint(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    host: String,
    port: Option<u16>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&app, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let host = host.trim();
                if host.is_empty()
                    || host.len() > 253
                    || host
                        .chars()
                        .any(|ch| ch.is_whitespace() || ch == '/' || ch == '\\')
                    || (host.contains(':') && host.parse::<std::net::Ipv6Addr>().is_err())
                {
                    return Err(CommandError::InvalidInput(
                        "enter an IP address or hostname without a port".into(),
                    ));
                }
                let port = port.unwrap_or(mesh_talk_core::node::DEFAULT_DISCOVERY_PORT);
                if port == 0 {
                    return Err(CommandError::InvalidInput(
                        "discovery port must be nonzero".into(),
                    ));
                }
                let addresses: Vec<_> = tokio::net::lookup_host((host, port))
                    .await
                    .map_err(|error| CommandError::Io(error.to_string()))?
                    .take(8)
                    .collect();
                if addresses.is_empty() {
                    return Err(CommandError::InvalidInput("no address found".into()));
                }
                admission.checked(runtime, || Ok(()))?;
                let rt = runtime;
                let mut last_error = None;
                for address in addresses {
                    match rt
                        .probe_endpoint_if(address, |commit| {
                            admission.authorize_io(runtime, commit)
                        })
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(error) => last_error = Some(error),
                    }
                }
                Err(CommandError::Io(last_error.map_or_else(
                    || "query failed".into(),
                    |error| error.to_string(),
                )))
            })
        })
        .await
}

/// This device's own identity + LAN/discovery facts, for the Diagnostics page.
#[tauri::command]
pub async fn diag_network_info(
    state: tauri::State<'_, NodeState>,
) -> Result<DiagNetworkInfo, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(DiagNetworkInfo {
        own_user_id: rt.user_id().to_string(),
        own_name: rt.display_name().to_string(),
        account_id: rt.account_id().to_string(),
        listen_tcp_port: rt.listen_tcp_port(),
        discovery_port: mesh_talk_core::node::DEFAULT_DISCOVERY_PORT,
        multicast_group: mesh_talk_core::node::DISCOVERY_MULTICAST_GROUP.to_string(),
        interfaces: mesh_talk_core::node::ipv4_interface_addrs()
            .into_iter()
            .map(|ip| ip.to_string())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::Manager;

    #[tokio::test]
    async fn diagnostics_expose_a_live_runtime_and_reject_invalid_manual_targets() {
        let root = tempfile::tempdir().unwrap();
        let managed = NodeState::empty();
        let app_state = AppState::new(crate::services::auth_service::AuthService::new(
            std::sync::Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        app_state.session().set(
            "diagnostics".into(),
            crate::services::user::User {
                user_id: "diagnostics-test".into(),
                name: "diagnostics".into(),
                display_name: "Diagnostics Test".into(),
                address: "local".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "test-password".into(),
        );
        let app = tauri::test::mock_builder()
            .manage(app_state)
            .manage(managed.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = app.state::<NodeState>();
        assert!(diag_get_peers(state.clone()).await.is_err());
        assert!(diag_network_info(state.clone()).await.is_err());

        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "diagnostics-test",
            "Diagnostics Test",
            "test-password",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let own_user_id = runtime.user_id().to_owned();
        *managed.0.lock().await = Some(runtime);

        assert!(diag_get_peers(state.clone()).await.unwrap().is_empty());
        let info = diag_network_info(state.clone()).await.unwrap();
        assert_eq!(info.own_user_id, own_user_id);
        assert_eq!(
            info.discovery_port,
            mesh_talk_core::node::DEFAULT_DISCOVERY_PORT
        );
        assert!(!info.multicast_group.is_empty());
        assert!(super::super::get_presence(state.clone()).await.unwrap()[&info.account_id].online);
        rescan_peers(app.state::<AppState>(), state.clone())
            .await
            .unwrap();

        let reserved = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let remote_port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let remote = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "remote-diagnostics-test",
            "Remote Diagnostics",
            "test-password",
            remote_port,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let remote_id = remote.user_id().to_owned();
        let remote_account = remote.account_id().to_owned();
        assert_eq!(remote.discovery_port(), remote_port);
        managed
            .0
            .lock()
            .await
            .as_ref()
            .unwrap()
            .probe_endpoint(std::net::SocketAddr::from(([127, 0, 0, 1], remote_port)))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if diag_get_peers(state.clone())
                    .await
                    .unwrap()
                    .iter()
                    .any(|peer| peer.user_id == remote_id)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let discovered = diag_get_peers(state.clone()).await.unwrap();
        assert!(
            discovered
                .iter()
                .any(|peer| peer.name == "Remote Diagnostics")
        );
        assert!(super::super::get_presence(state.clone()).await.unwrap()[&remote_account].online);

        let node = managed.0.lock().await.as_ref().unwrap().handle();
        let public = managed
            .0
            .lock()
            .await
            .as_ref()
            .unwrap()
            .peers()
            .into_iter()
            .find(|p| p.public.user_id() == remote_id)
            .unwrap()
            .public;
        let unbound = mesh_talk_core::identity::device::DeviceIdentity::generate().public();
        let pending_group = node
            .create_channel_invited("Pending", vec![public.clone(), unbound.clone()])
            .await
            .unwrap();
        let accepted_group = node
            .create_channel("Accepted", vec![public, unbound])
            .await
            .unwrap();
        let snapshot =
            serde_json::to_value(super::super::get_presence(state.clone()).await.unwrap()).unwrap();
        let pending = &snapshot[hex::encode(pending_group.as_bytes())];
        assert_eq!(
            pending["member_count"], 1,
            "self is counted; pending invitees are excluded"
        );
        assert_eq!(pending["online_count"], 1);
        assert_eq!(pending["unbound_member_count"], 0);
        let accepted = &snapshot[hex::encode(accepted_group.as_bytes())];
        assert_eq!(
            accepted["member_count"], 2,
            "accepted self and remote are distinct people"
        );
        assert_eq!(accepted["online_count"], 2);
        assert_eq!(
            accepted["unbound_member_count"], 1,
            "an unbound device is never labeled a person"
        );

        for host in ["", "example.com/path", "host name", "127.0.0.1:47474"] {
            assert!(
                probe_peer_endpoint(app.state::<AppState>(), state.clone(), host.into(), None)
                    .await
                    .is_err()
            );
        }
        assert!(
            probe_peer_endpoint(
                app.state::<AppState>(),
                state.clone(),
                "127.0.0.1".into(),
                Some(0)
            )
            .await
            .is_err()
        );
        probe_peer_endpoint(
            app.state::<AppState>(),
            state,
            " 127.0.0.1 ".into(),
            Some(47474),
        )
        .await
        .unwrap();
        managed.0.lock().await.take().unwrap().stop().await;
        remote.stop().await;
    }
}
