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
    let observation = authority.observation_lifetime();
    tauri::async_runtime::spawn(async move {
        let _observation = observation;
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
        match mesh_talk_core::node::NodeRuntime::start_configured_guarded_with_network_cost_observed(
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
            mesh_talk_core::node::NetworkCost::Unknown,
            |identity| {
                authority.observe_identity(identity).map_err(|_| {
                    mesh_talk_core::node::RuntimeError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "startup superseded",
                    ))
                })
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
                let _ = authority.current(|_| {
                    if runtime
                        .handle()
                        .set_device_hostname(os_hostname().as_deref())
                        .is_err()
                    {
                        log::warn!("Private device hostname publication failed");
                    }
                });
                // Upgrade an original, still-unlinked desktop profile once. A
                // joining device already has both certified devices and must
                // never seed its previous local nickname into the kept account.
                let _ = authority.current(|info| {
                    if let Err(error) = bind_account_profile(
                        &base_dir,
                        &account_id,
                        &runtime,
                        &info.user.display_name,
                    ) {
                        log::warn!("Account profile binding failed: {error}");
                    }
                });
                // Subscribe before installing/launching the host producer so no
                // invalidation can fall between capturing the receiver and spawn.
                let (installed, installation) = tokio::sync::watch::channel(false);
                let cost_node = runtime.handle();
                runtime.register_host_task(tokio::spawn(after_install(
                    installation.clone(),
                    crate::network_cost::observe(cost_node),
                )));
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
                let (profile_shutdown, profile_stopping) = tokio::sync::oneshot::channel();
                let (login_shutdown, login_stopping) = tokio::sync::oneshot::channel();
                runtime.register_graceful_host_task(
                    login_shutdown,
                    tokio::spawn(after_install(
                        installation.clone(),
                        crate::shared_account_login::reconcile(
                            app_handle.clone(),
                            base_dir.clone(),
                            authority.clone(),
                            integration_state.clone(),
                            node_handle.clone(),
                            login_stopping,
                        ),
                    )),
                );
                runtime.register_graceful_host_task(
                    profile_shutdown,
                    tokio::spawn(after_install(
                        installation.clone(),
                        reconcile_account_profile(
                            app_handle.clone(),
                            authority.clone(),
                            integration_state.clone(),
                            node_handle.clone(),
                            profile_stopping,
                        ),
                    )),
                );
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
                if let Err(error) = crate::integrations::local_access::restore(
                    &integration_state,
                    &node_handle,
                    &integration_lease,
                    &base_dir,
                    std::sync::Arc::new({
                        let handle = app_handle.clone();
                        move |event| {
                            let _ = handle.emit("integration-message-accepted", event);
                        }
                    }),
                )
                .await
                {
                    log::warn!("Desktop integration settings were not applied: {error}");
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

/// A local migration marker records which cryptographic account has already
/// reconciled its legacy desktop nickname. New records also distinguish an
/// adopted account awaiting its source profile from the old string-only marker.
fn bind_account_profile(
    base: &std::path::Path,
    owner: &str,
    runtime: &mesh_talk_core::node::NodeRuntime,
    local_name: &str,
) -> Result<(), crate::commands::CommandError> {
    let path = crate::config_store::account_path(base, owner, "profile-binding.json")
        .ok_or_else(|| crate::commands::CommandError::Validation("invalid profile owner".into()))?;
    let bound: serde_json::Value = crate::config_store::read_account(&path);
    if bound.get("account_id").and_then(serde_json::Value::as_str) == Some(runtime.account_id()) {
        return Ok(());
    }
    // The old marker could mark an already-bound legacy account complete even
    // though no shared profile existed. Recover its established local name once.
    // A fresh adoption never takes this branch: record the new object marker
    // even while its source profile is pending, so restart cannot publish the
    // joining device's unrelated old name ahead of the delayed source card.
    let recover_bound_legacy = bound.as_str() == Some(runtime.account_id())
        && runtime
            .handle()
            .account_profile(runtime.account_id())
            .is_none();
    if recover_bound_legacy
        || (bound.as_str() != Some(runtime.account_id())
            && runtime.handle().linked_devices().len() == 1)
    {
        runtime.handle().set_profile_display_name(local_name)?;
        runtime.set_display_name(local_name);
    }
    crate::config_store::write_account(
        &path,
        &serde_json::json!({ "version": 1, "account_id": runtime.account_id() }),
    )
}

/// The authenticated account profile is authoritative across device linking and restarts.
/// Only nickname needs projection into host credentials; bio/avatar remain core-owned.
async fn reconcile_account_profile<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    authority: RuntimeAuthority,
    state: crate::state::AppState,
    node: crate::chat_commands::NodeState,
    mut stopping: tokio::sync::oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        tokio::select! { _ = &mut stopping => return, _ = interval.tick() => {} }
        let auth = tokio::select! {
            _ = &mut stopping => return,
            auth = state.auth_operation.clone().lock_owned() => auth,
        };
        let guard = tokio::select! { _ = &mut stopping => return, guard = node.0.read() => guard };
        let Some(runtime) = guard.as_ref() else {
            return;
        };
        let Some(card) = runtime.handle().account_profile(runtime.account_id()) else {
            continue;
        };
        let name = card.display_name;
        if mesh_talk_core::identity::user::User::validate_display_name(&name).as_ref()
            != Some(&name)
        {
            continue;
        }
        let Ok(needs_update) = authority.current(|info| {
            if runtime.display_name() != name {
                runtime.set_display_name(&name);
            }
            info.user.display_name != name
        }) else {
            return;
        };
        let identity = (
            runtime.account_id().to_owned(),
            runtime.user_id().to_owned(),
        );
        drop(guard);
        if !needs_update {
            continue;
        }
        let service = state.auth_service().clone();
        let lease = authority.lease.clone();
        let Ok(password) = authority.current(|info| info.password.clone()) else {
            return;
        };
        let updated = tokio::task::spawn_blocking(move || {
            let _auth = auth;
            service.rename_session(&lease, &password, &name)
        })
        .await;
        if let Ok(Ok(user)) = updated {
            let _ = authority.current(|_| {
                let _ = app.emit(
                    "account-profile-changed",
                    serde_json::json!({"user": crate::commands::UserInfo::from(user), "account_id": identity.0, "device_id": identity.1}),
                );
            });
        }
    }
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

/// Read only the OS hostname; never derive private labels from public nicknames.
fn os_hostname() -> Option<String> {
    #[cfg(unix)]
    let value = {
        let mut buffer = [0u8; 256];
        // SAFETY: buffer is writable for the entire supplied length. Require a
        // terminating NUL so a truncated hostname is not published as complete.
        if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
            return None;
        }
        let end = buffer.iter().position(|byte| *byte == 0)?;
        std::str::from_utf8(&buffer[..end]).ok()?.to_owned()
    };
    #[cfg(windows)]
    let value = std::env::var("COMPUTERNAME").ok()?;
    #[cfg(not(any(unix, windows)))]
    let value = String::new();
    (!value.trim().is_empty() && value.len() <= 255 && !value.chars().any(char::is_control))
        .then_some(value)
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
                account_login_name: None,
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

    fn startup_projection(
        session: &crate::state::SessionState,
        node: &crate::chat_commands::NodeState,
        authority: &RuntimeAuthority,
    ) -> serde_json::Value {
        serde_json::to_value(
            node.startup_identity(
                session,
                &authority.lease,
                &crate::app_lock::AppLock::default(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn observed_identity(account: &str) -> mesh_talk_core::node::RuntimeIdentity {
        mesh_talk_core::node::RuntimeIdentity {
            account_id: account.into(),
            device_id: "device".into(),
        }
    }

    #[test]
    fn startup_identity_failure_and_cancellation_clear_only_current_observation() {
        let (session, node, authority) = authority();
        let lifetime = authority.observation_lifetime();
        let pending = startup_projection(&session, &node, &authority);
        assert_eq!(pending["phase"], "pending");
        assert!(pending.get("account_id").is_none());
        authority
            .observe_identity(observed_identity("first-account"))
            .unwrap();
        assert_eq!(
            startup_projection(&session, &node, &authority)["account_id"],
            "first-account"
        );
        drop(lifetime);
        let failed = startup_projection(&session, &node, &authority);
        assert_eq!(failed["phase"], "failed");
        assert!(failed.get("account_id").is_none());
        assert!(failed.get("device_id").is_none());

        let replacement =
            RuntimeAuthority::request(session.clone(), authority.lease.clone(), node.clone())
                .unwrap();
        let replacement_lifetime = replacement.observation_lifetime();
        assert_ne!(
            pending["startup_id"],
            startup_projection(&session, &node, &replacement)["startup_id"]
        );
        replacement
            .observe_identity(observed_identity("adopted-account"))
            .unwrap();
        assert!(
            authority
                .observe_identity(observed_identity("stale-account"))
                .is_err()
        );
        drop(authority.observation_lifetime());
        assert_eq!(
            startup_projection(&session, &node, &replacement)["account_id"],
            "adopted-account"
        );
        node.next_lifecycle_ticket();
        assert!(startup_projection(&session, &node, &replacement).is_null());
        drop(replacement_lifetime);
    }

    #[tokio::test]
    async fn startup_identity_cancelled_while_waiting_for_serialization_clears_pending() {
        let (session, node, authority) = authority();
        let gate = node.test_startup_gate().lock().await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let task_authority = authority.clone();
        let task = tokio::spawn(async move {
            let _observation = task_authority.observation_lifetime();
            entered_tx.send(()).unwrap();
            let _permit = task_authority.begin().await.unwrap();
        });
        entered_rx.await.unwrap();
        task.abort();
        assert!(task.await.is_err());
        assert_eq!(
            startup_projection(&session, &node, &authority)["phase"],
            "failed"
        );
        drop(gate);
    }

    #[test]
    fn startup_identity_rejects_same_owner_replacement_and_signed_out_observers() {
        let (session, node, authority) = authority();
        authority
            .observe_identity(observed_identity("first-account"))
            .unwrap();
        let old_lifetime = authority.observation_lifetime();
        let user = session
            .matching(&authority.lease, |info| info.user.clone())
            .unwrap();
        let replacement_lease = session.publish("new-token".into(), user, "password".into());
        let lock = crate::app_lock::AppLock::default();
        assert!(
            node.startup_identity(&session, &authority.lease, &lock)
                .is_err()
        );
        assert!(
            node.startup_identity(&session, &replacement_lease, &lock)
                .unwrap()
                .is_none()
        );
        assert!(
            authority
                .observe_identity(observed_identity("stale-account"))
                .is_err()
        );
        let replacement =
            RuntimeAuthority::request(session.clone(), replacement_lease, node.clone()).unwrap();
        replacement
            .observe_identity(observed_identity("new-account"))
            .unwrap();
        drop(old_lifetime);
        assert_eq!(
            startup_projection(&session, &node, &replacement)["account_id"],
            "new-account"
        );
        session.clear();
        assert!(
            node.startup_identity(&session, &replacement.lease, &lock)
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_identity_is_observable_before_runtime_installation_and_failure_retires_it() {
        let root = tempfile::tempdir().unwrap();
        // A real durable store open fails after the authenticated observation.
        std::fs::create_dir_all(root.path().join("accounts/host-owner/names.log")).unwrap();
        let (session, node, authority) = authority();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let base = root.path().to_path_buf();
        let task_authority = authority.clone();
        let startup = tokio::spawn(async move {
            let _observation = task_authority.observation_lifetime();
            let _permit = task_authority.begin().await.unwrap();
            mesh_talk_core::node::NodeRuntime::start_configured_guarded_with_network_observed(
                mesh_talk_core::node::RuntimeConfig {
                    base_dir: &base,
                    account_id: "host-owner",
                    display_name: "Owner",
                    password: "pw",
                    discovery_port: 0,
                    post_office_enabled: false,
                    candidate_source: None,
                },
                mesh_talk_core::node::RuntimeEvents {
                    on_dm: Box::new(|_| {}),
                    on_channel: Box::new(|_| {}),
                    on_file: Box::new(|_| {}),
                    on_profile: Box::new(|_| {}),
                    on_call_signal: Box::new(|_| {}),
                },
                mesh_talk_core::node::RuntimeNetworkOptions::default(),
                move |identity| {
                    task_authority.observe_identity(identity).unwrap();
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                |launch| Ok(launch("Owner")),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(15), entered_rx)
            .await
            .unwrap()
            .unwrap();
        let identity = startup_projection(&session, &node, &authority);
        let no_runtime = node.0.read().await.is_none();
        let denied = node.node_for_owner(&session, "host-owner").await.is_none();
        release_tx.send(()).unwrap();
        assert!(startup.await.unwrap().is_err());
        assert_eq!(identity["phase"], "identity");
        assert_eq!(identity["account_id"].as_str().unwrap().len(), 32);
        assert!(
            no_runtime && denied,
            "early observation cannot grant runtime access"
        );
        assert_eq!(
            startup_projection(&session, &node, &authority)["phase"],
            "failed"
        );
    }

    #[tokio::test]
    async fn startup_identity_installation_keeps_ready_until_next_ticket() {
        let root = tempfile::tempdir().unwrap();
        let (session, node, authority) = authority();
        let lifetime = authority.observation_lifetime();
        let permit = authority.begin().await.unwrap();
        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "host-owner",
            "Owner",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        authority
            .observe_identity(mesh_talk_core::node::RuntimeIdentity {
                account_id: runtime.account_id().to_owned(),
                device_id: runtime.user_id().to_owned(),
            })
            .unwrap();
        assert!(permit.install(runtime).await);
        drop(lifetime);
        let installed = startup_projection(&session, &node, &authority);
        let admitted = node.node_for_owner(&session, "host-owner").await.unwrap();
        assert_eq!(installed["phase"], "ready");
        assert_eq!(installed["account_id"], admitted.account_id());
        let replacement =
            RuntimeAuthority::request(session.clone(), authority.lease.clone(), node.clone())
                .unwrap();
        let pending = startup_projection(&session, &node, &replacement);
        assert_eq!(pending["phase"], "pending");
        assert!(pending.get("account_id").is_none());
        assert_ne!(installed["startup_id"], pending["startup_id"]);
        assert!(node.node_for_owner(&session, "host-owner").await.is_none());
        drop(admitted);
        node.0.write().await.take().unwrap().stop().await;
    }

    #[tokio::test]
    async fn signed_account_nickname_persists_emits_and_rejects_retired_owner() {
        let root = tempfile::tempdir().unwrap();
        let auth = crate::services::auth_service::AuthService::new(std::sync::Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        ));
        auth.register(
            "profile-owner".into(),
            "password-a".into(),
            "fixture".into(),
        )
        .unwrap();
        auth.login("profile-owner".into(), "password-a".into())
            .unwrap();
        let state = crate::state::AppState::new(auth.clone());
        let node = crate::chat_commands::NodeState::empty();
        let lease = state.session().capture().unwrap();
        let authority =
            RuntimeAuthority::request(state.session().clone(), lease.clone(), node.clone())
                .unwrap();
        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            lease.owner(),
            "profile-owner",
            "password-a",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let handle = runtime.handle();
        assert!(authority.begin().await.unwrap().install(runtime).await);
        handle
            .save_account_bio_if("Bio without a nickname".into(), |commit| commit())
            .await
            .unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        app.listen("account-profile-changed", move |event| {
            let _ =
                events.send(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        let (_shutdown, stopping) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(reconcile_account_profile(
            app.handle().clone(),
            authority.clone(),
            state.clone(),
            node.clone(),
            stopping,
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), received.recv())
                .await
                .is_err()
        );
        assert_eq!(
            node.0.read().await.as_ref().unwrap().display_name(),
            "profile-owner"
        );
        handle
            .set_profile_display_name("Shared account nickname")
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event["user"]["display_name"], "Shared account nickname");
        assert_eq!(
            state.session().get().unwrap().user.display_name,
            "Shared account nickname"
        );
        assert_eq!(
            node.0.read().await.as_ref().unwrap().display_name(),
            "Shared account nickname"
        );
        // Retiring the authority prevents further persistence or frontend publication.
        state.session().clear();
        handle.set_profile_display_name("Retired change").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert!(received.try_recv().is_err());
        let (relogged, _) = auth
            .login("profile-owner".into(), "password-a".into())
            .unwrap();
        assert_eq!(relogged.display_name, "Shared account nickname");
        node.0.write().await.take().unwrap().stop().await;
    }

    #[tokio::test]
    async fn legacy_profile_binding_migrates_once_and_preserves_signed_bio() {
        let root = tempfile::tempdir().unwrap();
        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "owner",
            "Original",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        runtime
            .handle()
            .save_account_profile_if(
                "Old signed name".into(),
                "Personal signature".into(),
                |commit| commit(),
            )
            .await
            .unwrap();
        bind_account_profile(root.path(), "owner", &runtime, "Current desktop nickname").unwrap();
        let card = runtime
            .handle()
            .account_profile(runtime.account_id())
            .unwrap();
        assert_eq!(card.display_name, "Current desktop nickname");
        assert_eq!(card.bio, "Personal signature");
        runtime
            .handle()
            .set_profile_display_name("New signed name")
            .unwrap();
        bind_account_profile(root.path(), "owner", &runtime, "Stale desktop nickname").unwrap();
        assert_eq!(
            runtime
                .handle()
                .account_profile(runtime.account_id())
                .unwrap()
                .display_name,
            "New signed name"
        );
        runtime.stop().await;
    }

    #[tokio::test]
    async fn legacy_bound_linked_account_without_card_recovers_its_existing_nickname() {
        let root = tempfile::tempdir().unwrap();
        let runtime = legacy_linked_runtime_without_card(root.path()).await;
        let marker =
            crate::config_store::account_path(root.path(), "owner", "profile-binding.json")
                .unwrap();
        crate::config_store::write_account(&marker, &runtime.account_id()).unwrap();
        bind_account_profile(root.path(), "owner", &runtime, "Kept Mac nickname").unwrap();
        assert_eq!(
            runtime
                .handle()
                .account_profile(runtime.account_id())
                .map(|card| card.display_name),
            Some("Kept Mac nickname".into())
        );
        runtime.stop().await;
    }

    #[tokio::test]
    async fn joining_account_without_card_never_seeds_its_previous_name_on_repeated_startup() {
        let root = tempfile::tempdir().unwrap();
        let runtime = legacy_linked_runtime_without_card(root.path()).await;
        for _ in 0..2 {
            bind_account_profile(root.path(), "owner", &runtime, "Unrelated joining name").unwrap();
            assert!(
                runtime
                    .handle()
                    .account_profile(runtime.account_id())
                    .is_none()
            );
        }
        runtime
            .handle()
            .set_profile_display_name("Imported kept account")
            .unwrap();
        bind_account_profile(root.path(), "owner", &runtime, "Unrelated joining name").unwrap();
        assert_eq!(
            runtime
                .handle()
                .account_profile(runtime.account_id())
                .unwrap()
                .display_name,
            "Imported kept account"
        );
        runtime.stop().await;
    }

    #[tokio::test]
    async fn first_bio_command_repairs_a_legacy_linked_account_without_a_profile() {
        let root = tempfile::tempdir().unwrap();
        let runtime = legacy_linked_runtime_without_card(root.path()).await;
        let account_id = runtime.account_id().to_string();
        let handle = runtime.handle();
        assert!(handle.account_profile(&account_id).is_none());
        let auth = crate::services::auth_service::AuthService::new(std::sync::Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        ));
        let state = crate::state::AppState::new(auth);
        let lease = state.session().publish(
            "token".into(),
            crate::services::user::User {
                user_id: "owner".into(),
                account_login_name: None,
                name: "login-name".into(),
                display_name: "Existing local name".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "pw".into(),
        );
        let node = crate::chat_commands::NodeState::empty();
        let authority =
            RuntimeAuthority::request(state.session().clone(), lease, node.clone()).unwrap();
        assert!(authority.begin().await.unwrap().install(runtime).await);
        let app = tauri::test::mock_builder()
            .manage(state.clone())
            .manage(node.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        assert!(
            crate::account_profile::save_account_bio(
                app.state(),
                app.state(),
                "other-owner".into(),
                "Foreign edit".into(),
            )
            .await
            .is_err()
        );
        assert!(handle.account_profile(&account_id).is_none());
        crate::account_profile::save_account_bio(
            app.state(),
            app.state(),
            "owner".into(),
            "New signature".into(),
        )
        .await
        .unwrap();
        let profile = handle.account_profile(&account_id).unwrap();
        assert_eq!(profile.display_name, "Existing local name");
        assert_eq!(profile.bio, "New signature");
        state.session().clear();
        assert!(
            crate::account_profile::save_account_bio(
                app.state(),
                app.state(),
                "owner".into(),
                "Retired edit".into(),
            )
            .await
            .is_err()
        );
        assert_eq!(handle.account_profile(&account_id), Some(profile));
        node.0.write().await.take().unwrap().stop().await;
    }

    async fn legacy_linked_runtime_without_card(
        root: &std::path::Path,
    ) -> mesh_talk_core::node::NodeRuntime {
        let start = || {
            mesh_talk_core::node::NodeRuntime::start(
                root,
                "owner",
                "Existing local name",
                "pw",
                0,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            )
        };
        let runtime = start().await.unwrap();
        let account_id = runtime.account_id().to_owned();
        runtime.stop().await;
        let directory = root.join("accounts/owner");
        for file in ["trust-cards.log", "account-profiles.log"] {
            let path = directory.join(file);
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
        let account = mesh_talk_core::identity::account_keystore::load(
            &directory.join("account.keystore"),
            "pw",
        )
        .unwrap();
        let other = mesh_talk_core::identity::device::DeviceIdentity::generate().public();
        let (mut roster, _) = mesh_talk_core::storage::record_log::EncryptedRecordLog::<
            mesh_talk_core::node::AccountDevice,
        >::open(
            &directory.join(format!("account-devices-{account_id}.log")),
            "pw",
            b"MTACDV",
        )
        .unwrap();
        roster
            .append(&mesh_talk_core::node::AccountDevice {
                certificate: account.certify(&other.ed25519_pub),
                public: other,
            })
            .unwrap();
        drop(roster);
        let runtime = start().await.unwrap();
        assert_eq!(runtime.handle().linked_devices().len(), 2);
        assert!(
            runtime
                .handle()
                .account_profile(runtime.account_id())
                .is_none()
        );
        runtime
    }

    #[tokio::test]
    async fn adopted_profile_binding_cannot_publish_the_joining_local_nickname() {
        let root = tempfile::tempdir().unwrap();
        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "owner",
            "Kept account",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let account_id = runtime.account_id().to_owned();
        runtime.stop().await;
        // The pairing owner persists source + joining certificates before the
        // target account keystore can be adopted. Recreate that durable boundary.
        let directory = root.path().join("accounts/owner");
        let account = mesh_talk_core::identity::account_keystore::load(
            &directory.join("account.keystore"),
            "pw",
        )
        .unwrap();
        let other = mesh_talk_core::identity::device::DeviceIdentity::generate().public();
        let (mut roster, _) = mesh_talk_core::storage::record_log::EncryptedRecordLog::<
            mesh_talk_core::node::AccountDevice,
        >::open(
            &directory.join(format!("account-devices-{account_id}.log")),
            "pw",
            b"MTACDV",
        )
        .unwrap();
        roster
            .append(&mesh_talk_core::node::AccountDevice {
                certificate: account.certify(&other.ed25519_pub),
                public: other,
            })
            .unwrap();
        drop(roster);
        let joined = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "owner",
            "Joining local nickname",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(joined.handle().linked_devices().len(), 2);
        bind_account_profile(root.path(), "owner", &joined, "Joining local nickname").unwrap();
        assert_eq!(
            joined
                .handle()
                .account_profile(&account_id)
                .unwrap()
                .display_name,
            "Kept account"
        );
        assert_eq!(joined.display_name(), "Kept account");
        joined.stop().await;
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
