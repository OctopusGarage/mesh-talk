//! Account-local capture accelerators and their native registration lifetime.
use crate::{
    commands::CommandError,
    settings::SettingsState,
    state::{AppState, SessionLease},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenshotShortcuts {
    pub capture: Option<String>,
    pub capture_hidden: Option<String>,
    pub global: bool,
}
impl Default for ScreenshotShortcuts {
    fn default() -> Self {
        Self {
            capture: Some("CommandOrControl+Shift+KeyX".into()),
            capture_hidden: Some("CommandOrControl+Alt+Shift+KeyX".into()),
            global: false,
        }
    }
}
impl ScreenshotShortcuts {
    pub(crate) fn validate(&self) -> Result<(), CommandError> {
        let keys = self.bindings()?;
        if self.global && keys.is_empty() {
            return Err(CommandError::InvalidInput(
                "set a screenshot shortcut before enabling global activation".into(),
            ));
        }
        if keys.len() == 2 && keys[0].0 == keys[1].0 {
            return Err(CommandError::InvalidInput(
                "screenshot shortcuts must be different".into(),
            ));
        }
        Ok(())
    }
    fn bindings(&self) -> Result<Vec<(Shortcut, bool)>, CommandError> {
        [(&self.capture, false), (&self.capture_hidden, true)]
            .into_iter()
            .filter_map(|(text, hidden)| {
                text.as_deref()
                    .map(|text| parse(text).map(|key| (key, hidden)))
            })
            .collect()
    }
}
fn parse(text: &str) -> Result<Shortcut, CommandError> {
    let invalid = || CommandError::InvalidInput("invalid or reserved screenshot shortcut".into());
    let parts: Vec<_> = text.split('+').collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return Err(invalid());
    };
    let order = ["CommandOrControl", "Control", "Alt", "Shift", "Super"];
    let mut last = None;
    for modifier in modifiers {
        let Some(index) = order.iter().position(|value| value == modifier) else {
            return Err(invalid());
        };
        if last.is_some_and(|last| index <= last) {
            return Err(invalid());
        }
        last = Some(index);
    }
    let primary = modifiers.contains(&"CommandOrControl") || modifiers.contains(&"Control");
    let alt = modifiers.contains(&"Alt");
    let shift = modifiers.contains(&"Shift");
    if modifiers.contains(&"Super")
        || (modifiers.contains(&"CommandOrControl") && modifiers.contains(&"Control"))
        || (!primary && !alt)
    {
        return Err(invalid());
    }
    let letter = key
        .strip_prefix("Key")
        .filter(|v| v.len() == 1 && v.as_bytes()[0].is_ascii_uppercase());
    let digit = key
        .strip_prefix("Digit")
        .is_some_and(|v| v.len() == 1 && v.as_bytes()[0].is_ascii_digit());
    let function = key
        .strip_prefix('F')
        .and_then(|v| v.parse::<u8>().ok())
        .filter(|n| (1..=12).contains(n))
        .is_some_and(|n| format!("F{n}") == *key);
    if letter.is_none() && !digit && !function {
        return Err(invalid());
    }
    if (alt && *key == "F4")
        || (primary
            && !alt
            && letter.is_some_and(|letter| {
                if shift {
                    "NPQTVWZ".contains(letter)
                } else {
                    "ACFHLNOPQRSTVWXZ".contains(letter)
                }
            }))
    {
        return Err(invalid());
    }
    text.parse().map_err(|_| invalid())
}

trait Registry {
    type Key: Copy + PartialEq;
    fn register(&mut self, key: Self::Key) -> Result<(), String>;
    fn unregister(&mut self, key: Self::Key) -> Result<(), String>;
    fn is_registered(&self, key: Self::Key) -> bool;
}
/// Register additions before touching active bindings. Rollback failures are surfaced too.
fn replace_keys<R: Registry>(
    registry: &mut R,
    previous: &[R::Key],
    next: &[R::Key],
) -> Result<(), String> {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let operation = (|| {
        for key in next.iter().filter(|key| !previous.contains(key)) {
            registry.register(*key)?;
            added.push(*key);
        }
        for key in previous.iter().filter(|key| !next.contains(key)) {
            registry.unregister(*key)?;
            removed.push(*key);
        }
        Ok::<(), String>(())
    })();
    if let Err(mut error) = operation {
        for key in removed {
            if let Err(rollback) = registry.register(key) {
                error.push_str(&format!("; restoring previous binding failed: {rollback}"));
            }
        }
        for key in added {
            if let Err(rollback) = registry.unregister(key) {
                error.push_str(&format!("; removing new binding failed: {rollback}"));
            }
        }
        return Err(error);
    }
    Ok(())
}
struct NativeRegistry<R: Runtime>(tauri::AppHandle<R>);
impl<R: Runtime> Registry for NativeRegistry<R> {
    type Key = Shortcut;
    fn register(&mut self, key: Shortcut) -> Result<(), String> {
        self.0
            .global_shortcut()
            .register(key)
            .map_err(|error| error.to_string())
    }
    fn unregister(&mut self, key: Shortcut) -> Result<(), String> {
        self.0
            .global_shortcut()
            .unregister(key)
            .map_err(|error| error.to_string())
    }
    fn is_registered(&self, key: Shortcut) -> bool {
        self.0.global_shortcut().is_registered(key)
    }
}

#[derive(Clone, Default)]
pub(crate) struct ScreenshotShortcutState {
    pub(crate) operation: Arc<tokio::sync::Mutex<()>>,
    inner: Arc<Mutex<Registration>>,
}
#[derive(Clone, Default)]
struct Registration {
    lease: Option<SessionLease>,
    bindings: Vec<(Shortcut, bool)>,
    pressed: BTreeSet<u32>,
    registration_id: u64,
    error: Option<String>,
    degraded: bool,
}
#[derive(Serialize)]
pub(crate) struct ShortcutStatus {
    global_active: bool,
    error: Option<String>,
    generation: u64,
    registration_id: u64,
}

fn current(app: &AppState, lease: &SessionLease) -> bool {
    app.session()
        .matching(lease, |_| !app.app_lock().is_locked())
        .unwrap_or(false)
}

impl ScreenshotShortcutState {
    /// Caller holds `operation`; no mutex/session guard spans a main-thread dispatch.
    pub(crate) async fn apply<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        lease: &SessionLease,
        settings: &ScreenshotShortcuts,
    ) -> Result<(), CommandError> {
        settings.validate()?;
        // The official global-hotkey backend is X11-only on Linux. Its worker can
        // disconnect without returning an error to register(), so never report
        // successful global coverage for a native Wayland window.
        #[cfg(target_os = "linux")]
        if settings.global
            && !app
                .get_webview_window("main")
                .is_some_and(|window| crate::window_visibility::is_x11(&window.as_ref().window()))
        {
            return Err(CommandError::InvalidInput(
                "global screenshot shortcuts are unavailable in this Wayland session; use application shortcuts".into(),
            ));
        }
        let handle = app.clone();
        self.apply_with_registry(lease, settings, move || NativeRegistry(handle))
            .await
    }

    async fn apply_with_registry<F, G>(
        &self,
        lease: &SessionLease,
        settings: &ScreenshotShortcuts,
        make_registry: F,
    ) -> Result<(), CommandError>
    where
        F: FnOnce() -> G + Send + 'static,
        G: Registry<Key = Shortcut> + Send + 'static,
    {
        settings.validate()?;
        let previous = self.inner.lock().unwrap().clone();
        let bindings = if settings.global {
            settings.bindings()?
        } else {
            Vec::new()
        };
        let before: Vec<_> = previous.bindings.iter().map(|item| item.0).collect();
        let after: Vec<_> = bindings.iter().map(|item| item.0).collect();
        if before != after {
            let candidates = previous
                .bindings
                .iter()
                .chain(&bindings)
                .copied()
                .collect::<Vec<_>>();
            let (result, actual) = tokio::task::spawn_blocking(move || {
                let mut registry = make_registry();
                let result = replace_keys(&mut registry, &before, &after);
                let mut actual = Vec::new();
                for binding in candidates {
                    if !actual.iter().any(|(key, _)| *key == binding.0)
                        && registry.is_registered(binding.0)
                    {
                        actual.push(binding);
                    }
                }
                (result, actual)
            })
            .await
            .map_err(|error| CommandError::Internal(error.to_string()))?;
            if let Err(error) = result {
                let mut inner = self.inner.lock().unwrap();
                // Keep every actual OS registration reachable for retry/retirement.
                // A partial rollback must never claim full global coverage.
                inner.degraded = actual != previous.bindings;
                inner.bindings = actual;
                if !inner.bindings.is_empty() && inner.lease.is_none() {
                    inner.lease = Some(lease.clone());
                }
                inner.error = Some(error.clone());
                return Err(CommandError::Internal(format!(
                    "could not register screenshot shortcuts: {error}"
                )));
            }
        }
        let mut inner = self.inner.lock().unwrap();
        inner.lease = Some(lease.clone());
        inner.bindings = bindings;
        inner.pressed.clear();
        inner.registration_id = inner.registration_id.wrapping_add(1);
        inner.error = None;
        inner.degraded = false;
        Ok(())
    }
    fn status(&self, lease: &SessionLease) -> ShortcutStatus {
        let inner = self.inner.lock().unwrap();
        let matching = inner.lease.as_ref() == Some(lease);
        ShortcutStatus {
            global_active: matching && !inner.degraded && !inner.bindings.is_empty(),
            error: inner.error.clone(),
            generation: lease.generation(),
            registration_id: inner.registration_id,
        }
    }
    async fn suspend<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        registration_id: u64,
    ) -> Result<(), CommandError> {
        let handle = app.clone();
        self.suspend_with_registry(move || NativeRegistry(handle), registration_id)
            .await
    }
    async fn suspend_with_registry<F, G>(
        &self,
        make_registry: F,
        registration_id: u64,
    ) -> Result<(), CommandError>
    where
        F: FnOnce() -> G + Send + 'static,
        G: Registry<Key = Shortcut> + Send + 'static,
    {
        let _operation = self.operation.lock().await;
        self.suspend_locked_with_registry(make_registry, registration_id)
            .await
    }
    /// Caller owns the registration operation lock; the token check is made
    /// inside that serialization rather than before an asynchronous wait.
    async fn suspend_locked<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        registration_id: u64,
    ) -> Result<(), CommandError> {
        let handle = app.clone();
        self.suspend_locked_with_registry(move || NativeRegistry(handle), registration_id)
            .await
    }
    async fn suspend_locked_with_registry<F, G>(
        &self,
        make_registry: F,
        registration_id: u64,
    ) -> Result<(), CommandError>
    where
        F: FnOnce() -> G + Send + 'static,
        G: Registry<Key = Shortcut> + Send + 'static,
    {
        let previous = self.inner.lock().unwrap().clone();
        if previous.registration_id != registration_id {
            return Ok(());
        }
        let before: Vec<_> = previous.bindings.iter().map(|item| item.0).collect();
        if !before.is_empty() {
            tokio::task::spawn_blocking(move || replace_keys(&mut make_registry(), &before, &[]))
                .await
                .map_err(|error| CommandError::Internal(error.to_string()))?
                .map_err(CommandError::Internal)?;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.bindings.clear();
        inner.pressed.clear();
        inner.lease = None;
        inner.registration_id = inner.registration_id.wrapping_add(1);
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn sync_screenshot_shortcuts(
    app: tauri::AppHandle,
    owner: String,
) -> Result<ShortcutStatus, CommandError> {
    sync_screenshot_shortcuts_for(&app, &owner).await
}

async fn sync_screenshot_shortcuts_for<R: Runtime>(
    app: &tauri::AppHandle<R>,
    owner: &str,
) -> Result<ShortcutStatus, CommandError> {
    let app_state = app.state::<AppState>();
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if lease.owner() != owner || !current(&app_state, &lease) {
        return Err(CommandError::Authorization(
            "screenshot session unavailable".into(),
        ));
    }
    let state = app.state::<ScreenshotShortcutState>();
    let operation = state.operation.lock().await;
    if !current(&app_state, &lease) {
        return Err(CommandError::Authorization(
            "screenshot session replaced".into(),
        ));
    }
    let settings = app
        .state::<SettingsState>()
        .get_for_owner(owner)?
        .screenshot_shortcuts;
    // Startup conflicts are observable without discarding the saved binding.
    let result = state.apply(app, &lease, &settings).await;
    if !current(&app_state, &lease) {
        let registration_id = state.inner.lock().unwrap().registration_id;
        drop(operation);
        state.suspend(app, registration_id).await?;
        return Err(CommandError::Authorization(
            "screenshot session replaced".into(),
        ));
    }
    if let Err(error) = result {
        state.inner.lock().unwrap().error = Some(error.to_string());
    }
    Ok(state.status(&lease))
}

/// Pause OS ownership before a recorder accepts input. Capture authority before
/// waiting so an old renderer cannot pause a replacement login of the same owner.
async fn pause_screenshot_shortcuts_for<R: Runtime>(
    app: &tauri::AppHandle<R>,
    owner: &str,
) -> Result<(), CommandError> {
    let app_state = app.state::<AppState>();
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if lease.owner() != owner || !current(&app_state, &lease) {
        return Err(CommandError::Authorization(
            "screenshot session unavailable".into(),
        ));
    }
    let state = app.state::<ScreenshotShortcutState>();
    let _operation = state.operation.lock().await;
    if !current(&app_state, &lease) {
        return Err(CommandError::Authorization(
            "screenshot session replaced".into(),
        ));
    }
    let snapshot = state.inner.lock().unwrap().clone();
    if snapshot.lease.as_ref() == Some(&lease) {
        state.suspend_locked(app, snapshot.registration_id).await?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn pause_screenshot_shortcuts(
    app: tauri::AppHandle,
    owner: String,
) -> Result<(), CommandError> {
    pause_screenshot_shortcuts_for(&app, &owner).await
}

#[tauri::command]
pub(crate) async fn suspend_screenshot_shortcuts(
    app: tauri::AppHandle,
    owner: String,
    generation: u64,
    registration_id: u64,
) -> Result<(), CommandError> {
    suspend_screenshot_shortcuts_for(&app, &owner, generation, registration_id).await
}

async fn suspend_screenshot_shortcuts_for<R: Runtime>(
    app: &tauri::AppHandle<R>,
    owner: &str,
    generation: u64,
    registration_id: u64,
) -> Result<(), CommandError> {
    let state = app.state::<ScreenshotShortcutState>();
    let matches = state
        .inner
        .lock()
        .unwrap()
        .lease
        .as_ref()
        .is_some_and(|lease| lease.owner() == owner && lease.generation() == generation);
    if matches {
        state.suspend(app, registration_id).await?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn wake_screenshot_window(
    app: tauri::AppHandle,
    owner: String,
) -> Result<(), CommandError> {
    wake_screenshot_window_for(&app, &owner).await
}

async fn wake_screenshot_window_for<R: Runtime>(
    app: &tauri::AppHandle<R>,
    owner: &str,
) -> Result<(), CommandError> {
    let app_state = app.state::<AppState>();
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if lease.owner() != owner || !current(&app_state, &lease) {
        return Err(CommandError::Authorization(
            "screenshot session unavailable".into(),
        ));
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let app_state = handle.state::<AppState>();
        let _ = app_state.session().matching(&lease, |_| {
            if app_state.app_lock().is_locked() {
                return;
            }
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = crate::window_visibility::show(&window.as_ref().window());
                let _ = window.set_focus();
            }
        });
    })
    .map_err(|error| CommandError::Internal(error.to_string()))
}

/// Screenshot pixels and restoration belong to the exact unlocked session that
/// initiated acquisition, including lock/unlock cycles within one login.
type QueuedCaptureAction = (
    Box<dyn FnOnce() + Send>,
    tokio::sync::oneshot::Receiver<Result<(), CommandError>>,
);

#[derive(Clone)]
pub(crate) struct CaptureAuthority {
    app: AppState,
    lease: SessionLease,
    revision: u64,
}
impl CaptureAuthority {
    pub(crate) fn capture(app: &AppState) -> Result<Self, CommandError> {
        let lease = app
            .session()
            .capture()
            .map_err(CommandError::Authorization)?;
        let revision = app.app_lock().authorization_revision()?;
        let authority = Self {
            app: app.clone(),
            lease,
            revision,
        };
        authority.ensure_current()?;
        Ok(authority)
    }
    pub(crate) fn with_current<T>(&self, operation: impl FnOnce() -> T) -> Result<T, CommandError> {
        self.app
            .session()
            .matching(&self.lease, |_| {
                self.app
                    .app_lock()
                    .with_unchanged_revision(self.revision, || Ok(operation()))
            })
            .map_err(CommandError::Authorization)?
    }
    pub(crate) fn ensure_current(&self) -> Result<(), CommandError> {
        self.with_current(|| ())
    }
    fn queued_action(&self, operation: impl FnOnce() + Send + 'static) -> QueuedCaptureAction {
        let authority = self.clone();
        let (send, completed) = tokio::sync::oneshot::channel();
        let queued = Box::new(move || {
            let _ = send.send(authority.with_current(operation));
        });
        (queued, completed)
    }
    /// Schedule without holding authority guards, then authorize at the actual
    /// main-thread UI commit. Linux visibility queries can dispatch synchronously.
    pub(crate) async fn on_main_thread<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        operation: impl FnOnce() + Send + 'static,
    ) -> Result<(), CommandError> {
        let (queued, completed) = self.queued_action(operation);
        app.run_on_main_thread(queued)
            .map_err(|error| CommandError::Internal(error.to_string()))?;
        completed
            .await
            .map_err(|error| CommandError::Internal(error.to_string()))?
    }
}

#[derive(Clone, Serialize)]
struct CaptureRequest {
    owner: String,
    generation: u64,
    hide_window: bool,
}

pub(crate) fn handle_shortcut<R: Runtime>(
    app: &tauri::AppHandle<R>,
    shortcut: &Shortcut,
    event: tauri_plugin_global_shortcut::ShortcutEvent,
) {
    let state = app.state::<ScreenshotShortcutState>();
    let request = {
        let mut inner = state.inner.lock().unwrap();
        if inner.degraded {
            return;
        }
        if event.state == ShortcutState::Released {
            inner.pressed.remove(&shortcut.id());
            return;
        }
        if !inner.pressed.insert(shortcut.id()) {
            return;
        }
        let Some(lease) = inner.lease.clone() else {
            return;
        };
        let Some((_, hide)) = inner.bindings.iter().find(|(key, _)| key == shortcut) else {
            return;
        };
        (lease, *hide)
    };
    let Ok(authority) = CaptureAuthority::capture(&app.state::<AppState>()) else {
        return;
    };
    if authority.lease != request.0 {
        return;
    }
    let handle = app.clone();
    // Linux global-hotkey callbacks arrive on an X11 worker. Queue before
    // holding session authority around synchronous native window queries.
    let (queued, _completed) = authority.queued_action(move || {
        let hidden = handle.get_webview_window("main").is_none_or(|window| {
            !window.is_visible().unwrap_or(false) || !window.is_focused().unwrap_or(false)
        });
        let _ = handle.emit_to(
            "main",
            "screenshot-shortcut",
            CaptureRequest {
                owner: request.0.owner().to_owned(),
                generation: request.0.generation(),
                hide_window: request.1 || hidden,
            },
        );
    });
    let _ = app.run_on_main_thread(queued);
}

/// Release OS accelerators after native lock/logout, even when the webview is unavailable.
pub(crate) async fn suspend_retired<R: Runtime>(app: &tauri::AppHandle<R>) {
    let handle = app.clone();
    suspend_retired_for(app, move || NativeRegistry(handle)).await;
}

async fn suspend_retired_for<R, F, G>(app: &tauri::AppHandle<R>, make_registry: F)
where
    R: Runtime,
    F: FnOnce() -> G + Send + 'static,
    G: Registry<Key = Shortcut> + Send + 'static,
{
    let state = app.state::<ScreenshotShortcutState>();
    let snapshot = state.inner.lock().unwrap().clone();
    if snapshot
        .lease
        .as_ref()
        .is_some_and(|lease| !current(&app.state::<AppState>(), lease))
        && let Err(error) = state
            .suspend_with_registry(make_registry, snapshot.registration_id)
            .await
    {
        log::warn!("Could not suspend screenshot shortcuts: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_shortcuts_default_to_local_and_survive_old_settings() {
        let settings: crate::settings::AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            settings.screenshot_shortcuts,
            ScreenshotShortcuts::default()
        );
        assert!(!settings.screenshot_shortcuts.global);
    }

    #[test]
    fn screenshot_shortcuts_reject_noncanonical_reserved_and_duplicate_bindings() {
        for text in [
            "Shift+KeyX",
            "Control+KeyC",
            "Control+Shift+KeyV",
            "Alt+F4",
            "Super+KeyX",
            "Shift+Control+KeyX",
            "CommandOrControl+Control+KeyX",
        ] {
            let settings = ScreenshotShortcuts {
                capture: Some(text.into()),
                ..ScreenshotShortcuts::default()
            };
            assert!(settings.validate().is_err(), "accepted {text}");
        }
        let duplicate = ScreenshotShortcuts {
            capture_hidden: ScreenshotShortcuts::default().capture,
            ..ScreenshotShortcuts::default()
        };
        assert!(duplicate.validate().is_err());
        assert!(ScreenshotShortcuts::default().validate().is_ok());
        assert!(
            ScreenshotShortcuts {
                capture: None,
                capture_hidden: None,
                global: true
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn failed_registration_rolls_back_only_new_keys_and_keeps_previous() {
        let mut registry = FakeRegistry {
            keys: vec![1],
            fail_register: Some(3),
            ..Default::default()
        };
        assert!(replace_keys(&mut registry, &[1], &[2, 3]).is_err());
        assert_eq!(registry.keys, vec![1]);
    }

    #[test]
    fn failed_unregistration_restores_removed_old_keys() {
        let mut registry = FakeRegistry {
            keys: vec![1, 2],
            fail_unregister: Some(2),
            ..Default::default()
        };
        assert!(replace_keys(&mut registry, &[1, 2], &[3]).is_err());
        registry.keys.sort();
        assert_eq!(registry.keys, vec![1, 2]);
    }

    #[test]
    fn registration_reports_incomplete_rollback_instead_of_claiming_success() {
        let mut registry = FakeRegistry {
            keys: vec![1, 2],
            fail_register: Some(1),
            fail_unregister: Some(2),
        };
        let error = replace_keys(&mut registry, &[1, 2], &[3]).unwrap_err();
        assert!(error.contains("restoring previous binding failed"));
        assert_eq!(registry.keys, vec![2]);
    }

    #[test]
    fn screenshot_capture_authority_rejects_lock_revision_and_same_owner_replacement() {
        let root = tempfile::tempdir().unwrap();
        let service = crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        ));
        let app = AppState::new(service);
        let user = crate::services::user::User {
            user_id: "screenshot-owner".into(),
            account_login_name: None,
            name: "capture".into(),
            display_name: "Capture".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        app.session()
            .publish("first".into(), user.clone(), "pw".into());
        let authority = CaptureAuthority::capture(&app).unwrap();
        assert!(authority.ensure_current().is_ok());
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = executed.clone();
        let (queued, completed) = authority.queued_action(move || {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        app.app_lock().clear();
        queued();
        assert!(completed.blocking_recv().unwrap().is_err());
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(authority.ensure_current().is_err());
        let authority = CaptureAuthority::capture(&app).unwrap();
        app.session().publish("second".into(), user, "pw".into());
        assert!(authority.ensure_current().is_err());
    }

    #[tokio::test]
    async fn screenshot_shortcut_pause_rejects_replaced_session_after_waiting() {
        let root = tempfile::tempdir().unwrap();
        let service = crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        ));
        let app = AppState::new(service);
        let user = crate::services::user::User {
            user_id: "pause-owner".into(),
            account_login_name: None,
            name: "capture".into(),
            display_name: "Capture".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        let first = app
            .session()
            .publish("first".into(), user.clone(), "pw".into());
        let state = ScreenshotShortcutState::default();
        state.inner.lock().unwrap().lease = Some(first);
        let host = tauri::test::mock_builder()
            .manage(app.clone())
            .manage(state.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let handle = host.handle().clone();
        let operation = state.operation.lock().await;
        let pause = pause_screenshot_shortcuts_for(&handle, "pause-owner");
        tokio::pin!(pause);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pause)
                .await
                .is_err()
        );
        let second = app.session().publish("second".into(), user, "pw".into());
        {
            let mut inner = state.inner.lock().unwrap();
            inner.lease = Some(second.clone());
            inner.registration_id = 5;
        }
        drop(operation);
        assert!(pause.await.is_err());
        assert_eq!(state.inner.lock().unwrap().registration_id, 5);
        assert!(state.inner.lock().unwrap().lease.as_ref() == Some(&second));
        assert!(
            pause_screenshot_shortcuts_for(&handle, "other-owner")
                .await
                .is_err()
        );
        pause_screenshot_shortcuts_for(&handle, "pause-owner")
            .await
            .unwrap();
        assert!(state.inner.lock().unwrap().lease.is_none());
        assert_eq!(state.inner.lock().unwrap().registration_id, 6);
    }

    #[tokio::test]
    async fn screenshot_shortcut_sync_rejects_replaced_session_after_waiting() {
        let root = tempfile::tempdir().unwrap();
        let service = crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        ));
        let app = AppState::new(service);
        let user = crate::services::user::User {
            user_id: "sync-owner".into(),
            account_login_name: None,
            name: "capture".into(),
            display_name: "Capture".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        let first = app
            .session()
            .publish("first".into(), user.clone(), "pw".into());
        let state = ScreenshotShortcutState::default();
        state.inner.lock().unwrap().lease = Some(first);
        let settings = SettingsState::default();
        settings.activate("sync-owner");
        let host = tauri::test::mock_builder()
            .manage(settings)
            .manage(app.clone())
            .manage(state.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let handle = host.handle().clone();
        let operation = state.operation.lock().await;
        let pause = sync_screenshot_shortcuts_for(&handle, "sync-owner");
        tokio::pin!(pause);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pause)
                .await
                .is_err()
        );
        let second = app.session().publish("second".into(), user, "pw".into());
        {
            let mut inner = state.inner.lock().unwrap();
            inner.lease = Some(second.clone());
            inner.registration_id = 5;
        }
        drop(operation);
        assert!(pause.await.is_err());
        assert_eq!(state.inner.lock().unwrap().registration_id, 5);
        assert!(state.inner.lock().unwrap().lease.as_ref() == Some(&second));
    }

    struct ShortcutFixture {
        _root: tempfile::TempDir,
        host: tauri::App<tauri::test::MockRuntime>,
        app: AppState,
        shortcuts: ScreenshotShortcutState,
        lease: SessionLease,
    }

    fn capture_user() -> crate::services::user::User {
        crate::services::user::User {
            user_id: "shortcut-coverage-owner".into(),
            account_login_name: None,
            name: "capture".into(),
            display_name: "Capture".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        }
    }

    fn shortcut_fixture(with_window: bool) -> ShortcutFixture {
        let root = tempfile::tempdir().unwrap();
        let service = crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        ));
        let app = AppState::new(service);
        let lease = app
            .session()
            .publish("capture-session".into(), capture_user(), "pw".into());
        let shortcuts = ScreenshotShortcutState::default();
        let settings = SettingsState::default();
        settings.activate(lease.owner());
        let host = tauri::test::mock_builder()
            .manage(app.clone())
            .manage(app.app_lock().clone())
            .manage(shortcuts.clone())
            .manage(settings)
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        if with_window {
            #[cfg(target_os = "linux")]
            host.manage(crate::window_visibility::HiddenPositions::default());
            tauri::WebviewWindowBuilder::new(&host, "main", Default::default())
                .build()
                .unwrap();
        }
        ShortcutFixture {
            _root: root,
            host,
            app,
            shortcuts,
            lease,
        }
    }

    #[derive(Clone, Default)]
    struct ShortcutRegistry(Arc<Mutex<ShortcutRegistryInner>>);
    #[derive(Default)]
    struct ShortcutRegistryInner {
        keys: Vec<Shortcut>,
        fail_register: Option<Shortcut>,
        fail_unregister: Option<Shortcut>,
    }
    impl Registry for ShortcutRegistry {
        type Key = Shortcut;
        fn register(&mut self, key: Shortcut) -> Result<(), String> {
            let mut inner = self.0.lock().unwrap();
            if inner.fail_register == Some(key) {
                return Err("occupied".into());
            }
            inner.keys.push(key);
            Ok(())
        }
        fn unregister(&mut self, key: Shortcut) -> Result<(), String> {
            let mut inner = self.0.lock().unwrap();
            if inner.fail_unregister == Some(key) {
                return Err("cannot unregister".into());
            }
            inner.keys.retain(|registered| *registered != key);
            Ok(())
        }
        fn is_registered(&self, key: Shortcut) -> bool {
            self.0.lock().unwrap().keys.contains(&key)
        }
    }
    fn global_bindings() -> ScreenshotShortcuts {
        ScreenshotShortcuts {
            capture: Some("Control+Shift+KeyY".into()),
            capture_hidden: Some("Control+Alt+Shift+KeyX".into()),
            global: true,
        }
    }
    async fn apply_registry(
        fixture: &ShortcutFixture,
        registry: &ShortcutRegistry,
        settings: &ScreenshotShortcuts,
    ) -> Result<(), CommandError> {
        let _operation = fixture.shortcuts.operation.lock().await;
        let registry = registry.clone();
        fixture
            .shortcuts
            .apply_with_registry(&fixture.lease, settings, move || registry)
            .await
    }
    fn requests(
        host: &tauri::AppHandle<tauri::test::MockRuntime>,
    ) -> Arc<Mutex<Vec<serde_json::Value>>> {
        use tauri::Listener;
        let received = Arc::new(Mutex::new(Vec::new()));
        let observed = received.clone();
        host.listen_any("screenshot-shortcut", move |event| {
            observed
                .lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        received
    }
    fn shortcut_event(fixture: &ShortcutFixture, key: Shortcut, state: ShortcutState) {
        handle_shortcut(
            fixture.host.handle(),
            &key,
            tauri_plugin_global_shortcut::ShortcutEvent {
                id: key.id(),
                state,
            },
        );
    }
    fn register_event_bindings(fixture: &ShortcutFixture) -> (Shortcut, Shortcut) {
        let capture: Shortcut = "Control+Shift+KeyY".parse().unwrap();
        let hidden: Shortcut = "Control+Alt+Shift+KeyX".parse().unwrap();
        let mut inner = fixture.shortcuts.inner.lock().unwrap();
        inner.lease = Some(fixture.lease.clone());
        inner.bindings = vec![(capture, false), (hidden, true)];
        inner.registration_id = 7;
        (capture, hidden)
    }

    #[tokio::test]
    async fn local_sync_status_and_exact_suspend_tokens_preserve_newer_registration() {
        let fixture = shortcut_fixture(false);
        let handle = fixture.host.handle();
        let status = sync_screenshot_shortcuts_for(handle, fixture.lease.owner())
            .await
            .ok()
            .unwrap();
        assert!(!status.global_active);
        assert!(status.error.is_none());
        assert_eq!(status.generation, fixture.lease.generation());
        assert_eq!(status.registration_id, 1);
        for (owner, generation, token) in [
            ("another-owner", status.generation, 1),
            (fixture.lease.owner(), status.generation + 1, 1),
            (fixture.lease.owner(), status.generation, 2),
        ] {
            assert!(
                suspend_screenshot_shortcuts_for(handle, owner, generation, token)
                    .await
                    .is_ok()
            );
            assert_eq!(fixture.shortcuts.status(&fixture.lease).registration_id, 1);
        }
        assert!(
            suspend_screenshot_shortcuts_for(handle, fixture.lease.owner(), status.generation, 1)
                .await
                .is_ok()
        );
        assert!(fixture.shortcuts.inner.lock().unwrap().lease.is_none());
        let replacement = sync_screenshot_shortcuts_for(handle, fixture.lease.owner())
            .await
            .ok()
            .unwrap();
        assert_eq!(replacement.registration_id, 3);
        assert!(
            suspend_screenshot_shortcuts_for(handle, fixture.lease.owner(), status.generation, 1)
                .await
                .is_ok()
        );
        assert_eq!(fixture.shortcuts.status(&fixture.lease).registration_id, 3);
        assert!(
            pause_screenshot_shortcuts_for(handle, fixture.lease.owner())
                .await
                .is_ok()
        );
        assert!(fixture.shortcuts.inner.lock().unwrap().lease.is_none());
    }

    #[tokio::test]
    async fn sync_and_wake_require_the_current_unlocked_settings_owner() {
        let fixture = shortcut_fixture(true);
        let handle = fixture.host.handle();
        assert!(
            sync_screenshot_shortcuts_for(handle, "another-owner")
                .await
                .is_err()
        );
        assert!(
            wake_screenshot_window_for(handle, "another-owner")
                .await
                .is_err()
        );
        assert!(
            wake_screenshot_window_for(handle, fixture.lease.owner())
                .await
                .is_ok()
        );
        fixture
            .host
            .state::<SettingsState>()
            .activate("different-settings-owner");
        assert!(
            sync_screenshot_shortcuts_for(handle, fixture.lease.owner())
                .await
                .is_err()
        );
        assert!(
            crate::app_lock::lock_app(
                fixture.host.state::<AppState>(),
                fixture.host.state::<crate::app_lock::AppLock>()
            )
            .is_ok()
        );
        assert!(
            sync_screenshot_shortcuts_for(handle, fixture.lease.owner())
                .await
                .is_err()
        );
        assert!(
            wake_screenshot_window_for(handle, fixture.lease.owner())
                .await
                .is_err()
        );
        fixture.app.session().clear();
        assert!(
            wake_screenshot_window_for(handle, fixture.lease.owner())
                .await
                .is_err()
        );
        assert!(
            pause_screenshot_shortcuts_for(handle, fixture.lease.owner())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn global_registry_replacement_reports_success_and_matching_token_releases_keys() {
        let fixture = shortcut_fixture(false);
        let registry = ShortcutRegistry::default();
        assert!(
            apply_registry(&fixture, &registry, &global_bindings())
                .await
                .is_ok()
        );
        let status = fixture.shortcuts.status(&fixture.lease);
        assert!(status.global_active);
        assert!(status.error.is_none());
        assert_eq!(registry.0.lock().unwrap().keys.len(), 2);
        let unchanged = registry.clone();
        assert!(
            fixture
                .shortcuts
                .suspend_with_registry(move || unchanged, status.registration_id + 1)
                .await
                .is_ok()
        );
        assert_eq!(registry.0.lock().unwrap().keys.len(), 2);
        let matching = registry.clone();
        assert!(
            fixture
                .shortcuts
                .suspend_with_registry(move || matching, status.registration_id)
                .await
                .is_ok()
        );
        assert!(registry.0.lock().unwrap().keys.is_empty());
        assert!(fixture.shortcuts.inner.lock().unwrap().lease.is_none());
        assert!(!fixture.shortcuts.status(&fixture.lease).global_active);
    }

    #[tokio::test]
    async fn failed_global_replacement_preserves_active_registration_and_exposes_error() {
        let fixture = shortcut_fixture(false);
        let registry = ShortcutRegistry::default();
        assert!(
            apply_registry(&fixture, &registry, &global_bindings())
                .await
                .is_ok()
        );
        let before = fixture.shortcuts.inner.lock().unwrap().clone();
        let key: Shortcut = "Control+Shift+KeyU".parse().unwrap();
        registry.0.lock().unwrap().fail_register = Some(key);
        let replacement = ScreenshotShortcuts {
            capture: Some("Control+Shift+KeyU".into()),
            capture_hidden: None,
            global: true,
        };
        assert!(
            apply_registry(&fixture, &registry, &replacement)
                .await
                .is_err()
        );
        let inner = fixture.shortcuts.inner.lock().unwrap();
        assert!(inner.bindings == before.bindings);
        assert_eq!(inner.registration_id, before.registration_id);
        assert!(!inner.degraded);
        drop(inner);
        let status = fixture.shortcuts.status(&fixture.lease);
        assert!(status.global_active);
        assert!(
            status
                .error
                .as_ref()
                .is_some_and(|error| error.contains("occupied"))
        );
        let stale_lease =
            fixture
                .app
                .session()
                .publish("replacement".into(), capture_user(), "pw".into());
        assert!(!fixture.shortcuts.status(&stale_lease).global_active);
    }

    #[tokio::test]
    async fn partial_initial_registration_retains_actual_os_keys_for_later_retirement() {
        let fixture = shortcut_fixture(false);
        let registry = ShortcutRegistry::default();
        let keys = global_bindings().bindings().ok().unwrap();
        registry.0.lock().unwrap().fail_register = Some(keys[1].0);
        registry.0.lock().unwrap().fail_unregister = Some(keys[0].0);
        assert!(
            apply_registry(&fixture, &registry, &global_bindings())
                .await
                .is_err()
        );
        {
            let inner = fixture.shortcuts.inner.lock().unwrap();
            assert!(inner.degraded);
            assert!(inner.lease.as_ref() == Some(&fixture.lease));
            assert!(inner.bindings == vec![keys[0]]);
        }
        assert!(!fixture.shortcuts.status(&fixture.lease).global_active);
        fixture.app.session().clear();
        let failing = registry.clone();
        suspend_retired_for(fixture.host.handle(), move || failing).await;
        assert_eq!(registry.0.lock().unwrap().keys.len(), 1);
        assert!(fixture.shortcuts.inner.lock().unwrap().lease.is_some());
        registry.0.lock().unwrap().fail_register = None;
        registry.0.lock().unwrap().fail_unregister = None;
        let retry = registry.clone();
        suspend_retired_for(fixture.host.handle(), move || retry).await;
        assert!(registry.0.lock().unwrap().keys.is_empty());
        assert!(fixture.shortcuts.inner.lock().unwrap().lease.is_none());
    }

    #[tokio::test]
    async fn lock_logout_and_owner_replacement_retire_global_keys_but_live_owner_does_not() {
        for transition in 0..3 {
            let fixture = shortcut_fixture(false);
            let registry = ShortcutRegistry::default();
            assert!(
                apply_registry(&fixture, &registry, &global_bindings())
                    .await
                    .is_ok()
            );
            let active = registry.clone();
            suspend_retired_for(fixture.host.handle(), move || active).await;
            assert_eq!(registry.0.lock().unwrap().keys.len(), 2);
            match transition {
                0 => fixture.app.session().clear(),
                1 => {
                    assert!(
                        crate::app_lock::lock_app(
                            fixture.host.state::<AppState>(),
                            fixture.host.state::<crate::app_lock::AppLock>()
                        )
                        .is_ok()
                    );
                }
                _ => {
                    fixture.app.session().publish(
                        "replacement".into(),
                        capture_user(),
                        "pw".into(),
                    );
                }
            }
            let retired = registry.clone();
            suspend_retired_for(fixture.host.handle(), move || retired).await;
            assert!(registry.0.lock().unwrap().keys.is_empty());
            assert!(fixture.shortcuts.inner.lock().unwrap().lease.is_none());
        }
    }

    #[test]
    fn global_handler_emits_once_per_press_and_release_rearms_both_capture_modes() {
        let fixture = shortcut_fixture(true);
        let received = requests(fixture.host.handle());
        let (capture, hidden) = register_event_bindings(&fixture);
        shortcut_event(&fixture, capture, ShortcutState::Released);
        assert!(received.lock().unwrap().is_empty());
        shortcut_event(&fixture, capture, ShortcutState::Pressed);
        shortcut_event(&fixture, capture, ShortcutState::Pressed);
        assert_eq!(received.lock().unwrap().len(), 1);
        shortcut_event(&fixture, capture, ShortcutState::Released);
        shortcut_event(&fixture, capture, ShortcutState::Pressed);
        shortcut_event(&fixture, hidden, ShortcutState::Pressed);
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event["owner"] == fixture.lease.owner()
                    && event["generation"] == fixture.lease.generation()
                    && event["hide_window"] == true)
        );
    }

    #[test]
    fn global_handler_denies_degraded_missing_unbound_locked_and_retired_authority() {
        for denial in 0..6 {
            let fixture = shortcut_fixture(false);
            let received = requests(fixture.host.handle());
            let (capture, _) = register_event_bindings(&fixture);
            match denial {
                0 => fixture.shortcuts.inner.lock().unwrap().degraded = true,
                1 => fixture.shortcuts.inner.lock().unwrap().lease = None,
                2 => fixture.shortcuts.inner.lock().unwrap().bindings.clear(),
                3 => {
                    assert!(
                        crate::app_lock::lock_app(
                            fixture.host.state::<AppState>(),
                            fixture.host.state::<crate::app_lock::AppLock>()
                        )
                        .is_ok()
                    );
                }
                4 => {
                    fixture.app.session().publish(
                        "replacement".into(),
                        capture_user(),
                        "pw".into(),
                    );
                }
                _ => fixture.app.session().clear(),
            }
            shortcut_event(&fixture, capture, ShortcutState::Pressed);
            assert!(received.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn main_thread_capture_action_runs_only_while_its_authority_is_current() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixture = shortcut_fixture(false);
        let authority = CaptureAuthority::capture(&fixture.app).ok().unwrap();
        let committed = Arc::new(AtomicUsize::new(0));
        let observed = committed.clone();
        assert!(
            authority
                .on_main_thread(fixture.host.handle(), move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                })
                .await
                .is_ok()
        );
        assert_eq!(committed.load(Ordering::SeqCst), 1);
        fixture.app.app_lock().clear();
        let observed = committed.clone();
        assert!(
            authority
                .on_main_thread(fixture.host.handle(), move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                })
                .await
                .is_err()
        );
        assert_eq!(committed.load(Ordering::SeqCst), 1);
    }

    #[derive(Default)]
    struct FakeRegistry {
        keys: Vec<u32>,
        fail_register: Option<u32>,
        fail_unregister: Option<u32>,
    }
    impl Registry for FakeRegistry {
        type Key = u32;
        fn register(&mut self, key: u32) -> Result<(), String> {
            if self.fail_register == Some(key) {
                return Err("occupied".into());
            }
            self.keys.push(key);
            Ok(())
        }
        fn unregister(&mut self, key: u32) -> Result<(), String> {
            if self.fail_unregister == Some(key) {
                return Err("cannot unregister".into());
            }
            self.keys.retain(|current| *current != key);
            Ok(())
        }
        fn is_registered(&self, key: u32) -> bool {
            self.keys.contains(&key)
        }
    }
}
