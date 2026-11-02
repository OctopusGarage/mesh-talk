//! Background-presence settings: the two non-autostart toggles that the Rust
//! close-handler and notification path read, plus their persistence.
//!
//! `launch_at_login` is intentionally NOT stored here — it is owned by
//! `tauri-plugin-autostart` (the source of truth is the OS launch-agent), so the
//! frontend reads/writes it through the plugin's own enable/disable/is-enabled.

mod shared;

use crate::config_store;
use crate::screenshot_shortcuts::ScreenshotShortcuts;
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
    /// Permit disclosure of this device's originating office. Independent opt-in.
    #[serde(default)]
    pub origin_sharing_enabled: bool,
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
    /// Capture bindings remain local to this account on this device.
    #[serde(default)]
    pub screenshot_shortcuts: ScreenshotShortcuts,
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
            origin_sharing_enabled: false,
            bridge_hub_id: String::new(),
            ringtone: default_ringtone(),
            privacy_level: PrivacyLevel::Balanced,
            notification_preview: true,
            auto_lock_seconds: 0,
            lock_when_hidden: false,
            confirm_sensitive_copy: false,
            clipboard_clear_seconds: 0,
            screenshot_shortcuts: ScreenshotShortcuts::default(),
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
    origin_sharing_enabled: bool,
    bridge_hub_id: String,
    ringtone: String,
    privacy_level: PrivacyLevel,
    notification_preview: bool,
    auto_lock_seconds: u32,
    lock_when_hidden: bool,
    confirm_sensitive_copy: bool,
    clipboard_clear_seconds: u32,
    screenshot_shortcuts: ScreenshotShortcuts,
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
            origin_sharing_enabled: value.origin_sharing_enabled,
            bridge_hub_id: value.bridge_hub_id.clone(),
            ringtone: value.ringtone.clone(),
            privacy_level: value.privacy_level,
            notification_preview: value.notification_preview,
            auto_lock_seconds: value.auto_lock_seconds,
            lock_when_hidden: value.lock_when_hidden,
            confirm_sensitive_copy: value.confirm_sensitive_copy,
            clipboard_clear_seconds: value.clipboard_clear_seconds,
            screenshot_shortcuts: value.screenshot_shortcuts.clone(),
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
            origin_sharing_enabled: self.origin_sharing_enabled,
            bridge_hub_id: self.bridge_hub_id.clone(),
            ringtone: self.ringtone.clone(),
            privacy_level: self.privacy_level,
            notification_preview: self.notification_preview,
            auto_lock_seconds: self.auto_lock_seconds,
            lock_when_hidden: self.lock_when_hidden,
            confirm_sensitive_copy: self.confirm_sensitive_copy,
            clipboard_clear_seconds: self.clipboard_clear_seconds,
            screenshot_shortcuts: self.screenshot_shortcuts.clone(),
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

struct AppliedSettings {
    revision: u64,
    lease: crate::state::SessionLease,
    account: String,
    node: std::sync::Weak<mesh_talk_core::node::Node>,
}

#[derive(Default)]
struct SettingsRuntime {
    applied: Option<AppliedSettings>,
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
    fn account_path(&self, owner: &str) -> Result<std::path::PathBuf, CommandError> {
        #[cfg(test)]
        let root = self
            .2
            .as_ref()
            .and_then(|path| path.parent())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(crate::data_dir);
        #[cfg(not(test))]
        let root = crate::data_dir();
        config_store::account_path(&root, owner, SETTINGS_FILE)
            .ok_or_else(|| CommandError::Authorization("Invalid settings owner".into()))
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

    fn needs_application(
        &self,
        runtime: &mesh_talk_core::node::NodeRuntime,
        admission: &RuntimeAdmission,
    ) -> bool {
        let inner = self.0.lock().unwrap();
        !inner.applied.as_ref().is_some_and(|applied| {
            applied.revision == inner.revision
                && &applied.lease == admission.lease()
                && applied.account == runtime.account_id()
                && applied
                    .node
                    .upgrade()
                    .is_some_and(|node| Arc::ptr_eq(&node, &runtime.handle()))
        })
    }

    fn mark_applied(
        &self,
        runtime: &mesh_talk_core::node::NodeRuntime,
        admission: &RuntimeAdmission,
        revision: u64,
    ) -> Result<(), CommandError> {
        let mut inner = self.0.lock().unwrap();
        if inner.owner.as_deref() != Some(admission.lease().owner()) || inner.revision != revision {
            return Err(CommandError::Authorization("settings superseded".into()));
        }
        inner.applied = Some(AppliedSettings {
            revision,
            lease: admission.lease().clone(),
            account: runtime.account_id().to_owned(),
            node: Arc::downgrade(&runtime.handle()),
        });
        Ok(())
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
) -> bool {
    let mut inner = state.0.lock().unwrap();
    inner.global.last_user = last_user.clone();
    inner.current.last_user = last_user;
    #[cfg(test)]
    let path = state
        .2
        .clone()
        .or_else(|| config_store::config_path(app, SETTINGS_FILE));
    #[cfg(not(test))]
    let path = config_store::config_path(app, SETTINGS_FILE);
    let Some(path) = path else {
        log::warn!("No config directory: saved-login metadata not persisted");
        return false;
    };
    if let Err(error) = config_store::write_account(&path, &inner.global) {
        log::warn!("Failed to persist saved-login metadata: {error}");
        return false;
    }
    true
}

/// Read the current two non-autostart toggles. (Launch-at-login is queried via
/// the autostart plugin's `is_enabled` on the frontend.)
#[tauri::command]
pub async fn get_app_settings(
    app: tauri::State<'_, crate::state::AppState>,
    state: tauri::State<'_, SettingsState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: Option<String>,
    expected_account: Option<String>,
) -> Result<AppSettings, CommandError> {
    match owner {
        Some(owner) if expected_account.is_none() => {
            config_store::with_account(&app, &owner, SETTINGS_FILE, |_| {
                state.get_for_owner(&owner).map(redact_login_owner)
            })
        }
        Some(owner) => {
            RuntimeAdmission::capture(&app, &node_state, &owner)?
                .with_runtime(|runtime| {
                    require_settings_account(runtime, expected_account.as_deref())?;
                    state.get_for_owner(&owner).map(redact_login_owner)
                })
                .await
        }
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
    node.set_origin_sharing_enabled_if(settings.origin_sharing_enabled, |commit| {
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
    let applied_state = state.clone();
    // The detached task owns both lifecycle and application serialization if the IPC is cancelled.
    let prune_admission = admission.clone();
    let (serialization, applied_node) = admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                apply_relay_settings(runtime, admission, &state, &owner, revision, &settings)
                    .await?;
                Ok((serialization, runtime.handle()))
            })
        })
        .await?;
    let Some(cutoff) = retention_cutoff_ms(prune_settings.retention_days) else {
        let _serialization = serialization;
        return prune_admission
            .with_runtime(|runtime| {
                if !Arc::ptr_eq(&applied_node, &runtime.handle()) {
                    return Err(CommandError::Authorization(
                        "settings runtime replaced".into(),
                    ));
                }
                applied_state.mark_applied(runtime, &prune_admission, revision)
            })
            .await;
    };
    prune_admission
        .blocking(move |runtime, admission| {
            let _serialization = serialization;
            if !Arc::ptr_eq(&applied_node, &runtime.handle()) {
                return Err(CommandError::Authorization(
                    "settings runtime replaced".into(),
                ));
            }
            prune_state.commit_current(&prune_owner, revision, || {
                runtime
                    .prune_older_than(cutoff)
                    .map(|_| ())
                    .map_err(CommandError::from)
            })?;
            applied_state.mark_applied(runtime, admission, revision)
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

/// Project authenticated sibling preferences into the native settings owner. Remote
/// application bypasses the local publisher and retains local paths/credentials.
#[tauri::command]
pub async fn reconcile_account_settings(
    app_state: tauri::State<'_, crate::state::AppState>,
    state: tauri::State<'_, SettingsState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: String,
    expected_account: String,
    seed_missing: bool,
) -> Result<AppSettings, CommandError> {
    let admission = RuntimeAdmission::capture(&app_state, &node_state, &owner)?;
    reconcile_shared_settings(
        &admission,
        &state,
        &owner,
        &expected_account,
        seed_missing,
        &crate::data_dir(),
    )
    .await
}

async fn reconcile_shared_settings(
    admission: &RuntimeAdmission,
    state: &SettingsState,
    owner: &str,
    expected_account: &str,
    seed_missing: bool,
    root: &std::path::Path,
) -> Result<AppSettings, CommandError> {
    let projected = admission
        .with_runtime(|runtime| {
            use mesh_talk_core::node::SyncCategory;
            if runtime.account_id() != expected_account {
                return Err(CommandError::Authorization(
                    "Settings belong to another account".into(),
                ));
            }
            let node = runtime.handle();
            let previous = state.get_for_owner(owner)?;
            if !node.device_sync_policy().appearance {
                return if state.needs_application(runtime, admission) {
                    state.snapshot(owner).map(Some)
                } else {
                    Ok(None)
                };
            }
            let records = node.account_preferences(SyncCategory::Appearance);
            if seed_missing {
                for (key, value) in shared::values(&previous) {
                    if !records.iter().any(|record| record.key == key) {
                        node.set_account_preference_if_absent(
                            SyncCategory::Appearance,
                            key,
                            Some(value),
                        )?;
                    }
                }
            }
            let next = protect_imported_settings(
                &previous,
                shared::project(
                    &previous,
                    &node.account_preferences(SyncCategory::Appearance),
                ),
            );
            if shared::changed(&previous, &next).is_empty() {
                return if state.needs_application(runtime, admission) {
                    state.snapshot(owner).map(Some)
                } else {
                    Ok(None)
                };
            }
            let path = config_store::account_path(root, owner, SETTINGS_FILE)
                .ok_or_else(|| CommandError::Authorization("Invalid settings owner".into()))?;
            config_store::write_account(&path, &AccountSettings::from(&next))?;
            state.update_for_owner(owner, next)?;
            state.snapshot(owner).map(Some)
        })
        .await?;
    if let Some((revision, settings)) = projected {
        apply_running_settings(
            admission.clone(),
            state.clone(),
            owner.to_owned(),
            revision,
            settings,
        )
        .await?;
    }
    admission
        .with_runtime(|_| state.get_for_owner(owner).map(redact_login_owner))
        .await
}

fn require_settings_account(
    runtime: &mesh_talk_core::node::NodeRuntime,
    expected: Option<&str>,
) -> Result<(), CommandError> {
    if expected != Some(runtime.account_id()) {
        return Err(CommandError::Authorization(
            "Settings belong to another account or account is not ready".into(),
        ));
    }
    Ok(())
}

fn normalize_settings(mut settings: AppSettings) -> Result<AppSettings, CommandError> {
    settings.screenshot_shortcuts.validate()?;
    let level = settings.privacy_level;
    settings = settings.with_privacy_level(level);
    let hub = settings.bridge_hub_id.trim();
    if !hub.is_empty() && (hub.len() != 32 || !hub.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(CommandError::InvalidInput(
            "bridge hub ID must be a full 32-character hexadecimal fingerprint".into(),
        ));
    }
    settings.bridge_hub_id = hub.to_ascii_lowercase();
    Ok(settings)
}

/// Linked preference records are not fresh local consent to relax protection.
/// Keep each existing protection while still importing stronger and routine values.
fn protect_imported_settings(previous: &AppSettings, mut next: AppSettings) -> AppSettings {
    let level = next.privacy_level;
    next = next.with_privacy_level(level);
    let longer_or_disabled = |old: u32, new: u32| old > 0 && (new == 0 || new > old);
    let mut preserved = false;
    macro_rules! keep {
        ($field:ident, $weaker:expr) => {
            if $weaker {
                next.$field = previous.$field;
                preserved = true;
            }
        };
    }
    keep!(
        notification_preview,
        !previous.notification_preview && next.notification_preview
    );
    keep!(
        auto_lock_seconds,
        longer_or_disabled(previous.auto_lock_seconds, next.auto_lock_seconds)
    );
    keep!(
        lock_when_hidden,
        previous.lock_when_hidden && !next.lock_when_hidden
    );
    keep!(
        confirm_sensitive_copy,
        previous.confirm_sensitive_copy && !next.confirm_sensitive_copy
    );
    keep!(
        clipboard_clear_seconds,
        longer_or_disabled(
            previous.clipboard_clear_seconds,
            next.clipboard_clear_seconds
        )
    );
    keep!(
        retention_days,
        longer_or_disabled(previous.retention_days, next.retention_days)
    );
    if preserved {
        next.privacy_level = PrivacyLevel::Custom;
    }
    next
}

fn requires_identity_verification(previous: &AppSettings, next: &AppSettings) -> bool {
    let longer_or_disabled = |old: u32, new: u32| old > 0 && (new == 0 || new > old);
    (!previous.stay_signed_in && next.stay_signed_in)
        || (!previous.notification_preview && next.notification_preview)
        || longer_or_disabled(previous.auto_lock_seconds, next.auto_lock_seconds)
        || (previous.lock_when_hidden && !next.lock_when_hidden)
        || (previous.confirm_sensitive_copy && !next.confirm_sensitive_copy)
        || longer_or_disabled(
            previous.clipboard_clear_seconds,
            next.clipboard_clear_seconds,
        )
        || longer_or_disabled(previous.retention_days, next.retention_days)
}

/// Update the toggles and persist them. If "stay signed in" was just turned OFF,
/// immediately forget the saved keychain secret + `last_user` so the next launch
/// shows the login screen (the toggle is the user's control over the stored secret).
#[tauri::command]
pub async fn set_app_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    app_state: tauri::State<'_, crate::state::AppState>,
    state: tauri::State<'_, SettingsState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: Option<String>,
    settings: AppSettings,
    patch: Option<serde_json::Value>,
    expected_account: Option<String>,
    password: Option<String>,
) -> Result<(), CommandError> {
    let entry_generation = app_state.session().generation();
    let mut admission = owner
        .as_deref()
        .map(|owner| RuntimeAdmission::capture(&app_state, &node_state, owner))
        .transpose()?;
    // Serialize the comparison and durable save, but never hold a synchronous
    // settings/session guard while the operating system asks for verification.
    let _save = state.1.lock().await;
    let entry_revision = owner
        .as_deref()
        .map(|owner| state.snapshot(owner).map(|(revision, _)| revision))
        .transpose()?;
    if let Some(owner) = owner.as_deref() {
        let previous = state.get_for_owner(owner)?;
        let next = match patch.as_ref() {
            Some(patch) => shared::patch(&previous, patch)?,
            None => settings.clone(),
        };
        let next = normalize_settings(next)?;
        if requires_identity_verification(&previous, &next) {
            let verified = admission
                .take()
                .unwrap()
                .verify_identity(password, "change Mesh-Talk privacy protections")
                .await?;
            admission = Some(verified);
        }
    }
    // Remembered credentials share authentication ordering with login, logout,
    // and native legacy-item migration. Ordinary cosmetic changes need no such
    // lease. Hold it from the preference commit until owned cleanup completes.
    let previous = match owner.as_deref() {
        Some(owner) => state.get_for_owner(owner)?,
        None => state.get(),
    };
    let next = match patch.as_ref() {
        Some(patch) => shared::patch(&previous, patch)?,
        None => settings.clone(),
    };
    let mut credential_auth = if normalize_settings(next)?.stay_signed_in != previous.stay_signed_in
    {
        Some(app_state.auth_operation.clone().lock_owned().await)
    } else {
        None
    };
    let mut credential_cleanup = None;
    // Installation controls remain usable during startup. They cannot write
    // account preference rows or normalize/change private account settings.
    if let Some(owner) = owner.as_deref()
        && expected_account.is_none()
        && patch.as_ref().is_some_and(shared::installation_patch)
    {
        let clear_user = config_store::with_account_lease(
            &app_state,
            admission.as_ref().unwrap().lease(),
            SETTINGS_FILE,
            |_| {
                admission.as_ref().unwrap().sensitive_commit(|| {
                    if state.snapshot(owner)?.0 != entry_revision.unwrap() {
                        return Err(CommandError::Authorization(
                            "settings superseded during verification".into(),
                        ));
                    }
                    let previous = state.get_for_owner(owner)?;
                    let settings = shared::patch(&previous, patch.as_ref().unwrap())?;
                    let (settings, clear_user) = preserve_login_bookkeeping(settings, &previous);
                    let global = state.update_for_owner(owner, settings)?;
                    save(&app, &global);
                    Ok(clear_user)
                })
            },
        )?;
        if let Some(user) = clear_user {
            credential_cleanup = Some(crate::commands::spawn_saved_credential_cleanup(
                credential_auth
                    .take()
                    .expect("saved-login changes own auth ordering"),
                user,
            ));
        }
        drop(_save);
        if let Some(cleanup) = credential_cleanup {
            cleanup
                .await
                .map_err(|_| CommandError::Service("Saved-login cleanup worker stopped".into()))?;
        }
        return Ok(());
    }
    match owner.as_deref() {
        Some(owner) => {
            use tauri::Manager;
            let shortcuts = app.try_state::<crate::screenshot_shortcuts::ScreenshotShortcutState>();
            // OS accelerator APIs dispatch synchronously to the main thread. Never call
            // them while holding native session or installed-runtime guards.
            let _shortcut_operation = match shortcuts.as_ref() {
                Some(shortcuts) => Some(shortcuts.operation.lock().await),
                None => None,
            };
            let admission = admission.as_ref().unwrap();
            let previous_shortcuts = admission
                .with_runtime(|runtime| {
                    require_settings_account(runtime, expected_account.as_deref())?;
                    state
                        .get_for_owner(owner)
                        .map(|settings| settings.screenshot_shortcuts)
                })
                .await?;
            let next = match patch.as_ref() {
                Some(patch) => shared::patch(&state.get_for_owner(owner)?, patch)?,
                None => settings.clone(),
            };
            let next_shortcuts = normalize_settings(next)?.screenshot_shortcuts;
            let shortcut_changed = previous_shortcuts != next_shortcuts;
            if shortcut_changed && let Some(shortcuts) = shortcuts.as_ref() {
                shortcuts
                    .apply(&app, admission.lease(), &next_shortcuts)
                    .await?;
            }
            let saved = admission
                .with_runtime(|runtime| {
                    require_settings_account(runtime, expected_account.as_deref())?;
                    if state.snapshot(owner)?.0 != entry_revision.unwrap() {
                        return Err(CommandError::Authorization(
                            "settings superseded during verification".into(),
                        ));
                    }
                    let previous = state.get_for_owner(owner)?;
                    let settings = match patch.as_ref() {
                        Some(patch) => shared::patch(&previous, patch)?,
                        None => settings.clone(),
                    };
                    let settings = normalize_settings(settings)?;
                    let (settings, clear_user) = preserve_login_bookkeeping(settings, &previous);
                    // Only the user's edited keys acquire new revisions. Reconcile never
                    // takes this path, so sibling changes cannot echo into fresh records.
                    for (key, value) in shared::changed(&previous, &settings) {
                        runtime.handle().set_account_preference(
                            mesh_talk_core::node::SyncCategory::Appearance,
                            key,
                            Some(value),
                        )?;
                    }
                    let path = state.account_path(owner)?;
                    config_store::write_account(&path, &AccountSettings::from(&settings))?;
                    let global = state.update_for_owner(owner, settings)?;
                    save(&app, &global);
                    Ok(clear_user)
                })
                .await;
            if saved.is_err()
                && shortcut_changed
                && let Some(shortcuts) = shortcuts.as_ref()
                && let Err(error) = shortcuts
                    .apply(&app, admission.lease(), &previous_shortcuts)
                    .await
            {
                log::error!(
                    "Could not restore screenshot shortcuts after failed settings save: {error}"
                );
            }
            if let Some(user) = saved? {
                credential_cleanup = Some(crate::commands::spawn_saved_credential_cleanup(
                    credential_auth
                        .take()
                        .expect("saved-login changes own auth ordering"),
                    user,
                ));
            }
        }
        None => {
            let clear_user = app_state
                .session()
                .without_session(entry_generation, || {
                    let previous = state.get();
                    let settings = match patch.as_ref() {
                        Some(patch) => shared::patch(&previous, patch)?,
                        None => settings,
                    };
                    let (settings, clear_user) =
                        preserve_login_bookkeeping(normalize_settings(settings)?, &previous);
                    let global = state.update_without_owner(settings)?;
                    save(&app, &global);
                    Ok::<_, CommandError>(clear_user)
                })
                .map_err(CommandError::Authorization)??;
            if let Some(user) = clear_user {
                credential_cleanup = Some(crate::commands::spawn_saved_credential_cleanup(
                    credential_auth
                        .take()
                        .expect("saved-login changes own auth ordering"),
                    user,
                ));
            }
        }
    }

    drop(_save);
    drop(credential_auth);
    // Apply the durable account preference to its running node, if still current.
    // Its owned task starts before awaiting native credential cleanup: a blocked
    // Keychain cannot postpone a committed network privacy or retention policy.
    let application = if let (Some(owner), Some(admission)) = (owner, admission) {
        let (revision, settings) = state.snapshot(&owner)?;
        // Persistence succeeds even when a concurrent save or logout supersedes live application.
        let state = state.inner().clone();
        Some(tokio::spawn(async move {
            apply_running_settings(admission, state, owner, revision, settings).await
        }))
    } else {
        None
    };
    if let Some(application) = application {
        let _ = application.await;
    }
    if let Some(cleanup) = credential_cleanup {
        cleanup
            .await
            .map_err(|_| CommandError::Service("Saved-login cleanup worker stopped".into()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_shortcuts_persist_per_device_without_portable_export() {
        let initial = AppSettings::default();
        let mut changed = initial.clone();
        changed.screenshot_shortcuts.global = true;
        changed.screenshot_shortcuts.capture = Some("Alt+Shift+KeyS".into());
        assert!(shared::changed(&initial, &changed).is_empty());
        assert!(!shared::values(&changed).contains_key("settings.screenshot_shortcuts"));
        let account: AccountSettings =
            serde_json::from_str(&serde_json::to_string(&AccountSettings::from(&changed)).unwrap())
                .unwrap();
        assert_eq!(
            account
                .with_global(&GlobalSettings::default())
                .screenshot_shortcuts,
            changed.screenshot_shortcuts
        );
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.json");
        let second = root.path().join("second.json");
        config_store::write_account(&first, &account).unwrap();
        let state = SettingsState::default();
        state.activate_at("first", &first);
        assert_eq!(
            state.get().screenshot_shortcuts,
            changed.screenshot_shortcuts
        );
        state.activate_at("second", &second);
        assert_eq!(
            state.get().screenshot_shortcuts,
            initial.screenshot_shortcuts
        );
        state.activate_at("first", &first);
        assert_eq!(
            state.get().screenshot_shortcuts,
            changed.screenshot_shortcuts
        );
    }

    #[tokio::test]
    async fn denied_saved_login_change_leaves_acknowledged_settings_untouched() {
        use tauri::Manager;
        let root = tempfile::tempdir().unwrap();
        let auth = crate::state::AppState::new(crate::services::auth_service::AuthService::new(
            Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().into()),
            )),
        ));
        let user = crate::services::user::User {
            user_id: "settings-owner".into(),
            name: "Owner".into(),
            account_login_name: None,
            display_name: "Owner".into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        auth.session().publish("session".into(), user, "pw".into());
        let state = SettingsState::isolated(root.path().join("settings.json"));
        state.set(AppSettings {
            stay_signed_in: false,
            ..AppSettings::default()
        });
        state.activate_at("settings-owner", &root.path().join("account.json"));
        auth.app_lock()
            .set_identity_verifier(|| Err(CommandError::Authentication("Invalid password".into())));
        let app = tauri::test::mock_builder()
            .manage(auth)
            .manage(state.clone())
            .manage(crate::chat_commands::NodeState::empty())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let result = set_app_settings(
            app.handle().clone(),
            app.state(),
            app.state(),
            app.state(),
            Some("settings-owner".into()),
            state.get(),
            Some(serde_json::json!({"stay_signed_in": true})),
            None,
            None,
        )
        .await;
        assert!(matches!(result, Err(CommandError::Auth(_))));
        assert!(!state.get().stay_signed_in);
        assert!(!root.path().join("settings.json").exists());
        assert!(!root.path().join("account.json").exists());
    }

    #[tokio::test]
    async fn successful_settings_save_releases_serialization_before_live_application() {
        use tauri::Manager;
        let root = tempfile::tempdir().unwrap();
        let auth = crate::state::AppState::new(crate::services::auth_service::AuthService::new(
            Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().into()),
            )),
        ));
        let user = crate::services::user::User {
            user_id: "settings-success-owner".into(),
            name: "Owner".into(),
            account_login_name: None,
            display_name: "Owner".into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        let lease = auth.session().publish("session".into(), user, "pw".into());
        let state = SettingsState::isolated(root.path().join("settings.json"));
        state.activate_at(lease.owner(), &root.path().join("missing.json"));
        let node = crate::chat_commands::NodeState::empty();
        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            lease.owner(),
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
        let account = runtime.account_id().to_owned();
        *node.0.write().await = Some(runtime);
        let app = tauri::test::mock_builder()
            .manage(auth)
            .manage(state.clone())
            .manage(node.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            set_app_settings(
                app.handle().clone(),
                app.state(),
                app.state(),
                app.state(),
                Some(lease.owner().into()),
                state.get(),
                Some(serde_json::json!({"ringtone": "digital"})),
                Some(account),
                None,
            ),
        )
        .await
        .expect("save must release serialization before applying")
        .unwrap();
        assert_eq!(state.get().ringtone, "digital");
        assert!(state.account_path(lease.owner()).unwrap().exists());
        assert!(state.1.try_lock().is_ok());
        node.0.write().await.take().unwrap().stop().await;
    }

    #[test]
    fn imported_settings_keep_local_protections_and_accept_stronger_preferences() {
        let previous = AppSettings::default().with_privacy_level(PrivacyLevel::Strict);
        let mut imported = AppSettings::default().with_privacy_level(PrivacyLevel::Custom);
        imported.auto_lock_seconds = 30;
        imported.ringtone = "digital".into();
        let next = protect_imported_settings(&previous, imported);
        assert_eq!(next.auto_lock_seconds, 30);
        assert!(next.lock_when_hidden);
        assert!(next.confirm_sensitive_copy);
        assert!(!next.notification_preview);
        assert_eq!(next.clipboard_clear_seconds, 30);
        assert_eq!(next.ringtone, "digital");
        assert_eq!(next.privacy_level, PrivacyLevel::Custom);
        assert!(!requires_identity_verification(&previous, &next));
        assert_eq!(normalize_settings(next).unwrap().auto_lock_seconds, 30);
    }

    #[test]
    fn sensitive_settings_compare_effective_protections() {
        let strict = AppSettings::default().with_privacy_level(PrivacyLevel::Strict);
        let mut cosmetic = strict.clone();
        cosmetic.ringtone = "other".into();
        assert!(!requires_identity_verification(&strict, &cosmetic));
        assert!(requires_identity_verification(
            &strict,
            &AppSettings::default()
        ));
        assert!(!requires_identity_verification(
            &AppSettings::default(),
            &strict
        ));
        let mut custom = strict.clone();
        custom.privacy_level = PrivacyLevel::Custom;
        custom.auto_lock_seconds = 120;
        assert!(requires_identity_verification(&strict, &custom));
        custom = strict.clone();
        custom.notification_preview = true;
        assert!(requires_identity_verification(&strict, &custom));
        let mut signed_out = strict.clone();
        signed_out.stay_signed_in = false;
        assert!(requires_identity_verification(&signed_out, &strict));
    }

    #[tokio::test]
    async fn startup_reads_local_settings_before_the_cryptographic_runtime_is_ready() {
        use tauri::Manager;
        let root = tempfile::tempdir().unwrap();
        let app = crate::state::AppState::new(crate::services::auth_service::AuthService::new(
            Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        let user = crate::services::user::User {
            user_id: "startup-settings-owner".into(),
            account_login_name: None,
            name: "settings".into(),
            display_name: "Settings".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        app.session().publish("first".into(), user, "pw".into());
        let settings = SettingsState::default();
        settings.0.lock().unwrap().owner = Some("startup-settings-owner".into());
        settings
            .update_for_owner(
                "startup-settings-owner",
                AppSettings {
                    notifications: false,
                    ..AppSettings::default()
                },
            )
            .unwrap();
        let host = tauri::test::mock_builder()
            .manage(app)
            .manage(settings)
            .manage(crate::chat_commands::NodeState::empty())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let values = get_app_settings(
            host.state(),
            host.state(),
            host.state(),
            Some("startup-settings-owner".into()),
            None,
        )
        .await
        .unwrap();
        assert!(!values.notifications);
        assert!(
            get_app_settings(
                host.state(),
                host.state(),
                host.state(),
                Some("wrong-owner".into()),
                None
            )
            .await
            .is_err()
        );
        assert!(
            get_app_settings(
                host.state(),
                host.state(),
                host.state(),
                Some("startup-settings-owner".into()),
                Some("not-started-account".into())
            )
            .await
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_settings_seed_project_without_echo_and_survive_restart_with_local_paths() {
        use mesh_talk_core::node::{NodeRuntime, SyncCategory};
        let root = tempfile::tempdir().unwrap();
        let app = crate::state::AppState::new(crate::services::auth_service::AuthService::new(
            Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        let user = crate::services::user::User {
            user_id: "shared-settings-owner".into(),
            account_login_name: None,
            name: "settings".into(),
            display_name: "Settings".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        app.session()
            .publish("first".into(), user.clone(), "pw".into());
        let nodes = crate::chat_commands::NodeState::empty();
        let runtime = NodeRuntime::start(
            root.path(),
            "shared-settings-owner",
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
        let account = runtime.account_id().to_owned();
        assert!(require_settings_account(&runtime, None).is_err());
        assert!(require_settings_account(&runtime, Some("wrong-account")).is_err());
        assert!(require_settings_account(&runtime, Some(&account)).is_ok());
        let handle = runtime.handle();
        *nodes.0.write().await = Some(runtime);
        let state = SettingsState::default();
        let path = config_store::account_path(root.path(), "shared-settings-owner", SETTINGS_FILE)
            .unwrap();
        state.activate_at("shared-settings-owner", &path);
        state
            .update_for_owner(
                "shared-settings-owner",
                AppSettings {
                    ringtone: "marimba".into(),
                    download_dir: "/this-device".into(),
                    ..AppSettings::default()
                },
            )
            .unwrap();
        handle
            .set_device_sync_category(SyncCategory::Appearance, true)
            .unwrap();
        let admission = RuntimeAdmission::capture_current(&app, &nodes).unwrap();
        assert!(
            reconcile_shared_settings(
                &admission,
                &state,
                "shared-settings-owner",
                "wrong-account",
                true,
                root.path()
            )
            .await
            .is_err()
        );
        assert!(
            handle
                .account_preferences(SyncCategory::Appearance)
                .is_empty()
        );
        reconcile_shared_settings(
            &admission,
            &state,
            "shared-settings-owner",
            &account,
            false,
            root.path(),
        )
        .await
        .unwrap();
        assert!(
            handle
                .account_preferences(SyncCategory::Appearance)
                .is_empty(),
            "a new target never exports defaults"
        );
        handle
            .set_account_preference(
                SyncCategory::Appearance,
                "settings.notifications".into(),
                None,
            )
            .unwrap();
        reconcile_shared_settings(
            &admission,
            &state,
            "shared-settings-owner",
            &account,
            true,
            root.path(),
        )
        .await
        .unwrap();
        assert_eq!(
            handle.account_preferences(SyncCategory::Appearance).len(),
            15,
            "the existing notifications tombstone is one of the 15 portable fields"
        );
        assert!(
            handle
                .account_preferences(SyncCategory::Appearance)
                .iter()
                .find(|row| row.key == "settings.notifications")
                .unwrap()
                .value
                .is_none(),
            "source seeding preserves tombstones"
        );
        for (key, value) in [
            ("ringtone", "\"digital\""),
            ("notifications", "false"),
            ("physical_lan_only", "true"),
        ] {
            handle
                .set_account_preference(
                    SyncCategory::Appearance,
                    format!("settings.{key}"),
                    Some(value.into()),
                )
                .unwrap();
        }
        handle
            .set_device_sync_category(SyncCategory::Appearance, false)
            .unwrap();
        let paused = reconcile_shared_settings(
            &admission,
            &state,
            "shared-settings-owner",
            &account,
            false,
            root.path(),
        )
        .await
        .unwrap();
        assert_eq!(paused.ringtone, "marimba");
        assert!(!handle.physical_lan_only());
        handle
            .set_device_sync_category(SyncCategory::Appearance, true)
            .unwrap();
        let before: std::collections::BTreeMap<_, _> = handle
            .account_preferences(SyncCategory::Appearance)
            .into_iter()
            .map(|row| (row.key.clone(), row))
            .collect();
        let applied = reconcile_shared_settings(
            &admission,
            &state,
            "shared-settings-owner",
            &account,
            false,
            root.path(),
        )
        .await
        .unwrap();
        assert_eq!(applied.ringtone, "digital");
        assert!(!applied.notifications);
        assert!(
            handle.physical_lan_only(),
            "import applies existing native runtime policy"
        );
        assert_eq!(applied.download_dir, "/this-device");
        let after: std::collections::BTreeMap<_, _> = handle
            .account_preferences(SyncCategory::Appearance)
            .into_iter()
            .map(|row| (row.key.clone(), row))
            .collect();
        assert_eq!(before, after, "remote application creates no new revisions");
        assert!(!state.needs_application(nodes.0.read().await.as_ref().unwrap(), &admission));
        // Model a durable projection whose live application was interrupted by
        // locking: the next unchanged snapshot must retry, without re-export.
        state.0.lock().unwrap().applied = None;
        handle
            .set_physical_lan_only_if(false, |commit| commit())
            .await
            .unwrap();
        reconcile_shared_settings(
            &admission,
            &state,
            "shared-settings-owner",
            &account,
            false,
            root.path(),
        )
        .await
        .unwrap();
        assert!(handle.physical_lan_only());
        assert!(!state.needs_application(nodes.0.read().await.as_ref().unwrap(), &admission));
        app.session()
            .publish("replacement".into(), user, "pw".into());
        assert!(
            reconcile_shared_settings(
                &admission,
                &state,
                "shared-settings-owner",
                &account,
                true,
                root.path()
            )
            .await
            .is_err()
        );
        drop(handle);
        nodes.0.write().await.take().unwrap().stop().await;
        let restarted = NodeRuntime::start(
            root.path(),
            "shared-settings-owner",
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
        let reopened: std::collections::BTreeMap<_, _> = restarted
            .handle()
            .account_preferences(SyncCategory::Appearance)
            .into_iter()
            .map(|row| (row.key.clone(), row))
            .collect();
        assert_eq!(after, reopened);
        let replacement_admission = RuntimeAdmission::capture_current(&app, &nodes).unwrap();
        assert!(
            state.needs_application(&restarted, &replacement_admission),
            "a new runtime never inherits an old applied token"
        );
        state.activate_at("shared-settings-owner", &path);
        assert_eq!(state.get().ringtone, "digital");
        assert_eq!(state.get().download_dir, "/this-device");
        restarted.stop().await;
    }

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
                account_login_name: None,
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
                        origin_sharing_enabled: true,
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
            assert!(
                !handle.origin_sharing_enabled(),
                "obsolete settings cannot enable origin disclosure"
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
                account_login_name: None,
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
                        origin_sharing_enabled: true,
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
            assert_eq!(
                handle.origin_sharing_enabled(),
                change == "current",
                "origin disclosure policy for {change}"
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
        assert_eq!(
            serde_json::to_value(&s).unwrap()["origin_sharing_enabled"],
            serde_json::json!(false),
            "existing installations do not opt into origin disclosure"
        );
        assert!(s.bridge_hub_id.is_empty(), "no hub is pinned by default");
    }

    #[test]
    fn origin_sharing_is_owner_scoped_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let alice_path = config_store::account_path(root.path(), "alice", SETTINGS_FILE).unwrap();
        let bob_path = config_store::account_path(root.path(), "bob", SETTINGS_FILE).unwrap();
        let settings: AppSettings = serde_json::from_value(serde_json::json!({
            "origin_sharing_enabled": true
        }))
        .unwrap();
        config_store::write_account(&alice_path, &AccountSettings::from(&settings)).unwrap();
        let state = SettingsState::default();
        state.activate_at("alice", &alice_path);
        assert_eq!(
            serde_json::to_value(state.get()).unwrap()["origin_sharing_enabled"],
            serde_json::json!(true)
        );
        state.activate_at("bob", &bob_path);
        assert_eq!(
            serde_json::to_value(state.get()).unwrap()["origin_sharing_enabled"],
            serde_json::json!(false)
        );
        state.activate_at("alice", &alice_path);
        assert_eq!(
            serde_json::to_value(state.get()).unwrap()["origin_sharing_enabled"],
            serde_json::json!(true)
        );
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
