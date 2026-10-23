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
pub mod app_lock;
pub mod avatars;
mod background_command;
mod channel_reception;
pub mod chat_commands;
pub mod commands;
pub mod config_store;
pub mod contact_policy;
mod desktop_session;
pub mod diagnostics;
mod easytier_discovery;
pub mod events;
pub mod favorites;
mod integrations;
mod linked_account;
pub mod logger;
pub mod message_favorites;
mod notifications;
pub mod owner_commands;
pub mod perf;
pub mod privacy_commands;
pub mod recovery;
mod routed_discovery;
mod runtime_admission;
mod runtime_bootstrap;
pub mod services;
pub mod session_store;
pub mod settings;
pub mod state;
mod tailscale_discovery;
pub mod tray;
pub mod trust;
pub mod trust_cards;
pub mod updates;
mod window_geometry;
mod window_visibility;

use crate::settings::SettingsState;
use crate::state::AppState;
use std::sync::Arc;
use tauri::{Manager, WindowEvent};

// Set only through the explicitly configured entry point used by the native
// evaluation example. Ordinary application startup never sets this value.
static RUNTIME_DATA_ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
static RUNTIME_DISCOVERY_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

pub(crate) fn configured_discovery_port() -> u16 {
    RUNTIME_DISCOVERY_PORT
        .get()
        .copied()
        .unwrap_or(mesh_talk_core::node::DEFAULT_DISCOVERY_PORT)
}

pub(crate) fn configured_data_root() -> Option<&'static std::path::PathBuf> {
    RUNTIME_DATA_ROOT.get()
}

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
    if let Some(root) = configured_data_root() {
        return root.clone();
    }
    user_home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".mesh-talk")
}

/// Tauri application entry point. The serverless node is the whole product now;
/// it starts per-session on login (see `commands::login` → `spawn_node_runtime`).
pub fn run_tauri() {
    run_tauri_configured(None, None, |builder| builder);
}

/// Run the same desktop application with an optional explicit storage root and
/// builder extension. The native evaluation example supplies its dev-only
/// driver here; the shipping entry point supplies neither.
pub fn run_tauri_configured(
    data_root: Option<std::path::PathBuf>,
    discovery_port: Option<u16>,
    extend: impl FnOnce(tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry>,
) {
    if let Some(port) = discovery_port {
        assert_ne!(port, 0, "configured discovery port must be nonzero");
        RUNTIME_DISCOVERY_PORT
            .set(port)
            .expect("runtime configured only once");
    }
    if let Some(root) = data_root {
        assert!(root.is_absolute(), "configured data root must be absolute");
        RUNTIME_DATA_ROOT
            .set(root)
            .expect("runtime configured only once");
    }
    let _timer = perf_monitor!("application_startup");
    log::info!("Starting Mesh-Talk desktop runtime");

    // Data directory + file manager (~/.mesh-talk), shared by the auth keystore.
    lazy_static::lazy_static! {
        static ref FILE_MANAGER: Arc<mesh_talk_core::storage::file_manager::FileManager> = {
            let data_path = data_dir();
            Arc::new(mesh_talk_core::storage::file_manager::FileManager::new(data_path))
        };
    }
    let _file_manager = FILE_MANAGER.clone();

    // Resolve an interrupted restore before auth can expose either new profile.
    crate::recovery::recover_interrupted_publication(&data_dir())
        .expect("recovery publication marker must be resolved before auth starts");

    // Auth (login/register) is the only stateful service the shell needs; the
    // node manages its own per-account stores out of `NodeState`.
    crate::services::auth_service::AuthService::init_global(FILE_MANAGER.as_ref().clone());
    let app_state = AppState::new(crate::services::auth_service::AuthService::global().clone());

    log::info!("No sockets are bound until a user signs in.");

    let settings_state = SettingsState::default();
    let trust_state = crate::trust::TrustState::default();
    let favorites_state = crate::favorites::FavoritesState::default();
    let avatars_state = crate::avatars::AvatarsState::default();

    let mut context = tauri::generate_context!();
    #[cfg(target_os = "windows")]
    if context.default_window_icon().is_none() {
        // Tauri 2.12 reads the icon from the executable's resources on Windows.
        // The native evaluation example does not embed those resources.
        context.set_default_window_icon(Some(
            tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))
                .expect("bundled app icon is a valid PNG"),
        ));
    }
    if let Some(root) = configured_data_root() {
        let namespace = root
            .parent()
            .and_then(|p| p.file_name())
            .expect("fixture namespace")
            .to_string_lossy();
        context.config_mut().identifier = format!("com.mesh-talk.native-eval.{namespace}");
    }

    let builder = tauri::Builder::default()
        // Single-instance MUST be the first plugin registered (Tauri requirement): its
        // callback runs in the already-running process when a second launch is attempted,
        // so we surface the existing window instead of spawning a duplicate that would
        // fight over the keystore + discovery port.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main").map(|w| w.as_ref().window()) {
                let _ = window.unminimize();
                let _ = crate::window_visibility::show(&window);
                let _ = window.set_focus();
            }
        }));
    // Extensions come after single-instance, which must remain the first plugin.
    extend(builder)
        // Restore window size/position across launches (clamps off-screen geometry).
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        // Launch-at-login. `--hidden` is passed on autostart so a login-time launch
        // comes up straight to the tray (see the WindowEvent handler / frontend boot).
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .setup(move |app| {
            #[cfg(target_os = "linux")]
            app.manage(crate::window_visibility::HiddenPositions::default());
            if let Err(e) = crate::logger::init_logging(app.handle()) {
                log::error!("Failed to initialize logging: {e}");
            }
            if let Ok(cache) = app.path().app_cache_dir() {
                crate::chat_commands::clean_stale_pasted_files(&cache);
            }
            #[cfg(target_os = "linux")]
            crate::diagnostics::log_linux_graphics_configuration();
            crate::config_store::quarantine_legacy(&app.handle().clone());
            // Seed the managed settings from disk before any window/notification logic runs.
            crate::settings::load_into_state(&app.handle().clone(), &app.state::<SettingsState>());
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        handle.state::<crate::app_lock::AppLock>().tick(&handle);
                    }
                });
            }
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
                        crate::settings::prune_running_settings(
                            &app_handle.state::<crate::state::AppState>(),
                            &app_handle.state::<crate::chat_commands::NodeState>(),
                            &app_handle.state::<SettingsState>(),
                        )
                        .await;
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    }
                });
            }

            // Frameless chrome off macOS: macOS uses the native overlay title bar (traffic
            // lights float over our content via `titleBarStyle: Overlay`), but Windows/Linux
            // would otherwise show the traditional title bar. Drop their native decorations
            // so the app provides its own (custom window controls live in the frontend).
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(not(target_os = "macos"))]
                let _ = window.set_decorations(false);
                // The window-state plugin restores saved pixel sizes without
                // enforcing the configured minimum. A damaged state file can
                // otherwise leave only a title-bar-height window visible.
                if let Some(config) = app.config().app.windows.iter().find(|w| w.label == "main") {
                    let scale = window.scale_factor()?;
                    let restored = window.inner_size()?.to_logical::<f64>(scale);
                    let minimum = tauri::LogicalSize::new(
                        config.min_width.unwrap_or(0.0),
                        config.min_height.unwrap_or(0.0),
                    );
                    let preferred = tauri::LogicalSize::new(config.width, config.height);
                    if let Some(repaired) =
                        crate::window_geometry::repaired_startup_size(restored, minimum, preferred)
                    {
                        log::warn!(
                            "Repairing restored main window size from {}x{} to {}x{}",
                            restored.width,
                            restored.height,
                            repaired.width,
                            repaired.height
                        );
                        window.set_size(repaired)?;
                    }
                }
            }

            // The window is created hidden in `tauri.conf.json`, then explicitly shown only
            // for normal launches. This avoids a visible flash on Windows autostart where a
            // visible window would otherwise be created first and hidden later in setup.
            if !std::env::args().any(|a| a == "--hidden")
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.show();
                let _ = window.set_focus();
            }

            #[cfg(debug_assertions)]
            if configured_data_root().is_none() {
                // Native evaluation uses a dedicated data root. Opening then
                // closing debug tools can leave WKWebView's split-pane size
                // behind, making viewport and window geometry disagree.
                let window = app.get_webview_window("main").unwrap();
                window.open_devtools();
                window.close_devtools();
            }
            Ok(())
        })
        // Close-to-tray: if "minimize to tray" is on, hide instead of exiting so the
        // node runtime stays alive and keeps receiving. Quit (tray menu) is the real exit.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                let hide = window
                    .app_handle()
                    .try_state::<SettingsState>()
                    .map(|s| s.get().minimize_to_tray)
                    .unwrap_or(true);
                if hide {
                    api.prevent_close();
                    let _ = crate::window_visibility::hide(window);
                }
            }
        })
        .manage(app_state.app_lock().clone())
        .manage(app_state)
        .manage(settings_state)
        .manage(crate::events::NotificationContextState::default())
        .manage(trust_state)
        .manage(favorites_state)
        .manage(avatars_state)
        .manage(crate::contact_policy::HiddenContactsState::default())
        .manage(crate::chat_commands::NodeState::empty())
        .invoke_handler({
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
                owner_commands::owner_node_identity,
                owner_commands::owner_enqueue_text,
                owner_commands::owner_enqueue_channel_text,
                owner_commands::owner_enqueue_channel_sticker,
                owner_commands::owner_enqueue_channel_file,
                owner_commands::owner_enqueue_sticker,
                owner_commands::owner_enqueue_file,
                owner_commands::owner_account_history,
                owner_commands::owner_delivery_statuses,
                commands::login,
                crate::contact_policy::get_hidden_contacts,
                crate::contact_policy::set_contact_hidden,
                crate::contact_policy::set_contact_hidden_batch,
                crate::contact_policy::delete_contact,
                crate::privacy_commands::get_privacy,
                crate::privacy_commands::set_invisible,
                crate::privacy_commands::set_privacy_allowed,
                crate::privacy_commands::initiate_privacy_contact,
                commands::logout,
                commands::register,
                commands::rename_account,
                commands::auto_login,
                commands::retry_saved_session,
                crate::app_lock::lock_app,
                crate::app_lock::lock_status,
                crate::app_lock::unlock_app,
                crate::app_lock::note_activity,
                crate::app_lock::set_call_active,
                commands::clear_saved_session,
                commands::adopt_linked_account,
                recovery::create_backup,
                recovery::prepare_backup_key,
                recovery::backup_status,
                recovery::inspect_backup,
                recovery::restore_backup,
                crate::chat_commands::my_id,
                crate::chat_commands::list_peers,
                crate::chat_commands::send_dm,
                crate::chat_commands::send_call_signal,
                crate::chat_commands::history,
                crate::chat_commands::account_id,
                crate::chat_commands::publish_avatar,
                crate::chat_commands::peer_avatars,
                crate::chat_commands::send_to_account,
                crate::chat_commands::account_history,
                crate::chat_commands::start_linking,
                crate::chat_commands::stop_linking,
                crate::chat_commands::link_device,
                crate::chat_commands::get_device_sync_policy,
                crate::chat_commands::get_device_sync_status,
                crate::chat_commands::set_device_sync_category,
                crate::chat_commands::get_account_preferences,
                crate::chat_commands::set_account_preference,
                crate::chat_commands::rekey_account,
                crate::chat_commands::list_accounts,
                crate::chat_commands::send_file_to_account,
                crate::chat_commands::react_account,
                crate::chat_commands::account_reactions,
                crate::chat_commands::list_channels,
                crate::chat_commands::create_channel,
                crate::chat_commands::add_channel_member,
                crate::chat_commands::remove_channel_member,
                crate::chat_commands::transfer_channel_owner,
                crate::chat_commands::leave_channel,
                crate::chat_commands::accept_channel_invite,
                crate::chat_commands::rename_channel,
                crate::chat_commands::dissolve_channel,
                crate::chat_commands::channel_members,
                crate::chat_commands::set_channel_replica_enabled,
                crate::chat_commands::channel_custody_statuses,
                crate::chat_commands::channel_activity,
                crate::chat_commands::send_channel_message,
                crate::chat_commands::channel_history,
                crate::chat_commands::send_file_dm,
                crate::chat_commands::send_file_channel,
                crate::chat_commands::save_file,
                crate::chat_commands::file_statuses,
                crate::chat_commands::media_migration_warning,
                crate::chat_commands::attachment_receive_policy,
                crate::chat_commands::set_attachment_global_auto,
                crate::chat_commands::set_attachment_contact_auto,
                crate::chat_commands::decide_attachment,
                crate::chat_commands::save_file_to_dir,
                crate::chat_commands::default_download_dir,
                crate::chat_commands::open_download_dir,
                crate::chat_commands::safety_number,
                crate::chat_commands::read_file,
                crate::chat_commands::read_media,
                crate::chat_commands::react_dm,
                crate::chat_commands::react_channel,
                crate::chat_commands::reactions,
                crate::chat_commands::channel_reactions,
                crate::chat_commands::search,
                crate::owner_commands::owner_search_recent,
                crate::owner_commands::owner_search_context,
                crate::message_favorites::owner_list_message_favorites,
                crate::message_favorites::owner_save_message_favorite,
                crate::message_favorites::owner_edit_message_favorite,
                crate::message_favorites::owner_remove_message_favorite,
                crate::message_favorites::owner_inspect_message_favorites,
                crate::message_favorites::owner_favorite_file_available,
                crate::message_favorites::owner_read_favorite_file,
                crate::message_favorites::owner_save_favorite_file,
                crate::chat_commands::delete_message,
                crate::chat_commands::recall_message,
                crate::chat_commands::clear_conversation,
                crate::chat_commands::send_sticker,
                crate::chat_commands::diag_get_peers,
                crate::chat_commands::diag_network_info,
                crate::chat_commands::get_presence,
                crate::chat_commands::rescan_peers,
                crate::chat_commands::probe_peer_endpoint,
                crate::chat_commands::write_temp_file,
                crate::chat_commands::delete_temp_file,
                crate::chat_commands::capture_screen,
                crate::chat_commands::screenshot_available,
                crate::chat_commands::set_badge,
                crate::chat_commands::network_name,
                crate::events::set_notification_context,
                crate::notifications::notification_permission,
                crate::notifications::request_notification_permission,
                crate::notifications::open_notification_settings,
                crate::logger::get_logs_dir,
                crate::logger::get_log_file,
                crate::logger::read_log_tail,
                crate::logger::save_log_tail,
                crate::diagnostics::env_info,
                crate::updates::check_for_update,
                crate::updates::download_update,
                crate::updates::cancel_update_download,
                crate::integrations::local_access::get_local_agent_access,
                crate::integrations::local_access::set_local_agent_access,
                crate::settings::get_app_settings,
                crate::settings::set_app_settings,
                crate::trust::get_trust,
                crate::trust::mark_verified,
                crate::trust_cards::get_trust_card,
                crate::trust_cards::publish_trust_card,
                crate::trust_cards::import_trust_artifact,
                crate::trust_cards::create_trust_introduction,
                crate::trust_cards::witness_trust_creation,
                crate::favorites::get_favorites,
                crate::favorites::set_favorite,
                crate::favorites::set_favorite_records,
                crate::favorites::set_alias,
                crate::favorites::set_channel_hidden,
                crate::avatars::get_avatars,
                crate::avatars::set_avatar
            ];
            move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
                if invoke
                    .message
                    .webview_ref()
                    .state::<crate::app_lock::AppLock>()
                    .is_locked()
                    && !crate::app_lock::allowed_while_locked(invoke.message.command())
                {
                    invoke
                        .resolver
                        .reject(commands::CommandError::Authorization("app locked".into()));
                    true
                } else {
                    handler(invoke)
                }
            }
        })
        .run(context)
        .unwrap_or_else(|e| {
            // A failed launch must be a non-zero exit, not a silent success.
            log::error!("Error while running tauri application: {e}");
            std::process::exit(1);
        });
}
