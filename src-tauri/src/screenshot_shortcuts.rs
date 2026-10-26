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
struct NativeRegistry<'a, R: Runtime>(&'a tauri::AppHandle<R>);
impl<R: Runtime> Registry for NativeRegistry<'_, R> {
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
        let previous = self.inner.lock().unwrap().clone();
        let bindings = if settings.global {
            settings.bindings()?
        } else {
            Vec::new()
        };
        let before: Vec<_> = previous.bindings.iter().map(|item| item.0).collect();
        let after: Vec<_> = bindings.iter().map(|item| item.0).collect();
        if before != after {
            let handle = app.clone();
            let candidates = previous
                .bindings
                .iter()
                .chain(&bindings)
                .copied()
                .collect::<Vec<_>>();
            let (result, actual) = tokio::task::spawn_blocking(move || {
                let result = replace_keys(&mut NativeRegistry(&handle), &before, &after);
                let mut actual = Vec::new();
                for binding in candidates {
                    if !actual.iter().any(|(key, _)| *key == binding.0)
                        && handle.global_shortcut().is_registered(binding.0)
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
        let _operation = self.operation.lock().await;
        self.suspend_locked(app, registration_id).await
    }
    /// Caller owns the registration operation lock; the token check is made
    /// inside that serialization rather than before an asynchronous wait.
    async fn suspend_locked<R: Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        registration_id: u64,
    ) -> Result<(), CommandError> {
        let previous = self.inner.lock().unwrap().clone();
        if previous.registration_id != registration_id {
            return Ok(());
        }
        let before: Vec<_> = previous.bindings.iter().map(|item| item.0).collect();
        if !before.is_empty() {
            let handle = app.clone();
            tokio::task::spawn_blocking(move || {
                replace_keys(&mut NativeRegistry(&handle), &before, &[])
            })
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
    let state = app.state::<ScreenshotShortcutState>();
    let matches = state
        .inner
        .lock()
        .unwrap()
        .lease
        .as_ref()
        .is_some_and(|lease| lease.owner() == owner && lease.generation() == generation);
    if matches {
        state.suspend(&app, registration_id).await?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn wake_screenshot_window(
    app: tauri::AppHandle,
    owner: String,
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
    let state = app.state::<ScreenshotShortcutState>();
    let snapshot = state.inner.lock().unwrap().clone();
    if snapshot
        .lease
        .as_ref()
        .is_some_and(|lease| !current(&app.state::<AppState>(), lease))
        && let Err(error) = state.suspend(app, snapshot.registration_id).await
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
    }
}
