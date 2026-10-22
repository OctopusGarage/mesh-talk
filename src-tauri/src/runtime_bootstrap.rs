//! Starts the account-scoped node runtime and publishes its inbound events.
//! Session authority and runtime installation stay together at this boundary.

use crate::chat_commands::RuntimeAuthority;
use tauri::{Emitter, Manager};

// ---------------------------------------------------------------------------
// Node lifecycle bridge
// ---------------------------------------------------------------------------

/// Startup errors can embed the account directory. Redact every occurrence at
/// the logging boundary while preserving the error category and remaining context.
pub(crate) fn node_start_error_for_log(
    error: &mesh_talk_core::node::RuntimeError,
    account_id: &str,
) -> String {
    let diagnostic = error.to_string();
    if account_id.is_empty() {
        diagnostic
    } else {
        diagnostic.replace(account_id, "[account]")
    }
}

/// Spawn the node runtime in the background, wiring its inbound callbacks to the
/// app's Tauri events, and store it in `node_handle`. Shared by login and by account
/// adoption after device linking (which drops the old runtime and re-spawns; `start`
/// reloads the account keystore, so a re-spawn adopts a freshly-linked account secret).
/// `account_id` is the host-app namespace for the data directory — distinct from the
/// node's cryptographic account.
pub(crate) fn spawn_node_runtime<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    lease: crate::state::SessionLease,
    password: String,
    node_handle: crate::chat_commands::NodeState,
    session: crate::state::SessionState,
) {
    let integration_lease = lease.clone();
    let integration_state = app_handle.state::<crate::state::AppState>().inner().clone();
    let Ok(authority) = RuntimeAuthority::request(session, lease, node_handle.clone()) else {
        return;
    };
    let dm_authority = authority.clone();
    let channel_authority = authority.clone();
    let file_authority = authority.clone();
    let profile_authority = authority.clone();
    let call_authority = authority.clone();
    let app_handle_for_dm = app_handle.clone();
    let app_handle_for_channel = app_handle.clone();
    let app_handle_for_file = app_handle.clone();
    let app_handle_for_profile = app_handle.clone();
    let app_handle_for_call = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(permit) = authority.begin().await else {
            return;
        };
        let display_name = permit.display_name.clone();
        let account_id = authority.lease.owner().to_owned();
        // The base directory for node per-account data (the app's `~/.mesh-talk`).
        #[cfg(test)]
        let (base_dir, discovery_port) = node_handle
            .4
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| (crate::data_dir(), crate::configured_discovery_port()));
        #[cfg(not(test))]
        let (base_dir, discovery_port) = (crate::data_dir(), crate::configured_discovery_port());
        let relay_settings = app_handle
            .state::<crate::settings::SettingsState>()
            .get_for_owner(&account_id)
            .unwrap_or_else(|_| crate::settings::AppSettings {
                post_office_enabled: false,
                ..Default::default()
            });
        match mesh_talk_core::node::NodeRuntime::start_configured_guarded_with_network(
            mesh_talk_core::node::RuntimeConfig {
                base_dir: &base_dir,
                account_id: &account_id,
                display_name: &display_name,
                password: &password,
                discovery_port,
                post_office_enabled: relay_settings.post_office_enabled,
                #[cfg(not(test))]
                candidate_source: Some(crate::routed_discovery::desktop_candidates(discovery_port)),
                #[cfg(test)]
                candidate_source: None,
            },
            mesh_talk_core::node::RuntimeEvents {
                on_dm: Box::new(move |dm| {
                    let _ = dm_authority.current(|_| {
                        crate::events::emit_dm_received(&app_handle_for_dm, dm);
                    });
                }),
                on_channel: Box::new(move |msg: mesh_talk_core::node::ReceivedChannelMessage| {
                    let _ = channel_authority.current(|_| {
                        crate::events::emit_channel_message(
                            &app_handle_for_channel,
                            &format!(
                                "{}:{}:{}",
                                channel_authority.lease.owner(),
                                channel_authority.lease.generation(),
                                channel_authority.ticket
                            ),
                            msg,
                        );
                    });
                }),
                on_file: Box::new(move |f: mesh_talk_core::node::ReceivedFile| {
                    let _ = file_authority.current(|_| {
                        crate::events::emit_file_received(
                            &app_handle_for_file,
                            &format!(
                                "{}:{}:{}",
                                file_authority.lease.owner(),
                                file_authority.lease.generation(),
                                file_authority.ticket
                            ),
                            f,
                        );
                    });
                }),
                on_profile: Box::new(move |p: mesh_talk_core::node::ReceivedProfile| {
                    let _ = profile_authority.current(|_| {
                        crate::events::emit_profile_received(
                            &app_handle_for_profile,
                            p.account_id,
                            p.avatar,
                        );
                    });
                }),
                on_call_signal: Box::new(move |s: mesh_talk_core::node::ReceivedCallSignal| {
                    let _ = call_authority.current(|_| {
                        crate::events::emit_call_signal(&app_handle_for_call, s.from, s.payload);
                    });
                }),
            },
            mesh_talk_core::node::RuntimeNetworkOptions {
                post_office_send_enabled: relay_settings.post_office_send_enabled,
                post_office_receive_enabled: relay_settings.post_office_receive_enabled,
                physical_lan_only: relay_settings.physical_lan_only,
            },
            |launch| {
                authority
                    .current(|info| launch(&info.user.display_name))
                    .map_err(|_| {
                        mesh_talk_core::node::RuntimeError::Io(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "session replaced",
                        ))
                    })
            },
        )
        .await
        {
            Ok(mut runtime) => {
                // Subscribe before installing/launching the host producer so no
                // invalidation can fall between capturing the receiver and spawn.
                let (installed, installation) = tokio::sync::watch::channel(false);
                let changes = runtime.handle().subscribe_channel_changes();
                let crypto_account_id = runtime.account_id().to_owned();
                let device_id = runtime.user_id().to_owned();
                runtime.register_host_task(tokio::spawn(after_install(
                    installation.clone(),
                    forward_channel_changes(
                        app_handle.clone(),
                        authority.clone(),
                        crypto_account_id,
                        device_id,
                        changes,
                    ),
                )));
                let handle = runtime.handle();
                for (kind, changes) in [
                    ("state", handle.subscribe_ui_changes()),
                    ("files", handle.subscribe_file_changes()),
                    ("roster", handle.subscribe_roster_changes()),
                ] {
                    runtime.register_host_task(tokio::spawn(after_install(
                        installation.clone(),
                        forward_state_changes(
                            app_handle.clone(),
                            authority.clone(),
                            runtime.account_id().to_owned(),
                            runtime.user_id().to_owned(),
                            kind,
                            changes,
                        ),
                    )));
                }
                let app = app_handle.clone();
                let status_authority = authority.clone();
                let account = runtime.account_id().to_owned();
                let device = runtime.user_id().to_owned();
                runtime.register_host_task(tokio::spawn(after_install(installation, async move {
                    loop {
                        handle.delivery_status_notified().await;
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        if status_authority
                            .current(|_| {
                                crate::events::emit_node_state_changed(
                                    &app,
                                    crate::events::NodeStateChangedEvent {
                                        owner: status_authority.lease.owner().to_owned(),
                                        account_id: account.clone(),
                                        device_id: device.clone(),
                                        kind: "delivery",
                                    },
                                )
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                })));
                if !permit.install(runtime).await {
                    return;
                }
                installed.send_replace(true);
                // Keep startup's exact session and installation authority through
                // trust publication and every relay-policy commit after a wait.
                let Ok(admission) = crate::runtime_admission::RuntimeAdmission::capture_lease(
                    &integration_state,
                    &node_handle,
                    &integration_lease,
                ) else {
                    return;
                };
                if admission
                    .with_runtime(|runtime| {
                        if let Some(path) =
                            crate::config_store::account_path(&base_dir, &account_id, "trust.json")
                        {
                            runtime.handle().replace_verified_dm_pull_peers(
                                crate::trust::verified_fingerprints_at(&path),
                            );
                        }
                        Ok(())
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                if let Err(error) = crate::settings::apply_startup_settings(
                    admission,
                    app_handle
                        .state::<crate::settings::SettingsState>()
                        .inner()
                        .clone(),
                )
                .await
                {
                    log::warn!("Startup relay settings were not applied: {error}");
                }
                if std::env::var_os("MESH_TALK_INTEGRATIONS").as_deref()
                    == Some(std::ffi::OsStr::new("1"))
                {
                    let mut guard = node_handle.0.write().await;
                    if let Some(runtime) = guard
                        .as_mut()
                        .filter(|runtime| runtime.host_account_id() == Some(&account_id))
                        && let Err(error) = crate::integrations::start(
                            &integration_state,
                            &node_handle,
                            &integration_lease,
                            runtime,
                            &base_dir,
                            &password,
                            std::sync::Arc::new({
                                let handle = app_handle.clone();
                                move |event| {
                                    let _ = handle.emit("integration-message-accepted", event);
                                }
                            }),
                        )
                        .await
                    {
                        log::warn!("Desktop integrations failed to start: {}", error.code);
                    }
                }
                // Keep lifecycle diagnostics without persisting an account identifier.
                log::info!("Node started");
            }
            Err(e) => log::warn!(
                "Node failed to start: {}",
                node_start_error_for_log(&e, &account_id)
            ),
        }
    });
}

/// A runtime-owned producer: exact startup authority protects each synchronous
/// emit, and retirement cancels/joins the task with the other host producers.
async fn forward_channel_changes<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    authority: RuntimeAuthority,
    account_id: String,
    device_id: String,
    mut changes: tokio::sync::broadcast::Receiver<mesh_talk_core::eventlog::event::ConversationId>,
) {
    loop {
        let channel_id = match changes.recv().await {
            Ok(id) => Some(hex::encode(id.as_bytes())),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        if authority
            .current(|_| {
                crate::events::emit_channel_changed(
                    &app,
                    crate::events::ChannelChangedEvent {
                        owner: authority.lease.owner().to_owned(),
                        account_id: account_id.clone(),
                        device_id: device_id.clone(),
                        channel_id,
                    },
                );
            })
            .is_err()
        {
            return;
        }
    }
}

async fn after_install(
    mut ready: tokio::sync::watch::Receiver<bool>,
    operation: impl std::future::Future<Output = ()>,
) {
    if ready.wait_for(|installed| *installed).await.is_err() {
        return;
    }
    operation.await;
}

async fn forward_state_changes<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    authority: RuntimeAuthority,
    account: String,
    device: String,
    kind: &'static str,
    mut changes: tokio::sync::watch::Receiver<u64>,
) {
    while changes.changed().await.is_ok() {
        tokio::time::sleep(std::time::Duration::from_millis(if kind == "roster" {
            1000
        } else {
            100
        }))
        .await;
        changes.borrow_and_update();
        if authority
            .current(|_| {
                crate::events::emit_node_state_changed(
                    &app,
                    crate::events::NodeStateChangedEvent {
                        owner: authority.lease.owner().to_owned(),
                        account_id: account.clone(),
                        device_id: device.clone(),
                        kind,
                    },
                )
            })
            .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::eventlog::event::ConversationId;
    use tauri::Listener;

    fn authority() -> (
        crate::state::SessionState,
        crate::chat_commands::NodeState,
        RuntimeAuthority,
    ) {
        let session = crate::state::SessionState::default();
        let node = crate::chat_commands::NodeState::empty();
        let lease = session.publish(
            "token".into(),
            crate::services::user::User {
                user_id: "host-owner".into(),
                name: "owner".into(),
                display_name: "Owner".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "password".into(),
        );
        let authority = RuntimeAuthority::request(session.clone(), lease, node.clone()).unwrap();
        (session, node, authority)
    }

    #[tokio::test]
    async fn host_producers_wait_for_successful_installation() {
        let (installed, ready) = tokio::sync::watch::channel(false);
        let (produced, mut receiver) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(after_install(ready, async move {
            produced.send(()).await.unwrap();
        }));
        tokio::task::yield_now().await;
        assert!(receiver.try_recv().is_err());
        installed.send_replace(true);
        task.await.unwrap();
        assert!(receiver.try_recv().is_ok());
        let (installation, ready) = tokio::sync::watch::channel(false);
        drop(installation);
        after_install(ready, async {
            panic!("failed installation must not run host producers")
        })
        .await;
    }

    #[tokio::test]
    async fn state_invalidations_coalesce_and_stop_after_authority_replacement() {
        let (session, node, authority) = authority();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        app.listen("node-state-changed", move |event| {
            let _ =
                events.send(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        let (changes, receiver) = tokio::sync::watch::channel(0);
        let task = tokio::spawn(forward_state_changes(
            app.handle().clone(),
            authority.clone(),
            "account".into(),
            "device".into(),
            "files",
            receiver,
        ));
        for generation in 1..=100 {
            changes.send_replace(generation);
        }
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            event,
            serde_json::json!({"owner":"host-owner", "account_id":"account", "device_id":"device", "kind":"files"})
        );
        assert!(received.try_recv().is_err());
        let _replacement =
            RuntimeAuthority::request(session, authority.lease.clone(), node).unwrap();
        changes.send_replace(101);
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(received.try_recv().is_err());
    }

    #[tokio::test]
    async fn channel_changes_emit_scoped_invalidations_and_stop_after_runtime_replacement() {
        let (session, node, authority) = authority();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        app.listen("channel-changed", move |event| {
            events
                .send(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap())
                .unwrap();
        });
        let (changes, receiver) = tokio::sync::broadcast::channel(4);
        let task = tokio::spawn(forward_channel_changes(
            app.handle().clone(),
            authority.clone(),
            "crypto-account".into(),
            "device".into(),
            receiver,
        ));
        changes.send(ConversationId::new([7; 32])).unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            event,
            serde_json::json!({"owner":"host-owner", "account_id":"crypto-account",
            "device_id":"device", "channel_id":hex::encode([7; 32])})
        );
        let _replacement =
            RuntimeAuthority::request(session, authority.lease.clone(), node).unwrap();
        changes.send(ConversationId::new([8; 32])).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(received.try_recv().is_err());
    }

    #[tokio::test]
    async fn channel_changes_resynchronize_after_lag_and_reject_logged_out_authority() {
        let (session, _, authority) = authority();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        app.listen("channel-changed", move |event| {
            events
                .send(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap())
                .unwrap();
        });
        let (changes, receiver) = tokio::sync::broadcast::channel(1);
        changes.send(ConversationId::new([1; 32])).unwrap();
        changes.send(ConversationId::new([2; 32])).unwrap();
        let task = tokio::spawn(forward_channel_changes(
            app.handle().clone(),
            authority,
            "crypto-account".into(),
            "device".into(),
            receiver,
        ));
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(event["channel_id"].is_null());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        session.clear();
        changes.send(ConversationId::new([3; 32])).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(received.try_recv().is_err());
    }
}
