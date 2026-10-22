//! Background-presence settings: the two non-autostart toggles that the Rust
//! close-handler and notification path read, plus their persistence.
//!
//! `launch_at_login` is intentionally NOT stored here — it is owned by
//! `tauri-plugin-autostart` (the source of truth is the OS launch-agent), so the
//! frontend reads/writes it through the plugin's own enable/disable/is-enabled.

use crate::config_store;
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyLevel {
    #[default]
    Balanced,
    Private,
    Strict,
    Custom,
}

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
    /// Voice/video calls (experimental, 1:1, LAN-only). Default OFF — the feature is not yet
    /// reliable, so it is opt-in. `#[serde(default)]` makes a bool default to `false`, which
    /// is exactly the off-by-default we want, and keeps older `settings.json` forward-compat.
    #[serde(default)]
    pub calls_enabled: bool,
    /// Whether this account uses discovered post-office relays. Defaults on for existing users.
    #[serde(default = "default_true")]
    pub post_office_enabled: bool,
    #[serde(default = "default_true")]
    pub post_office_send_enabled: bool,
    #[serde(default = "default_true")]
    pub post_office_receive_enabled: bool,
    #[serde(default)]
    pub physical_lan_only: bool,
    /// Share public presence with an opted-in post-office bridge. Defaults off.
    #[serde(default)]
    pub bridge_discovery_enabled: bool,
    /// Preferred relay fingerprint. Other verified relays remain available for failover.
    #[serde(default)]
    pub bridge_hub_id: String,
    /// The incoming-call ringtone id (see frontend `lib/ringtones.ts`). A cosmetic UI
    /// preference; the backend only stores it. Defaults to the classic phone ring.
    #[serde(default = "default_ringtone")]
    pub ringtone: String,
    #[serde(default)]
    pub privacy_level: PrivacyLevel,
    #[serde(default = "default_true")]
    pub notification_preview: bool,
    #[serde(default)]
    pub auto_lock_seconds: u32,
    #[serde(default)]
    pub lock_when_hidden: bool,
    #[serde(default)]
    pub confirm_sensitive_copy: bool,
    #[serde(default)]
    pub clipboard_clear_seconds: u32,
}

/// The default ringtone id (the classic phone ring).
fn default_ringtone() -> String {
    "classic".to_string()
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
            calls_enabled: false,
            post_office_enabled: true,
            post_office_send_enabled: true,
            post_office_receive_enabled: true,
            physical_lan_only: false,
            bridge_discovery_enabled: false,
            bridge_hub_id: String::new(),
            ringtone: default_ringtone(),
            privacy_level: PrivacyLevel::Balanced,
            notification_preview: true,
            auto_lock_seconds: 0,
            lock_when_hidden: false,
            confirm_sensitive_copy: false,
            clipboard_clear_seconds: 0,
        }
    }
}

/// Settings shared by the desktop installation, including auto-login bookkeeping.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct GlobalSettings {
    minimize_to_tray: bool,
    stay_signed_in: bool,
    last_user: Option<String>,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray: true,
            stay_signed_in: true,
            last_user: None,
        }
    }
}

impl From<&AppSettings> for GlobalSettings {
    fn from(value: &AppSettings) -> Self {
        Self {
            minimize_to_tray: value.minimize_to_tray,
            stay_signed_in: value.stay_signed_in,
            last_user: value.last_user.clone(),
        }
    }
}

/// Settings that can reveal or affect the signed-in account's activity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct AccountSettings {
    notifications: bool,
    download_dir: String,
    retention_days: u32,
    calls_enabled: bool,
    post_office_enabled: bool,
    post_office_send_enabled: bool,
    post_office_receive_enabled: bool,
    physical_lan_only: bool,
    bridge_discovery_enabled: bool,
    bridge_hub_id: String,
    ringtone: String,
    privacy_level: PrivacyLevel,
    notification_preview: bool,
    auto_lock_seconds: u32,
    lock_when_hidden: bool,
    confirm_sensitive_copy: bool,
    clipboard_clear_seconds: u32,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self::from(&AppSettings::default())
    }
}

impl From<&AppSettings> for AccountSettings {
    fn from(value: &AppSettings) -> Self {
        Self {
            notifications: value.notifications,
            download_dir: value.download_dir.clone(),
            retention_days: value.retention_days,
            calls_enabled: value.calls_enabled,
            post_office_enabled: value.post_office_enabled,
            post_office_send_enabled: value.post_office_send_enabled,
            post_office_receive_enabled: value.post_office_receive_enabled,
            physical_lan_only: value.physical_lan_only,
            bridge_discovery_enabled: value.bridge_discovery_enabled,
            bridge_hub_id: value.bridge_hub_id.clone(),
            ringtone: value.ringtone.clone(),
            privacy_level: value.privacy_level,
            notification_preview: value.notification_preview,
            auto_lock_seconds: value.auto_lock_seconds,
            lock_when_hidden: value.lock_when_hidden,
            confirm_sensitive_copy: value.confirm_sensitive_copy,
            clipboard_clear_seconds: value.clipboard_clear_seconds,
        }
    }
}

impl AccountSettings {
    fn with_global(&self, global: &GlobalSettings) -> AppSettings {
        AppSettings {
            minimize_to_tray: global.minimize_to_tray,
            notifications: self.notifications,
            download_dir: self.download_dir.clone(),
            stay_signed_in: global.stay_signed_in,
            last_user: global.last_user.clone(),
            retention_days: self.retention_days,
            calls_enabled: self.calls_enabled,
            post_office_enabled: self.post_office_enabled,
            post_office_send_enabled: self.post_office_send_enabled,
            post_office_receive_enabled: self.post_office_receive_enabled,
            physical_lan_only: self.physical_lan_only,
            bridge_discovery_enabled: self.bridge_discovery_enabled,
            bridge_hub_id: self.bridge_hub_id.clone(),
            ringtone: self.ringtone.clone(),
            privacy_level: self.privacy_level,
            notification_preview: self.notification_preview,
            auto_lock_seconds: self.auto_lock_seconds,
            lock_when_hidden: self.lock_when_hidden,
            confirm_sensitive_copy: self.confirm_sensitive_copy,
            clipboard_clear_seconds: self.clipboard_clear_seconds,
        }
    }
}

impl AppSettings {
    pub fn with_privacy_level(mut self, level: PrivacyLevel) -> Self {
        self.privacy_level = level;
        match level {
            PrivacyLevel::Balanced => {
                self.notification_preview = true;
                self.auto_lock_seconds = 0;
                self.lock_when_hidden = false;
                self.confirm_sensitive_copy = false;
                self.clipboard_clear_seconds = 0;
            }
            PrivacyLevel::Private => {
                self.notification_preview = false;
                self.auto_lock_seconds = 600;
                self.lock_when_hidden = false;
                self.confirm_sensitive_copy = false;
                self.clipboard_clear_seconds = 60;
            }
            PrivacyLevel::Strict => {
                self.notification_preview = false;
                self.auto_lock_seconds = 60;
                self.lock_when_hidden = true;
                self.confirm_sensitive_copy = true;
                self.clipboard_clear_seconds = 30;
            }
            PrivacyLevel::Custom => {}
        }
        self
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

#[derive(Default)]
struct SettingsRuntime {
    revision: u64,
    global: GlobalSettings,
    owner: Option<String>,
    current: AppSettings,
}

/// The active account's effective settings and the installation-wide controls.
#[derive(Clone, Default)]
pub struct SettingsState(
    Arc<Mutex<SettingsRuntime>>,
    Arc<tokio::sync::Mutex<()>>,
    #[cfg(test)] Option<std::path::PathBuf>,
);

impl SettingsState {
    #[cfg(test)]
    pub(crate) fn isolated(path: std::path::PathBuf) -> Self {
        Self(
            Arc::new(Mutex::new(SettingsRuntime::default())),
            Arc::default(),
            Some(path),
        )
    }
    fn snapshot(&self, owner: &str) -> Result<(u64, AppSettings), CommandError> {
        let inner = self.0.lock().unwrap();
        if inner.owner.as_deref() != Some(owner) {
            return Err(CommandError::Authorization(
                "settings owner mismatch".into(),
            ));
        }
        Ok((inner.revision, inner.current.clone()))
    }

    /// Hold the current revision through the synchronous destructive commit.
    fn commit_current<T>(
        &self,
        owner: &str,
        revision: u64,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let inner = self.0.lock().unwrap();
        if inner.owner.as_deref() != Some(owner) || inner.revision != revision {
            return Err(CommandError::Authorization("settings superseded".into()));
        }
        operation()
    }

    pub fn get(&self) -> AppSettings {
        self.0.lock().unwrap().current.clone()
    }
    #[cfg(test)]
    pub(crate) fn active_retention(&self) -> Option<(String, u32)> {
        let inner = self.0.lock().unwrap();
        inner
            .owner
            .as_ref()
            .map(|owner| (owner.clone(), inner.current.retention_days))
    }
    pub fn set(&self, value: AppSettings) {
        let mut inner = self.0.lock().unwrap();
        inner.revision += 1;
        inner.global = GlobalSettings::from(&value);
        inner.owner = None;
        inner.current = value;
    }

    pub(crate) fn activate(&self, owner: &str) {
        let path = config_store::account_path(&crate::data_dir(), owner, SETTINGS_FILE)
            .expect("authenticated owner has a valid profile path");
        self.activate_at(owner, &path);
    }

    fn activate_at(&self, owner: &str, path: &std::path::Path) {
        let account: AccountSettings = config_store::read_account(path);
        let mut inner = self.0.lock().unwrap();
        inner.revision += 1;
        inner.current = account.with_global(&inner.global);
        inner.owner = Some(owner.to_owned());
    }

    pub(crate) fn deactivate(&self) {
        let mut inner = self.0.lock().unwrap();
        inner.revision += 1;
        inner.owner = None;
        inner.current = AccountSettings::default().with_global(&inner.global);
    }

    pub(crate) fn get_for_owner(&self, owner: &str) -> Result<AppSettings, CommandError> {
        let inner = self.0.lock().unwrap();
        if inner.owner.as_deref() != Some(owner) {
            return Err(CommandError::Authorization(
                "settings owner mismatch".into(),
            ));
        }
        Ok(inner.current.clone())
    }

    fn update_for_owner(
        &self,
        owner: &str,
        settings: AppSettings,
    ) -> Result<GlobalSettings, CommandError> {
        let mut inner = self.0.lock().unwrap();
        if inner.owner.as_deref() != Some(owner) {
            return Err(CommandError::Authorization(
                "settings owner mismatch".into(),
            ));
        }
        inner.revision += 1;
        inner.global.minimize_to_tray = settings.minimize_to_tray;
        inner.global.stay_signed_in = settings.stay_signed_in;
        inner.global.last_user = settings.last_user.clone();
        inner.current = AccountSettings::from(&settings).with_global(&inner.global);
        Ok(inner.global.clone())
    }

    fn update_without_owner(&self, settings: AppSettings) -> Result<GlobalSettings, CommandError> {
        let mut inner = self.0.lock().unwrap();
        if inner.owner.is_some() {
            return Err(CommandError::Authorization(
                "settings owner required".into(),
            ));
        }
        inner.revision += 1;
        inner.global.minimize_to_tray = settings.minimize_to_tray;
        inner.global.stay_signed_in = settings.stay_signed_in;
        inner.global.last_user = settings.last_user;
        inner.current = AccountSettings::default().with_global(&inner.global);
        Ok(inner.global.clone())
    }
}

/// The persistence file for the two toggles, under the app config dir.
const SETTINGS_FILE: &str = "settings.json";

/// Load persisted settings (defaults if the file is missing or unreadable) and
/// seed the managed state. Called once during setup.
pub fn load_into_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &SettingsState) {
    let global = config_store::load::<R, GlobalSettings>(app, SETTINGS_FILE);
    let mut inner = state.0.lock().unwrap();
    inner.current = AccountSettings::default().with_global(&global);
    inner.global = global.clone();
    // Preserve unowned preferences before removing them from the active global file.
    match config_store::archive_legacy_settings(app) {
        Ok(()) => save(app, &global),
        Err(error) => log::warn!("Could not archive legacy settings: {error}"),
    }
}

/// Persist settings to disk (best-effort; logged on failure).
fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, value: &GlobalSettings) {
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
    let mut inner = state.0.lock().unwrap();
    inner.global.last_user = last_user.clone();
    inner.current.last_user = last_user;
    #[cfg(test)]
    if let Some(path) = state.2.as_ref() {
        std::fs::write(path, serde_json::to_vec(&inner.global).unwrap()).unwrap();
        return;
    }
    save(app, &inner.global);
}

/// Read the current two non-autostart toggles. (Launch-at-login is queried via
/// the autostart plugin's `is_enabled` on the frontend.)
#[tauri::command]
pub fn get_app_settings(
    app: tauri::State<'_, crate::state::AppState>,
    state: tauri::State<'_, SettingsState>,
    owner: Option<String>,
) -> Result<AppSettings, CommandError> {
    match owner {
        Some(owner) => config_store::with_account(&app, &owner, SETTINGS_FILE, |_| {
            state.get_for_owner(&owner).map(redact_login_owner)
        }),
        None => {
            if app.session().capture().is_ok() {
                return Err(CommandError::Authorization(
                    "settings owner required".into(),
                ));
            }
            Ok(redact_login_owner(
                AccountSettings::default().with_global(&state.0.lock().unwrap().global),
            ))
        }
    }
}

fn redact_login_owner(mut settings: AppSettings) -> AppSettings {
    settings.last_user = None;
    settings
}

fn preserve_login_bookkeeping(
    mut settings: AppSettings,
    previous: &AppSettings,
) -> (AppSettings, Option<String>) {
    let clear_user = if previous.stay_signed_in && !settings.stay_signed_in {
        previous.last_user.clone()
    } else {
        None
    };
    // The frontend never chooses which saved credential the backend will use.
    settings.last_user = if previous.stay_signed_in && !settings.stay_signed_in {
        None
    } else {
        previous.last_user.clone()
    };
    (settings, clear_user)
}

/// Apply relay policy only while its originating session and persisted revision
/// still authorize each commit, including after the asynchronous privacy gate.
async fn apply_relay_settings(
    runtime: &mesh_talk_core::node::NodeRuntime,
    admission: &RuntimeAdmission,
    state: &SettingsState,
    owner: &str,
    revision: u64,
    settings: &AppSettings,
) -> Result<(), CommandError> {
    let node = runtime.handle();
    node.set_physical_lan_only_if(settings.physical_lan_only, |commit| {
        admission.authorize_io(runtime, &mut || {
            state
                .commit_current(owner, revision, || Ok(commit()))
                .map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, error.to_string())
                })?
        })
    })
    .await
    .map_err(|error| CommandError::Authorization(error.to_string()))?;
    admission.checked(runtime, || {
        state.commit_current(owner, revision, || {
            node.set_post_office_permissions(
                settings.post_office_send_enabled,
                settings.post_office_receive_enabled,
            );
            node.set_post_office_enabled(settings.post_office_enabled);
            node.set_bridge_hub_id(
                (!settings.bridge_hub_id.is_empty()).then_some(settings.bridge_hub_id.as_str()),
            )
            .map_err(|error| CommandError::InvalidInput(error.into()))?;
            node.set_bridge_discovery_enabled(settings.bridge_discovery_enabled);
            Ok(())
        })
    })?;
    Ok(())
}

/// Startup shares the same application serialization and commit authority as saves.
pub(crate) async fn apply_startup_settings(
    admission: RuntimeAdmission,
    state: SettingsState,
) -> Result<(), CommandError> {
    let owner = admission.lease().owner().to_owned();
    let (revision, settings) = state.snapshot(&owner)?;
    let serialization = state.1.clone().lock_owned().await;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let _serialization = serialization;
                apply_relay_settings(runtime, admission, &state, &owner, revision, &settings).await
            })
        })
        .await
}

/// Applications are serialized; each commit checks the latest persisted revision
/// under its originating runtime/session lease. Blocking prune owns that runtime.
async fn apply_running_settings(
    admission: RuntimeAdmission,
    state: SettingsState,
    owner: String,
    revision: u64,
    settings: AppSettings,
) -> Result<(), CommandError> {
    let serialization = state.1.clone().lock_owned().await;
    let prune_state = state.clone();
    let prune_owner = owner.clone();
    let prune_settings = settings.clone();
    // The detached task owns both lifecycle and application serialization if the IPC is cancelled.
    let prune_admission = admission.clone();
    let serialization = admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                apply_relay_settings(runtime, admission, &state, &owner, revision, &settings)
                    .await?;
                Ok(serialization)
            })
        })
        .await?;
    let Some(cutoff) = retention_cutoff_ms(prune_settings.retention_days) else {
        return Ok(());
    };
    prune_admission
        .blocking(move |runtime, _| {
            let _serialization = serialization;
            prune_state.commit_current(&prune_owner, revision, || {
                runtime
                    .prune_older_than(cutoff)
                    .map(|_| ())
                    .map_err(CommandError::from)
            })
        })
        .await
}

pub(crate) async fn prune_running_settings(
    app: &crate::state::AppState,
    node: &crate::chat_commands::NodeState,
    state: &SettingsState,
) {
    let Ok(admission) = RuntimeAdmission::capture_current(app, node) else {
        return;
    };
    let owner = admission.lease().owner().to_owned();
    let Ok((revision, settings)) = state.snapshot(&owner) else {
        return;
    };
    let Some(cutoff) = retention_cutoff_ms(settings.retention_days) else {
        return;
    };
    let state = state.clone();
    let _ = admission
        .blocking(move |runtime, _| {
            state.commit_current(&owner, revision, || {
                runtime
                    .prune_older_than(cutoff)
                    .map(|_| ())
                    .map_err(CommandError::from)
            })
        })
        .await;
}

/// Update the toggles and persist them. If "stay signed in" was just turned OFF,
/// immediately forget the saved keychain secret + `last_user` so the next launch
/// shows the login screen (the toggle is the user's control over the stored secret).
#[tauri::command]
pub async fn set_app_settings(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, crate::state::AppState>,
    state: tauri::State<'_, SettingsState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: Option<String>,
    settings: AppSettings,
) -> Result<(), CommandError> {
    let admission = owner
        .as_deref()
        .map(|owner| RuntimeAdmission::capture(&app_state, &node_state, owner))
        .transpose()?;
    let level = settings.privacy_level;
    let mut settings = settings.with_privacy_level(level);
    let hub = settings.bridge_hub_id.trim();
    if !hub.is_empty() && (hub.len() != 32 || !hub.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(CommandError::InvalidInput(
            "bridge hub ID must be a full 32-character hexadecimal fingerprint".into(),
        ));
    }
    settings.bridge_hub_id = hub.to_ascii_lowercase();
    match owner.as_deref() {
        Some(owner) => config_store::with_account_lease(
            &app_state,
            admission.as_ref().unwrap().lease(),
            SETTINGS_FILE,
            |path| {
                let previous = state.get_for_owner(owner)?;
                let (settings, clear_user) =
                    preserve_login_bookkeeping(settings.clone(), &previous);
                config_store::write_account(path, &AccountSettings::from(&settings))?;
                let global = state.update_for_owner(owner, settings)?;
                save(&app, &global);
                if let Some(user) = clear_user.as_deref() {
                    crate::session_store::clear(user);
                }
                Ok(())
            },
        )?,
        None => {
            if app_state.session().capture().is_ok() {
                return Err(CommandError::Authorization(
                    "settings owner required".into(),
                ));
            }
            let (settings, clear_user) = preserve_login_bookkeeping(settings, &state.get());
            let global = state.update_without_owner(settings)?;
            save(&app, &global);
            if let Some(user) = clear_user.as_deref() {
                crate::session_store::clear(user);
            }
        }
    }

    // Apply the durable account preference to its running node, if still current.
    if let (Some(owner), Some(admission)) = (owner, admission) {
        let (revision, settings) = state.snapshot(&owner)?;
        // Persistence succeeds even when a concurrent save or logout supersedes live application.
        let state = state.inner().clone();
        let application = tokio::spawn(async move {
            apply_running_settings(admission, state, owner, revision, settings).await
        });
        let _ = application.await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delayed_settings_application_rejects_newer_disabled_save_and_same_owner_relogin() {
        for replace_session in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let app = crate::state::AppState::new(crate::services::auth_service::AuthService::new(
                Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                    mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
                )),
            ));
            let user = crate::services::user::User {
                user_id: "settings-owner".into(),
                name: "settings".into(),
                display_name: "Settings".into(),
                address: "local".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            };
            app.session()
                .publish("original".into(), user.clone(), "pw".into());
            let node = crate::chat_commands::NodeState::empty();
            let runtime = mesh_talk_core::node::NodeRuntime::start(
                root.path(),
                "settings-owner",
                "Settings",
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
            let handle = runtime.handle();
            *node.0.write().await = Some(runtime);
            let state = SettingsState::default();
            state.0.lock().unwrap().owner = Some("settings-owner".into());
            state
                .update_for_owner(
                    "settings-owner",
                    AppSettings {
                        retention_days: 1,
                        physical_lan_only: true,
                        ..AppSettings::default()
                    },
                )
                .unwrap();
            let (revision, settings) = state.snapshot("settings-owner").unwrap();
            let admission = RuntimeAdmission::capture_current(&app, &node).unwrap();
            let serialization = state.1.clone().lock_owned().await;
            let pending_state = state.clone();
            let pending = tokio::spawn(async move {
                apply_running_settings(
                    admission,
                    pending_state,
                    "settings-owner".into(),
                    revision,
                    settings,
                )
                .await
            });
            if replace_session {
                app.session()
                    .publish("replacement".into(), user, "pw".into());
            } else {
                state
                    .update_for_owner("settings-owner", AppSettings::default())
                    .unwrap();
            }
            drop(serialization);
            assert!(pending.await.unwrap().is_err());
            assert!(
                !handle.physical_lan_only(),
                "obsolete settings cannot commit runtime policy or reach retention pruning"
            );
            node.0.write().await.take().unwrap().stop().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_relay_policy_rejects_authority_or_revision_change_during_policy_wait() {
        for change in ["settings", "relogin", "logout", "lock", "current"] {
            let root = tempfile::tempdir().unwrap();
            let app = crate::state::AppState::new(crate::services::auth_service::AuthService::new(
                Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                    mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
                )),
            ));
            let user = crate::services::user::User {
                user_id: "settings-owner".into(),
                name: "settings".into(),
                display_name: "Settings".into(),
                address: "local".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            };
            app.session()
                .publish("original".into(), user.clone(), "pw".into());
            let node = crate::chat_commands::NodeState::empty();
            let runtime = mesh_talk_core::node::NodeRuntime::start(
                root.path(),
                "settings-owner",
                "Settings",
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
            let handle = runtime.handle();
            *node.0.write().await = Some(runtime);
            let state = SettingsState::default();
            state.0.lock().unwrap().owner = Some("settings-owner".into());
            state
                .update_for_owner(
                    "settings-owner",
                    AppSettings {
                        physical_lan_only: true,
                        ..AppSettings::default()
                    },
                )
                .unwrap();
            let (revision, settings) = state.snapshot("settings-owner").unwrap();
            let admission = RuntimeAdmission::capture_current(&app, &node).unwrap();
            let (entered, held) = std::sync::mpsc::channel();
            let (release, resumed) = std::sync::mpsc::channel();
            let holder_node = handle.clone();
            let holder = std::thread::spawn(move || {
                tauri::async_runtime::block_on(holder_node.set_invisible_if(false, move || {
                    entered.send(()).unwrap();
                    resumed.recv().unwrap();
                    Err(std::io::Error::other("test holder makes no mutation"))
                }))
            });
            held.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let runtime_guard = node.0.write().await;
            let runtime = runtime_guard.as_ref().unwrap();
            let result = {
                let pending = apply_relay_settings(
                    runtime,
                    &admission,
                    &state,
                    "settings-owner",
                    revision,
                    &settings,
                );
                tokio::pin!(pending);
                tokio::select! {
                    biased;
                    result = &mut pending => panic!("policy gate was not held: {result:?}"),
                    _ = tokio::task::yield_now() => {}
                }
                match change {
                    "settings" => {
                        state
                            .update_for_owner("settings-owner", AppSettings::default())
                            .unwrap();
                    }
                    "relogin" => {
                        app.session()
                            .publish("replacement".into(), user, "pw".into());
                    }
                    "logout" => app.session().clear(),
                    "lock" => {
                        use tauri::Manager;
                        let host = tauri::test::mock_builder()
                            .manage(app.clone())
                            .manage(app.app_lock().clone())
                            .build(tauri::test::mock_context(tauri::test::noop_assets()))
                            .unwrap();
                        crate::app_lock::lock_app(host.state(), host.state()).unwrap();
                    }
                    _ => {}
                }
                release.send(()).unwrap();
                pending.await
            };
            drop(runtime_guard);
            assert!(holder.join().unwrap().is_err());
            assert_eq!(
                result.is_ok(),
                change == "current",
                "startup result for {change}"
            );
            assert_eq!(
                handle.physical_lan_only(),
                change == "current",
                "startup policy for {change}"
            );
            node.0.write().await.take().unwrap().stop().await;
        }
    }

    #[test]
    fn stale_retention_commit_cannot_run_after_newer_save_disables_retention() {
        let state = SettingsState::default();
        state.0.lock().unwrap().owner = Some("alice".into());
        state
            .update_for_owner(
                "alice",
                AppSettings {
                    retention_days: 1,
                    ..AppSettings::default()
                },
            )
            .unwrap();
        let (revision, _) = state.snapshot("alice").unwrap();
        let (release, waiting) = std::sync::mpsc::channel();
        let queued_state = state.clone();
        let pending = std::thread::spawn(move || {
            waiting.recv().unwrap();
            let mut pruned = false;
            let result = queued_state.commit_current("alice", revision, || {
                pruned = true;
                Ok(())
            });
            assert!(result.is_err());
            assert!(!pruned);
        });
        state
            .update_for_owner("alice", AppSettings::default())
            .unwrap();
        release.send(()).unwrap();
        pending.join().unwrap();
    }

    #[test]
    fn account_settings_keep_private_values_separate_from_device_settings() {
        let alice = AccountSettings {
            retention_days: 7,
            notification_preview: false,
            ..AccountSettings::default()
        };
        let bob = AccountSettings::default();
        let global = GlobalSettings {
            minimize_to_tray: false,
            stay_signed_in: true,
            last_user: Some("alice".into()),
        };
        let alice_view = alice.with_global(&global);
        let bob_view = bob.with_global(&global);
        assert_eq!(alice_view.retention_days, 7);
        assert_eq!(bob_view.retention_days, 0);
        assert!(!alice_view.notification_preview);
        assert!(bob_view.notification_preview);
        assert!(!alice_view.minimize_to_tray);
        assert!(!bob_view.minimize_to_tray);
        assert_eq!(alice_view.last_user.as_deref(), Some("alice"));
        assert!(redact_login_owner(alice_view).last_user.is_none());
    }

    #[test]
    fn renderer_cannot_choose_saved_login_owner() {
        let previous = AppSettings {
            last_user: Some("alice".into()),
            ..AppSettings::default()
        };
        let forged = AppSettings {
            last_user: Some("bob".into()),
            ..AppSettings::default()
        };
        let (kept, clear) = preserve_login_bookkeeping(forged.clone(), &previous);
        assert_eq!(kept.last_user.as_deref(), Some("alice"));
        assert!(clear.is_none());
        let (disabled, clear) = preserve_login_bookkeeping(
            AppSettings {
                stay_signed_in: false,
                ..forged
            },
            &previous,
        );
        assert!(disabled.last_user.is_none());
        assert_eq!(clear.as_deref(), Some("alice"));
    }

    #[test]
    fn account_settings_persist_separately_and_legacy_global_fields_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let alice_path = config_store::account_path(root.path(), "alice", SETTINGS_FILE).unwrap();
        let bob_path = config_store::account_path(root.path(), "bob", SETTINGS_FILE).unwrap();
        let alice = AccountSettings {
            download_dir: "/Alice/private".into(),
            retention_days: 5,
            ..AccountSettings::default()
        };
        config_store::write_account(&alice_path, &alice).unwrap();
        let loaded: AccountSettings = config_store::read_account(&alice_path);
        let other: AccountSettings = config_store::read_account(&bob_path);
        assert_eq!(loaded.download_dir, "/Alice/private");
        assert_eq!(loaded.retention_days, 5);
        assert!(other.download_dir.is_empty());
        assert_eq!(other.retention_days, 0);
        let old_global = r#"{"stay_signed_in":false,"last_user":"alice","download_dir":"/leaked","retention_days":30}"#;
        let global: GlobalSettings = serde_json::from_str(old_global).unwrap();
        assert!(!global.stay_signed_in);
        assert_eq!(
            AccountSettings::default()
                .with_global(&global)
                .retention_days,
            0
        );
    }

    #[test]
    fn active_runtime_switches_preferences_and_clears_them_on_sign_out() {
        let root = tempfile::tempdir().unwrap();
        let alice = config_store::account_path(root.path(), "alice", SETTINGS_FILE).unwrap();
        let bob = config_store::account_path(root.path(), "bob", SETTINGS_FILE).unwrap();
        config_store::write_account(
            &alice,
            &AccountSettings {
                retention_days: 7,
                notifications: false,
                ..AccountSettings::default()
            },
        )
        .unwrap();
        config_store::write_account(
            &bob,
            &AccountSettings {
                retention_days: 30,
                notifications: true,
                ..AccountSettings::default()
            },
        )
        .unwrap();
        let state = SettingsState::default();
        state.activate_at("alice", &alice);
        assert_eq!(state.get().retention_days, 7);
        assert_eq!(state.active_retention(), Some(("alice".into(), 7)));
        assert!(!state.get().notifications);
        assert!(state.get_for_owner("bob").is_err());
        state.activate_at("bob", &bob);
        assert_eq!(state.get().retention_days, 30);
        assert!(state.get().notifications);
        state.deactivate();
        assert_eq!(state.get().retention_days, 0);
        assert_eq!(state.active_retention(), None);
        assert!(state.get_for_owner("bob").is_err());
    }

    #[test]
    fn old_settings_json_without_the_new_fields_loads_with_defaults() {
        // A settings.json written by a build BEFORE calls_enabled/ringtone existed must still
        // load — the missing fields fall back to their defaults rather than failing the whole
        // parse (which would silently reset every toggle). This is the forward-compat contract.
        let json = r#"{"minimize_to_tray":true,"notifications":false}"#;
        let s: AppSettings = serde_json::from_str(json).expect("loads with defaults");
        assert!(!s.calls_enabled, "calls default OFF");
        assert_eq!(s.ringtone, "classic", "ringtone defaults to classic");
        assert!(s.minimize_to_tray);
        assert!(!s.notifications);
        assert_eq!(s.retention_days, 0);
        assert!(s.stay_signed_in);
        assert!(
            s.post_office_enabled,
            "existing accounts keep relay delivery enabled"
        );
        assert!(s.post_office_send_enabled);
        assert!(s.post_office_receive_enabled);
        assert!(!s.physical_lan_only);
        assert!(!s.bridge_discovery_enabled, "bridge discovery defaults off");
        assert!(s.bridge_hub_id.is_empty(), "no hub is pinned by default");
    }

    #[test]
    fn post_office_preference_is_account_scoped_and_persistent() {
        let root = tempfile::tempdir().unwrap();
        let alice_path = config_store::account_path(root.path(), "alice", SETTINGS_FILE).unwrap();
        let bob_path = config_store::account_path(root.path(), "bob", SETTINGS_FILE).unwrap();
        let alice = AccountSettings {
            post_office_enabled: false,
            post_office_send_enabled: false,
            post_office_receive_enabled: true,
            physical_lan_only: true,
            bridge_discovery_enabled: true,
            bridge_hub_id: "0123456789abcdef0123456789abcdef".into(),
            ..AccountSettings::default()
        };
        config_store::write_account(&alice_path, &alice).unwrap();
        let saved: AccountSettings = config_store::read_account(&alice_path);
        let other: AccountSettings = config_store::read_account(&bob_path);
        assert!(!saved.post_office_enabled);
        assert!(!saved.post_office_send_enabled);
        assert!(saved.post_office_receive_enabled);
        assert!(saved.physical_lan_only);
        assert!(saved.bridge_discovery_enabled);
        assert_eq!(saved.bridge_hub_id, "0123456789abcdef0123456789abcdef");
        assert!(other.post_office_enabled);
        assert!(!other.bridge_discovery_enabled);
        assert!(other.bridge_hub_id.is_empty());
        assert!(
            !saved
                .with_global(&GlobalSettings::default())
                .post_office_enabled
        );
    }

    #[test]
    fn settings_round_trip_through_json_preserves_calls_and_ringtone() {
        let s = AppSettings {
            calls_enabled: true,
            ringtone: "marimba".to_string(),
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&s).expect("serializes");
        let back: AppSettings = serde_json::from_str(&json).expect("deserializes");
        assert!(back.calls_enabled);
        assert_eq!(back.ringtone, "marimba");
    }

    #[test]
    fn the_default_ringtone_is_classic() {
        assert_eq!(AppSettings::default().ringtone, "classic");
        assert!(!AppSettings::default().calls_enabled);
    }

    #[test]
    fn old_settings_keep_balanced_privacy_behavior() {
        let old = r#"{"minimize_to_tray":true,"notifications":true}"#;
        let settings: AppSettings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.privacy_level, PrivacyLevel::Balanced);
        assert!(settings.notification_preview);
        assert_eq!(settings.auto_lock_seconds, 0);
        assert!(!settings.lock_when_hidden);
        assert!(!settings.confirm_sensitive_copy);
        assert_eq!(settings.clipboard_clear_seconds, 0);
    }

    #[test]
    fn privacy_presets_apply_only_their_visible_controls() {
        let original = AppSettings {
            notifications: false,
            retention_days: 30,
            stay_signed_in: false,
            ..AppSettings::default()
        };
        let private = original.clone().with_privacy_level(PrivacyLevel::Private);
        assert!(!private.notifications);
        assert_eq!(private.retention_days, 30);
        assert!(!private.stay_signed_in);
        assert!(!private.notification_preview);
        assert_eq!(private.auto_lock_seconds, 600);
        assert!(!private.confirm_sensitive_copy);
        assert_eq!(private.clipboard_clear_seconds, 60);

        let strict = original.with_privacy_level(PrivacyLevel::Strict);
        assert_eq!(strict.auto_lock_seconds, 60);
        assert!(strict.lock_when_hidden);
        assert!(strict.confirm_sensitive_copy);
        assert_eq!(strict.clipboard_clear_seconds, 30);
    }
}
