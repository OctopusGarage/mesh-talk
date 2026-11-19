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
    let guard = state.0.read().await;
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
    let guard = state.0.read().await;
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

    // Keep the observer current-thread: inventory assertions below distinguish
    // caller reads from maintenance reads. Actual hosts own a separate reactor.
    struct DiagnosticsHost {
        handle: tokio::runtime::Handle,
        observer: std::thread::ThreadId,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl DiagnosticsHost {
        fn new(state: NodeState) -> Self {
            let (ready, started) = std::sync::mpsc::sync_channel(1);
            let (shutdown, stopped) = tokio::sync::oneshot::channel();
            let thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                ready.send(runtime.handle().clone()).unwrap();
                runtime.block_on(async move {
                    let _ = stopped.await;
                    let node = state.0.write().await.take();
                    if let Some(node) = node {
                        node.stop().await;
                    }
                });
                // Runtime destruction also joins its blocking workers, on the
                // owning thread rather than inside the observer's async task.
            });
            Self {
                handle: started.recv().unwrap(),
                observer: std::thread::current().id(),
                shutdown: Some(shutdown),
                thread: Some(thread),
            }
        }

        async fn start(
            &self,
            state: NodeState,
            root: &std::path::Path,
            owner: &str,
            name: &str,
            port: u16,
        ) -> (String, String, u16) {
            let root = root.to_owned();
            let owner = owner.to_owned();
            let name = name.to_owned();
            let observer = self.observer;
            self.handle
                .spawn(async move {
                    assert_ne!(std::thread::current().id(), observer);
                    let runtime = mesh_talk_core::node::NodeRuntime::start(
                        &root,
                        &owner,
                        &name,
                        "test-password",
                        port,
                        |_| {},
                        |_| {},
                        |_| {},
                        |_| {},
                        |_| {},
                    )
                    .await
                    .unwrap();
                    let identity = (
                        runtime.user_id().to_owned(),
                        runtime.account_id().to_owned(),
                        runtime.discovery_port(),
                    );
                    *state.0.write().await = Some(runtime);
                    identity
                })
                .await
                .unwrap()
        }
    }

    impl Drop for DiagnosticsHost {
        fn drop(&mut self) {
            let _ = self.shutdown.take().unwrap().send(());
            let result = self.thread.take().unwrap().join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn diagnostics_host_unwind_joins_runtime_and_releases_profile_for_restart() {
        let root = tempfile::tempdir().unwrap();
        let managed = NodeState::empty();
        let host = DiagnosticsHost::new(managed.clone());
        let (user_id, account_id, _) = host
            .start(
                managed.clone(),
                root.path(),
                "diagnostics-unwind",
                "Diagnostics Unwind",
                0,
            )
            .await;
        assert!(managed.0.read().await.is_some());
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _host = host;
            // Resume an existing unwind so the expected failure does not invoke
            // the process-wide panic hook or change its concurrent observers.
            std::panic::resume_unwind(Box::new("controlled diagnostics observer failure"));
        }));
        assert!(failure.is_err());
        assert!(
            managed.0.read().await.is_none(),
            "unwind cleanup must join runtime retirement before returning"
        );

        let restarted = DiagnosticsHost::new(managed.clone());
        let (restarted_user, restarted_account, _) = restarted
            .start(
                managed.clone(),
                root.path(),
                "diagnostics-unwind",
                "Diagnostics Unwind",
                0,
            )
            .await;
        assert_eq!(restarted_user, user_id);
        assert_eq!(restarted_account, account_id);
        drop(restarted);
        assert!(managed.0.read().await.is_none());
    }

    #[tokio::test]
    async fn diagnostics_expose_a_live_runtime_and_reject_invalid_manual_targets() {
        let root = tempfile::tempdir().unwrap();
        let managed = NodeState::empty();
        let remote_state = NodeState::empty();
        let host = DiagnosticsHost::new(managed.clone());
        let remote_host = DiagnosticsHost::new(remote_state.clone());
        let app_state = AppState::new(crate::services::auth_service::AuthService::new(
            std::sync::Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        app_state.session().set(
            "diagnostics".into(),
            crate::services::user::User {
                user_id: "diagnostics-test".into(),
                account_login_name: None,
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

        let (own_user_id, _, _) = host
            .start(
                managed.clone(),
                root.path(),
                "diagnostics-test",
                "Diagnostics Test",
                0,
            )
            .await;

        assert!(diag_get_peers(state.clone()).await.unwrap().is_empty());
        let info = diag_network_info(state.clone()).await.unwrap();
        assert_eq!(info.own_user_id, own_user_id);
        assert_eq!(
            info.discovery_port,
            mesh_talk_core::node::DEFAULT_DISCOVERY_PORT
        );
        assert!(!info.multicast_group.is_empty());
        assert_eq!(
            serde_json::to_value(
                super::super::get_presence(state.clone(), None)
                    .await
                    .unwrap()
            )
            .unwrap()[&info.account_id]["online"],
            true
        );
        rescan_peers(app.state::<AppState>(), state.clone())
            .await
            .unwrap();

        let (remote_id, remote_account, remote_port) = remote_host
            .start(
                remote_state.clone(),
                root.path(),
                "remote-diagnostics-test",
                "Remote Diagnostics",
                0,
            )
            .await;
        assert_eq!(
            remote_state
                .0
                .read()
                .await
                .as_ref()
                .unwrap()
                .discovery_port(),
            remote_port
        );
        let probe_state = managed.clone();
        let observer = host.observer;
        host.handle
            .spawn(async move {
                assert_ne!(std::thread::current().id(), observer);
                probe_state
                    .0
                    .read()
                    .await
                    .as_ref()
                    .unwrap()
                    .probe_endpoint(std::net::SocketAddr::from(([127, 0, 0, 1], remote_port)))
                    .await
            })
            .await
            .unwrap()
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
        assert_eq!(
            serde_json::to_value(
                super::super::get_presence(state.clone(), None)
                    .await
                    .unwrap()
            )
            .unwrap()[&remote_account]["online"],
            true
        );

        let node = managed.0.write().await.as_ref().unwrap().handle();
        let public = managed
            .0
            .write()
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
        let snapshot = serde_json::to_value(
            super::super::get_presence(state.clone(), None)
                .await
                .unwrap(),
        )
        .unwrap();
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

        // Read the real inventory explicitly: physical-group presence deliberately
        // skips its inventory body. Maintenance must first publish the document.
        // Observe actual caller-thread durable reads before testing matching tokens.
        // This current-thread runtime executes the command projection synchronously;
        // maintenance owns a separate blocking task and cannot pollute this counter.
        let inventory_key = mesh_talk_core::node::AccountDocumentKey::GroupInventory {
            channel: hex::encode(accepted_group.as_bytes()),
            source_device: node.user_id(),
        };
        let initial_reads = node.test_group_inventory_body_read_count();
        let full = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                node.account_document(&inventory_key).unwrap();
                let snapshot = serde_json::to_value(
                    super::super::get_presence(state.clone(), Some(String::new()))
                        .await
                        .unwrap(),
                )
                .unwrap();
                if node.test_group_inventory_body_read_count() > initial_reads {
                    break snapshot;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("normal group publication must produce readable inventory");
        assert!(
            full["groups"]
                .as_object()
                .unwrap()
                .contains_key(&hex::encode(accepted_group.as_bytes()))
        );
        let mut revision = full["groups_revision"].as_str().unwrap().to_owned();
        let mut matched = 0;
        for _ in 0..16 {
            let reads = node.test_group_inventory_body_read_count();
            let reply = serde_json::to_value(
                super::super::get_presence(state.clone(), Some(revision.clone()))
                    .await
                    .unwrap(),
            )
            .unwrap();
            let returned = reply["groups_revision"].as_str().unwrap();
            if reply.get("groups").is_none() {
                assert_eq!(returned, revision);
                assert_eq!(
                    node.test_group_inventory_body_read_count(),
                    reads,
                    "matching revision must skip all caller-thread durable inventory payload reads"
                );
                matched += 1;
            } else {
                // Parallel native fixtures may legitimately change the live roster.
                assert_ne!(
                    returned, revision,
                    "full projection requires changed inputs"
                );
                revision = returned.to_owned();
            }
            assert!(
                reply["people"][&info.account_id]["online"]
                    .as_bool()
                    .unwrap()
            );
            if matched == 3 {
                break;
            }
        }
        assert_eq!(
            matched, 3,
            "live inputs never allowed three matching-token reads"
        );
        let reads = node.test_group_inventory_body_read_count();
        let repair = serde_json::to_value(
            super::super::get_presence(state.clone(), Some(String::new()))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(repair.get("groups").is_some());
        assert!(
            repair["groups"]
                .as_object()
                .unwrap()
                .contains_key(&hex::encode(accepted_group.as_bytes())),
            "forced repair must return the current physical group projection"
        );
        assert_eq!(
            node.test_group_inventory_body_read_count(),
            reads,
            "forced physical-group projection must not hydrate unused inventory"
        );
        // Explicit inventory consumers retain durable reads. Core inventory
        // regressions separately reject missing/corrupt blobs after warming.
        assert!(node.account_document(&inventory_key).unwrap().is_some());
        assert!(
            node.test_group_inventory_body_read_count() > reads,
            "explicit inventory reads must still check the durable body"
        );
        node.set_account_display_name("Changed Diagnostics")
            .unwrap();
        let changed = serde_json::to_value(
            super::super::get_presence(state.clone(), Some(revision))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(
            changed.get("groups").is_some(),
            "profile changes invalidate the group projection hint"
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
        // IPC success promises a queued query, not a reply from an arbitrary
        // service. Own its endpoint and observe the actual wire command.
        let witness = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target_port = witness.local_addr().unwrap().port();
        let probe_app = app.handle().clone();
        let observer = host.observer;
        host.handle
            .spawn(async move {
                assert_ne!(std::thread::current().id(), observer);
                probe_peer_endpoint(
                    probe_app.state::<AppState>(),
                    probe_app.state::<NodeState>(),
                    " 127.0.0.1 ".into(),
                    Some(target_port),
                )
                .await
            })
            .await
            .unwrap()
            .expect("manual IPC must send to its owned loopback witness");
        let mut packet = [0u8; 65];
        let (length, _) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            witness.recv_from(&mut packet),
        )
        .await
        .expect("owned IPC query must reach its requested endpoint")
        .unwrap();
        assert_eq!(length, 64);
        assert!(mesh_talk_core::discovery::query::decode_query(&packet[..length]).is_some());

        // A valid input still preserves the specific Core disclosure denial.
        // An unrelated socket timeout must not satisfy this error control.
        let private_state = managed.clone();
        host.handle
            .spawn(async move {
                private_state
                    .0
                    .read()
                    .await
                    .as_ref()
                    .unwrap()
                    .set_invisible(true)
                    .await
            })
            .await
            .unwrap()
            .unwrap();
        let probe_app = app.handle().clone();
        let denied = host
            .handle
            .spawn(async move {
                probe_peer_endpoint(
                    probe_app.state::<AppState>(),
                    probe_app.state::<NodeState>(),
                    "127.0.0.1".into(),
                    Some(target_port),
                )
                .await
            })
            .await
            .unwrap();
        assert!(
            matches!(denied, Err(CommandError::Io(ref message)) if message.contains("routed discovery is disabled")),
            "manual IPC must preserve the specific disclosure denial: {denied:?}"
        );
        assert!(
            matches!(witness.try_recv_from(&mut packet), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        drop(witness);
        // Each host guard stops and joins its runtime on success and unwind.
        drop(remote_host);
        drop(host);
    }
}
