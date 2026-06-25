//! Background-presence settings: the two non-autostart toggles that the Rust
//! close-handler and notification path read, plus their persistence.
//!
//! `launch_at_login` is intentionally NOT stored here — it is owned by
//! `tauri-plugin-autostart` (the source of truth is the OS launch-agent), so the
//! frontend reads/writes it through the plugin's own enable/disable/is-enabled.

use crate::commands::CommandError;
use crate::config_store;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// The persisted, user-facing toggles (both default on: a messenger should keep
/// running in the background and tell you when something arrives).
///
/// This is a genuine self-describing JSON-at-rest format (`settings.json`, read via
/// `serde_json::from_str`), so per-field `#[serde(default)]` IS the right forward-compat
/// tool: a `settings.json` written by an OLDER build that lacks a field added LATER
/// still loads (the missing field falls back to its default) instead of failing the
/// whole parse and silently resetting every toggle. New fields MUST carry a
/// `#[serde(default = ...)]` (or a `Default`-backed `#[serde(default)]`) for this.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppSettings {
    /// Closing the window hides it to the tray instead of quitting.
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    /// Show a native notification on incoming messages when the window isn't focused.
    #[serde(default = "default_true")]
    pub notifications: bool,
    /// Remembered default folder received files save into without re-prompting (a per-file
    /// "Save as…" override still exists). Empty = always prompt. New field, so it carries a
    /// `#[serde(default)]` for forward-compat with older `settings.json` (see struct docs).
    #[serde(default)]
    pub download_dir: String,
    /// "Stay signed in": on a successful login the password is saved to the OS keychain
    /// (see [`crate::session_store`]) and reused on the next launch to auto-unlock. Default
    /// on, since the user explicitly wants to skip re-entering their password. Carries a
    /// `#[serde(default = ...)]` for forward-compat with older `settings.json`.
    #[serde(default = "default_true")]
    pub stay_signed_in: bool,
    /// The username to auto-login on next launch (the keychain account to read). Only set
    /// while `stay_signed_in` and a successful login has happened; cleared on sign-out.
    #[serde(default)]
    pub last_user: Option<String>,
    /// Chat-history retention: keep messages for at most this many days; older ones are
    /// erased from this device (a background prune + an immediate prune on change). `0`
    /// means keep forever. New field → `#[serde(default)]` for forward-compat.
    #[serde(default)]
    pub retention_days: u32,
}

/// Default for both toggles (a messenger should run in the background and notify).
fn default_true() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray: true,
            notifications: true,
            download_dir: String::new(),
            stay_signed_in: true,
            last_user: None,
            retention_days: 0,
        }
    }
}

/// The cutoff timestamp (ms since epoch) for a retention window of `days`, or `None` when
/// `days == 0` (keep forever). Messages with `wall_clock < cutoff` are eligible for prune.
pub fn retention_cutoff_ms(days: u32) -> Option<u64> {
    if days == 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some(now.saturating_sub(days as u64 * 24 * 60 * 60 * 1000))
}

/// Managed Tauri state wrapping [`AppSettings`] so the close-handler and the
/// notification path can read it cheaply behind a `Mutex`.
#[derive(Clone, Default)]
pub struct SettingsState(Arc<Mutex<AppSettings>>);

impl SettingsState {
    pub fn get(&self) -> AppSettings {
        self.0.lock().unwrap().clone()
    }
    pub fn set(&self, value: AppSettings) {
        *self.0.lock().unwrap() = value;
    }
}

/// The persistence file for the two toggles, under the app config dir.
const SETTINGS_FILE: &str = "settings.json";

/// Load persisted settings (defaults if the file is missing or unreadable) and
/// seed the managed state. Called once during setup.
pub fn load_into_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &SettingsState) {
    state.set(config_store::load::<R, AppSettings>(app, SETTINGS_FILE));
}

/// Persist settings to disk (best-effort; logged on failure).
fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, value: &AppSettings) {
    config_store::save(app, SETTINGS_FILE, "settings", value);
}

/// Persist `last_user` (the keychain account to auto-login next launch) into the
/// managed state and to disk, preserving every other setting. Called by the login
/// path so a successful sign-in records who to auto-unlock next time. Best-effort.
pub fn record_last_user<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &SettingsState,
    last_user: Option<String>,
) {
    let mut settings = state.get();
    settings.last_user = last_user;
    save(app, &settings);
    state.set(settings);
}

/// Read the current two non-autostart toggles. (Launch-at-login is queried via
/// the autostart plugin's `is_enabled` on the frontend.)
#[tauri::command]
pub fn get_app_settings(state: tauri::State<'_, SettingsState>) -> AppSettings {
    state.get()
}

/// Update the toggles and persist them. If "stay signed in" was just turned OFF,
/// immediately forget the saved keychain secret + `last_user` so the next launch
/// shows the login screen (the toggle is the user's control over the stored secret).
#[tauri::command]
pub async fn set_app_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, SettingsState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    mut settings: AppSettings,
) -> Result<(), CommandError> {
    let prev = state.get();
    if prev.stay_signed_in && !settings.stay_signed_in {
        if let Some(user) = prev.last_user.as_deref() {
            crate::session_store::clear(user);
        }
        // The frontend doesn't manage `last_user`; preserve our own bookkeeping and
        // clear it here so a stale username can't drive a future auto-login.
        settings.last_user = None;
    } else {
        // Likewise keep `last_user` under backend control, not the frontend payload.
        settings.last_user = prev.last_user;
    }
    let retention_days = settings.retention_days;
    save(&app, &settings);
    state.set(settings);

    // Apply a tightened retention window immediately (the periodic prune would otherwise
    // only catch it on its next tick). Best-effort; a no-op if the node isn't running yet.
    if let Some(cutoff) = retention_cutoff_ms(retention_days) {
        let node = {
            let guard = node_state.0.lock().await;
            guard.as_ref().map(|rt| rt.handle())
        };
        if let Some(node) = node {
            let _ = tokio::task::spawn_blocking(move || node.prune_older_than(cutoff)).await;
        }
    }
    Ok(())
}
