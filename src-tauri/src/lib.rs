// Pragmatic, intentional lint allowances (the substantive clippy lints stay on):
//  - module_inception: test modules are `mod tests` inside `*/tests.rs`.
//  - too_many_arguments: a few constructors; refactor tracked separately.
//  - assertions_on_constants: placeholder "can construct" smoke tests.
//  - single_match: a couple of intentional single-arm matches.
//  - manual_flatten: a nested `if let Ok` loop kept for readability.
#![allow(
    clippy::module_inception,
    clippy::too_many_arguments,
    clippy::assertions_on_constants,
    clippy::single_match,
    clippy::manual_flatten
)]

// Protocol/core modules live in the `mesh_talk_core` crate; this crate is the
// Tauri desktop shell over it.
pub mod avatars;
pub mod chat_commands;
pub mod commands;
pub mod config_store;
pub mod diagnostics;
pub mod events;
pub mod favorites;
pub mod logger;
pub mod perf;
pub mod services;
pub mod session_store;
pub mod settings;
pub mod state;
pub mod tray;
pub mod trust;

use crate::settings::SettingsState;
use crate::state::AppState;
use std::sync::Arc;
use tauri::{Manager, WindowEvent};

/// The user's home directory, cross-platform: `HOME` on Unix/macOS, `USERPROFILE` on
/// Windows (where `HOME` is normally unset). Anchors the app's data + logs at `~/.mesh-talk`.
pub(crate) fn user_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// The app's data directory (`~/.mesh-talk`, falling back to `./.mesh-talk` if no home
/// dir is resolvable). Single source of truth for the keystore, node, and logs locations.
pub(crate) fn data_dir() -> std::path::PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".mesh-talk")
}

/// Tauri application entry point. The serverless node is the whole product now;
/// it starts per-session on login (see `commands::login` → `spawn_node_runtime`).
pub fn run_tauri() {
    let _timer = perf_monitor!("application_startup");
    log::info!("Starting Mesh-Talk desktop runtime");

    // Data directory + file manager (~/.mesh-talk), shared by the auth keystore.
    lazy_static::lazy_static! {
        static ref FILE_MANAGER: Arc<mesh_talk_core::storage::file_manager::FileManager> = {
            let data_path = data_dir();
            log::info!("Data path: {}", data_path.display());
            Arc::new(mesh_talk_core::storage::file_manager::FileManager::new(data_path))
        };
    }
    let _file_manager = FILE_MANAGER.clone();

    // Auth (login/register) is the only stateful service the shell needs; the
    // node manages its own per-account stores out of `NodeState`.
    crate::services::auth_service::AuthService::init_global(FILE_MANAGER.as_ref().clone());
    let app_state = AppState::new(crate::services::auth_service::AuthService::global().clone());

    log::info!("No sockets are bound until a user signs in.");

    let settings_state = SettingsState::default();
    let trust_state = crate::trust::TrustState::default();
    let favorites_state = crate::favorites::FavoritesState::default();
    let avatars_state = crate::avatars::AvatarsState::default();

    tauri::Builder::default()
        // Single-instance MUST be the first plugin registered (Tauri requirement): its
        // callback runs in the already-running process when a second launch is attempted,
        // so we surface the existing window instead of spawning a duplicate that would
        // fight over the keystore + discovery port.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        // Restore window size/position across launches (clamps off-screen geometry).
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        // Launch-at-login. `--hidden` is passed on autostart so a login-time launch
        // comes up straight to the tray (see the WindowEvent handler / frontend boot).
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .setup(move |app| {
            if let Err(e) = crate::logger::init_logging(app.handle()) {
                log::error!("Failed to initialize logging: {e}");
            }
            // Seed the managed settings from disk before any window/notification logic runs.
            crate::settings::load_into_state(&app.handle().clone(), &app.state::<SettingsState>());
            crate::trust::load_into_state(
                &app.handle().clone(),
                &app.state::<crate::trust::TrustState>(),
            );
            crate::favorites::load_into_state(
                &app.handle().clone(),
                &app.state::<crate::favorites::FavoritesState>(),
            );
            crate::avatars::load_into_state(
                &app.handle().clone(),
                &app.state::<crate::avatars::AvatarsState>(),
            );
            crate::tray::create_system_tray(&app.handle().clone())?;

            // Chat-history retention: one app-lifetime task that periodically erases messages
            // older than the configured window. Spawned once here (not per-login) so tasks
            // don't accumulate across login/logout; it's a no-op while logged out (no node) or
            // when retention is off. An immediate prune on a tightened window is handled in
            // `set_app_settings`.
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager;
                    loop {
                        let days = app_handle.state::<SettingsState>().get().retention_days;
                        if let Some(cutoff) = crate::settings::retention_cutoff_ms(days) {
                            let node = {
                                let node_state =
                                    app_handle.state::<crate::chat_commands::NodeState>();
                                let guard = node_state.0.lock().await;
                                guard.as_ref().map(|rt| rt.handle())
                            };
                            if let Some(node) = node {
                                let _ = tokio::task::spawn_blocking(move || {
                                    node.prune_older_than(cutoff)
                                })
                                .await;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    }
                });
            }

            // Frameless chrome off macOS: macOS uses the native overlay title bar (traffic
            // lights float over our content via `titleBarStyle: Overlay`), but Windows/Linux
            // would otherwise show the traditional title bar. Drop their native decorations
            // so the app provides its own (custom window controls live in the frontend).
            #[cfg(not(target_os = "macos"))]
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_decorations(false);
            }

            // A login-time autostart launch should come up hidden to the tray.
            if std::env::args().any(|a| a == "--hidden") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            #[cfg(debug_assertions)]
            {
                let window = app.get_webview_window("main").unwrap();
                window.open_devtools();
                window.close_devtools();
            }
            Ok(())
        })
        // Close-to-tray: if "minimize to tray" is on, hide instead of exiting so the
        // node runtime stays alive and keeps receiving. Quit (tray menu) is the real exit.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let hide = window
                        .app_handle()
                        .try_state::<SettingsState>()
                        .map(|s| s.get().minimize_to_tray)
                        .unwrap_or(true);
                    if hide {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .manage(app_state)
        .manage(settings_state)
        .manage(trust_state)
        .manage(favorites_state)
        .manage(avatars_state)
        .manage(crate::chat_commands::NodeState::empty())
        .invoke_handler(tauri::generate_handler![
            commands::login,
            commands::logout,
            commands::register,
            commands::rename_account,
            commands::auto_login,
            commands::clear_saved_session,
            commands::adopt_linked_account,
            crate::chat_commands::my_id,
            crate::chat_commands::list_peers,
            crate::chat_commands::send_dm,
            crate::chat_commands::history,
            crate::chat_commands::account_id,
            crate::chat_commands::publish_avatar,
            crate::chat_commands::peer_avatars,
            crate::chat_commands::send_to_account,
            crate::chat_commands::account_history,
            crate::chat_commands::start_linking,
            crate::chat_commands::stop_linking,
            crate::chat_commands::link_device,
            crate::chat_commands::rekey_account,
            crate::chat_commands::list_accounts,
            crate::chat_commands::send_file_to_account,
            crate::chat_commands::react_account,
            crate::chat_commands::account_reactions,
            crate::chat_commands::list_channels,
            crate::chat_commands::create_channel,
            crate::chat_commands::add_channel_member,
            crate::chat_commands::remove_channel_member,
            crate::chat_commands::channel_members,
            crate::chat_commands::send_channel_message,
            crate::chat_commands::channel_history,
            crate::chat_commands::send_file_dm,
            crate::chat_commands::send_file_channel,
            crate::chat_commands::save_file,
            crate::chat_commands::save_file_to_dir,
            crate::chat_commands::safety_number,
            crate::chat_commands::read_file,
            crate::chat_commands::read_media,
            crate::chat_commands::react_dm,
            crate::chat_commands::react_channel,
            crate::chat_commands::reactions,
            crate::chat_commands::channel_reactions,
            crate::chat_commands::search,
            crate::chat_commands::delete_message,
            crate::chat_commands::recall_message,
            crate::chat_commands::clear_conversation,
            crate::chat_commands::send_sticker,
            crate::chat_commands::diag_get_peers,
            crate::chat_commands::diag_network_info,
            crate::chat_commands::get_presence,
            crate::chat_commands::rescan_peers,
            crate::chat_commands::write_temp_file,
            crate::chat_commands::capture_screen,
            crate::chat_commands::set_badge,
            crate::chat_commands::network_name,
            crate::logger::get_logs_dir,
            crate::logger::get_log_file,
            crate::logger::read_log_tail,
            crate::logger::save_log_tail,
            crate::diagnostics::env_info,
            crate::settings::get_app_settings,
            crate::settings::set_app_settings,
            crate::trust::get_trust,
            crate::trust::mark_verified,
            crate::favorites::get_favorites,
            crate::favorites::set_favorite,
            crate::favorites::set_alias,
            crate::avatars::get_avatars,
            crate::avatars::set_avatar
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            // A failed launch must be a non-zero exit, not a silent success.
            log::error!("Error while running tauri application: {e}");
            std::process::exit(1);
        });
}
