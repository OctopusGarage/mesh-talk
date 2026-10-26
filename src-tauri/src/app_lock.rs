//! Native session lock. The node continues receiving while app IPC is denied.
use crate::commands::CommandError;
use crate::state::{AppState, SessionLease};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

struct LockInner {
    lease: Option<SessionLease>,
    generation: u64,
    authorization_revision: u64,
    last_activity: Instant,
    call_active: bool,
}

impl Default for LockInner {
    fn default() -> Self {
        Self {
            lease: None,
            generation: 0,
            authorization_revision: 0,
            last_activity: Instant::now(),
            call_active: false,
        }
    }
}

fn due(inner: &LockInner, settings: &crate::settings::AppSettings, hidden: bool) -> bool {
    inner.lease.is_none()
        && !inner.call_active
        && ((settings.lock_when_hidden && hidden)
            || (settings.auto_lock_seconds > 0
                && inner.last_activity.elapsed()
                    >= Duration::from_secs(settings.auto_lock_seconds as u64)))
}

#[derive(Clone, Default)]
pub struct AppLock(Arc<Mutex<LockInner>>);

impl AppLock {
    pub(crate) fn authorization_revision(&self) -> Result<u64, CommandError> {
        self.0
            .lock()
            .map(|inner| inner.authorization_revision)
            .map_err(|_| CommandError::Internal("lock unavailable".into()))
    }

    /// Keep lock transitions serialized through the final sensitive projection.
    pub(crate) fn with_unchanged_revision<T>(
        &self,
        revision: u64,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let inner = self
            .0
            .lock()
            .map_err(|_| CommandError::Internal("lock unavailable".into()))?;
        if inner.lease.is_some() || inner.authorization_revision != revision {
            return Err(CommandError::Authorization(
                "app lock changed during authentication".into(),
            ));
        }
        operation()
    }

    pub fn is_locked(&self) -> bool {
        self.0.lock().map_or(true, |lock| lock.lease.is_some())
    }

    fn lock(&self, lease: SessionLease) -> Result<(), CommandError> {
        let mut inner = self
            .0
            .lock()
            .map_err(|_| CommandError::Internal("lock unavailable".into()))?;
        inner.authorization_revision = inner.authorization_revision.wrapping_add(1);
        inner.generation = lease.generation();
        inner.lease = Some(lease);
        inner.call_active = false;
        Ok(())
    }

    fn unlock(&self, lease: &SessionLease) -> Result<(), CommandError> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| CommandError::Internal("lock unavailable".into()))?;
        if guard.lease.as_ref() != Some(lease) {
            return Err(CommandError::Authorization(
                "session changed while locked".into(),
            ));
        }
        guard.authorization_revision = guard.authorization_revision.wrapping_add(1);
        guard.lease = None;
        guard.last_activity = Instant::now();
        Ok(())
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.0.lock() {
            guard.authorization_revision = guard.authorization_revision.wrapping_add(1);
            guard.lease = None;
            guard.call_active = false;
            guard.last_activity = Instant::now();
        }
    }

    fn activity(&self, generation: u64, call_active: Option<bool>) {
        if let Ok(mut inner) = self.0.lock() {
            if inner.generation != generation {
                inner.generation = generation;
                inner.call_active = false;
            }
            inner.last_activity = Instant::now();
            if let Some(active) = call_active {
                inner.call_active = active;
            }
        }
    }

    pub fn tick<R: tauri::Runtime>(&self, handle: &tauri::AppHandle<R>) {
        let app = handle.state::<AppState>();
        let settings = handle.state::<crate::settings::SettingsState>().get();
        let Ok(lease) = app.session().capture() else {
            return;
        };
        let hidden = handle
            .get_webview_window("main")
            .is_some_and(|window| !window.is_visible().unwrap_or(true));
        let should_lock = {
            let Ok(mut inner) = self.0.lock() else {
                return;
            };
            if inner.generation != lease.generation() {
                inner.generation = lease.generation();
                inner.last_activity = Instant::now();
                inner.call_active = false;
                return;
            }
            due(&inner, &settings, hidden)
        };
        if !should_lock {
            return;
        }
        let locked = app
            .session()
            .matching(&lease, |_| {
                let Ok(mut inner) = self.0.lock() else {
                    return false;
                };
                let still_due =
                    inner.generation == lease.generation() && due(&inner, &settings, hidden);
                if still_due {
                    inner.authorization_revision = inner.authorization_revision.wrapping_add(1);
                    inner.lease = Some(lease.clone());
                }
                still_due
            })
            .unwrap_or(false);
        if locked {
            *app.drafts.lock().unwrap() = None;
            let _ = handle.emit("app-locked", ());
        }
    }
}

#[tauri::command]
pub fn note_activity(app: tauri::State<'_, AppState>, state: tauri::State<'_, AppLock>) {
    state.activity(app.session().generation(), None);
}

#[tauri::command]
pub fn set_call_active(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, AppLock>,
    active: bool,
) {
    state.activity(app.session().generation(), Some(active));
}

/// Only lock recovery and sign-out can run during a lock. Plugin IPC has its own capability gate.
pub fn allowed_while_locked(command: &str) -> bool {
    matches!(
        command,
        "lock_status"
            | "unlock_app"
            | "logout"
            | "delete_temp_file"
            | "initial_presentation_ready"
            | "suspend_screenshot_shortcuts"
    )
}

#[tauri::command]
pub fn lock_app(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, AppLock>,
) -> Result<(), CommandError> {
    let lease = app
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    app.session()
        .matching(&lease, |_| state.lock(lease.clone()))
        .map_err(CommandError::Authorization)??;
    *app.drafts.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
pub fn lock_status(
    state: tauri::State<'_, AppLock>,
    app: tauri::State<'_, AppState>,
) -> Option<crate::commands::UserInfo> {
    if state.is_locked() {
        app.session()
            .get()
            .map(|info| crate::commands::UserInfo::from(info.user))
    } else {
        None
    }
}

#[tauri::command]
pub async fn unlock_app(
    password: Option<String>,
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, AppLock>,
) -> Result<(), CommandError> {
    let lease = app
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if !state.is_locked() {
        return Err(CommandError::Authorization("app is not locked".into()));
    }
    verify_session_identity(&app, &lease, password, "unlock Mesh-Talk").await?;
    app.session()
        .matching(&lease, |_| state.unlock(&lease))
        .map_err(CommandError::Authorization)?
}

/// Reuse app-lock authentication for one operation without changing lock state.
pub(crate) async fn verify_session_identity(
    app: &AppState,
    lease: &SessionLease,
    password: Option<String>,
    prompt_reason: &'static str,
) -> Result<(), CommandError> {
    // Resolve identity under the captured lease before starting an OS/password prompt.
    let username = app
        .session()
        .matching(lease, |info| info.user.login_name().to_owned())
        .map_err(CommandError::Authorization)?;
    #[cfg(target_os = "macos")]
    {
        let _ = (password, username);
        authenticate_device_owner(prompt_reason).await?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt_reason;
        let supplied = password
            .ok_or_else(|| CommandError::Authentication("Enter password to unlock".into()))?;
        let auth = app.auth_service().clone();
        let verified =
            tokio::task::spawn_blocking(move || auth.verify_credentials(&username, &supplied))
                .await
                .map_err(|e| CommandError::Internal(format!("unlock verification: {e}")))?
                .map_err(|_| CommandError::Authentication("Invalid password".into()))?;
        if verified != lease.owner() {
            return Err(CommandError::Authorization(
                "account changed while locked".into(),
            ));
        }
    }
    app.session()
        .matching(lease, |_| ())
        .map_err(CommandError::Authorization)
}

#[cfg(target_os = "macos")]
async fn authenticate_device_owner(prompt_reason: &'static str) -> Result<(), CommandError> {
    tokio::task::spawn_blocking(move || {
        use block2::RcBlock;
        use objc2::runtime::Bool;
        use objc2_foundation::{NSError, NSString};
        use objc2_local_authentication::{LAContext, LAPolicy};

        let context = unsafe { LAContext::new() };
        let policy = LAPolicy::DeviceOwnerAuthentication;
        unsafe { context.canEvaluatePolicy_error(policy) }.map_err(|error| {
            CommandError::Authentication(format!("Mac authentication unavailable: {error}"))
        })?;

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let reply = RcBlock::new(move |success: Bool, _error: *mut NSError| {
            let _ = sender.send(success.as_bool());
        });
        let reason = NSString::from_str(prompt_reason);
        // Keep the context alive until the system finishes its Touch ID or Mac
        // account-password prompt. The reply runs on a private OS queue.
        unsafe { context.evaluatePolicy_localizedReason_reply(policy, &reason, &reply) };
        match receiver.recv_timeout(Duration::from_secs(120)) {
            Ok(true) => Ok(()),
            Ok(false) => Err(CommandError::Authentication(
                "Mac authentication canceled or failed".into(),
            )),
            Err(_) => {
                unsafe { context.invalidate() };
                Err(CommandError::Authentication(
                    "Mac authentication did not complete".into(),
                ))
            }
        }
    })
    .await
    .map_err(|error| CommandError::Internal(format!("Mac authentication task: {error}")))?
}

pub fn app_locked<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) -> bool {
    handle
        .try_state::<AppLock>()
        .is_some_and(|lock| lock.is_locked())
}

pub fn reject_if_locked<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
) -> Result<(), CommandError> {
    if app_locked(handle) {
        Err(CommandError::Authorization("app locked".into()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reveal_revision_rejects_a_lock_then_unlock_transition() {
        let lock = AppLock::default();
        let revision = lock.authorization_revision().unwrap();
        let session = crate::state::SessionState::default();
        let user = crate::services::user::User {
            user_id: "owner".into(),
            name: "owner".into(),
            account_login_name: None,
            display_name: "Owner".into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        session.set("token".into(), user, "pw".into());
        let lease = session.capture().unwrap();
        lock.lock(lease.clone()).unwrap();
        lock.unlock(&lease).unwrap();
        assert!(!lock.is_locked());
        assert!(
            lock.with_unchanged_revision(revision, || Ok("secret"))
                .is_err()
        );
    }

    #[test]
    fn locked_commands_allow_only_unlock_and_preferences() {
        assert!(allowed_while_locked("unlock_app"));
        assert!(allowed_while_locked("initial_presentation_ready"));
        assert!(!allowed_while_locked("account_history"));
        assert!(!allowed_while_locked("read_media"));
        assert!(!allowed_while_locked("owner_enqueue_text"));
    }

    #[test]
    fn native_idle_policy_suspends_during_calls_and_locks_when_hidden() {
        let strict = crate::settings::AppSettings::default()
            .with_privacy_level(crate::settings::PrivacyLevel::Strict);
        let mut inner = LockInner::default();
        assert!(!due(&inner, &strict, false));
        assert!(due(&inner, &strict, true));
        inner.call_active = true;
        inner.last_activity = Instant::now() - Duration::from_secs(120);
        assert!(!due(&inner, &strict, true));
        inner.call_active = false;
        assert!(due(&inner, &strict, false));
    }

    #[test]
    fn post_work_response_check_rejects_content_after_lock() {
        let state = AppLock::default();
        let app = tauri::test::mock_builder()
            .manage(state.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        assert!(reject_if_locked(app.handle()).is_ok());
        let session = crate::state::SessionState::default();
        let user = crate::services::user::User {
            user_id: "owner".into(),
            name: "owner".into(),
            account_login_name: None,
            display_name: "Owner".into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        session.set("token".into(), user, "pw".into());
        state.lock(session.capture().unwrap()).unwrap();
        assert!(reject_if_locked(app.handle()).is_err());
    }
}
