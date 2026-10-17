//! Starts the account-scoped node runtime and publishes its inbound events.
//! Session authority and runtime installation stay together at this boundary.

use crate::chat_commands::RuntimeAuthority;
use tauri::Manager;

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
                        crate::events::emit_dm_received(
                            &app_handle_for_dm,
                            dm.from,
                            dm.from_name,
                            dm.text,
                            dm.reply_to,
                        );
                    });
                }),
                on_channel: Box::new(move |msg: mesh_talk_core::node::ReceivedChannelMessage| {
                    let _ = channel_authority.current(|_| {
                        crate::events::emit_channel_message(
                            &app_handle_for_channel,
                            hex::encode(msg.channel_id.as_bytes()),
                            msg.channel_name,
                            msg.from,
                            msg.text,
                            msg.reply_to,
                        );
                    });
                }),
                on_file: Box::new(move |f: mesh_talk_core::node::ReceivedFile| {
                    let _ = file_authority.current(|_| {
                        crate::events::emit_file_received(
                            &app_handle_for_file,
                            hex::encode(f.conv.as_bytes()),
                            f.is_channel,
                            f.from,
                            f.name,
                            f.size,
                            f.mime,
                            hex::encode(f.file_conv.as_bytes()),
                            f.media,
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
            Ok(runtime) => {
                if !permit.install(runtime).await {
                    return;
                }
                // Serialize with mark_verified so startup cannot replace a newer
                // in-memory allowlist with an older trust-file snapshot. Read
                // the relay preference under the same runtime lock so startup
                // cannot overwrite a setting changed after publication.
                let guard = node_handle.0.lock().await;
                if let Some(runtime) = guard
                    .as_ref()
                    .filter(|runtime| runtime.host_account_id() == Some(&account_id))
                {
                    let node = runtime.handle();
                    if let Some(path) =
                        crate::config_store::account_path(&base_dir, &account_id, "trust.json")
                    {
                        node.replace_verified_dm_pull_peers(
                            crate::trust::verified_fingerprints_at(&path),
                        );
                    }
                    if let Ok(settings) = app_handle
                        .state::<crate::settings::SettingsState>()
                        .get_for_owner(&account_id)
                        && let Err(error) =
                            crate::settings::apply_relay_settings(&node, &settings).await
                    {
                        log::warn!("Ignored invalid bridge hub setting: {error}");
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
