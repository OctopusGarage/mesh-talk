use crate::services::auth_service::AuthService;
use crate::services::user::User;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(test)]
type FileProgressHook = Arc<Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>>;
#[cfg(test)]
type HostOperationGate = Arc<
    Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
>;

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub token: String,
    pub user: User,
    /// The user's password, retained in memory for the lifetime of the session.
    ///
    /// The node opens its per-account encrypted stores (keystore, event log,
    /// ratchet sessions, …) with a key derived from this password, so it must be
    /// available without re-prompting while the session is live. It is never serialized
    /// to disk — it lives only in the in-memory session.
    pub password: String,
}

#[derive(Clone, Default)]
pub struct SessionState {
    inner: Arc<Mutex<Option<SessionInfo>>>,
    generation: Arc<AtomicU64>,
}

/// Private host-session authority; never serialized or exposed to the frontend.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SessionLease {
    owner: String,
    generation: u64,
}

impl SessionLease {
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }
}

impl SessionState {
    pub(crate) fn unchanged<T>(
        &self,
        generation: u64,
        operation: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let _guard = self.inner.lock().map_err(|_| "Session lock unavailable")?;
        if self.generation() != generation {
            return Err("session replaced".into());
        }
        Ok(operation())
    }
    pub(crate) fn capture(&self) -> Result<SessionLease, String> {
        let guard = self.inner.lock().map_err(|_| "Session lock unavailable")?;
        let info = guard.as_ref().ok_or("not logged in")?;
        Ok(SessionLease {
            owner: info.user.user_id.clone(),
            generation: self.generation(),
        })
    }

    /// The callback is synchronous: no session guard can cross an await.
    pub(crate) fn matching<T>(
        &self,
        lease: &SessionLease,
        operation: impl FnOnce(&mut SessionInfo) -> T,
    ) -> Result<T, String> {
        let mut guard = self.inner.lock().map_err(|_| "Session lock unavailable")?;
        let info = guard.as_mut().ok_or("session expired")?;
        if info.user.user_id != lease.owner || self.generation() != lease.generation {
            return Err("session replaced".into());
        }
        Ok(operation(info))
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    pub fn set(&self, token: String, user: User, password: String) {
        self.publish(token, user, password);
    }

    pub(crate) fn publish(&self, token: String, user: User, password: String) -> SessionLease {
        let mut guard = self.inner.lock().unwrap();
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let lease = SessionLease {
            owner: user.user_id.clone(),
            generation,
        };
        *guard = Some(SessionInfo {
            token,
            user,
            password,
        });
        lease
    }

    pub fn clear(&self) {
        let mut guard = self.inner.lock().unwrap();
        self.generation.fetch_add(1, Ordering::AcqRel);
        *guard = None;
    }

    /// Update the live session's display name in place (after a rename). No-op if there
    /// is no active session.
    pub fn set_display_name(&self, display_name: String) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(info) = guard.as_mut() {
            info.user.display_name = display_name;
        }
    }

    pub fn get(&self) -> Option<SessionInfo> {
        self.inner.lock().unwrap().clone()
    }

    /// Run an owner-authorized operation while retaining the live session guard.
    /// Callers see only the local user id; password and session token stay private.
    pub(crate) fn with_owner<T>(
        &self,
        operation: impl FnOnce(Option<&str>) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self.inner.lock().map_err(|_| "Session lock unavailable")?;
        operation(guard.as_ref().map(|session| session.user.user_id.as_str()))
    }
}

#[cfg(test)]
mod session_generation_tests {
    use super::*;
    #[test]
    fn delayed_rename_result_cannot_mutate_replacement_owner() {
        let session = SessionState::default();
        let user = User {
            user_id: "alice".into(),
            name: "alice".into(),
            display_name: "Alice".into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        session.set("a".into(), user.clone(), "a-password".into());
        let admitted_generation = session.generation();
        let lease = session.capture().unwrap();
        let mut replacement = user;
        replacement.user_id = "bob".into();
        replacement.display_name = "Bob".into();
        session.set("b".into(), replacement, "b-password".into());
        // Models the existing command's delayed persistence result after replacement.
        assert!(session
            .matching(&lease, |info| info.user.display_name =
                "Alice renamed".into())
            .is_err());
        assert_ne!(session.generation(), admitted_generation);
        assert_eq!(session.get().unwrap().user.display_name, "Bob");
    }
    #[test]
    fn owner_replacement_and_logout_invalidate_queued_operations_but_rename_does_not() {
        let session = SessionState::default();
        let initial = session.generation();
        let user = User {
            user_id: "alice".into(),
            name: "alice".into(),
            display_name: "Alice".into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        session.set("token".into(), user.clone(), "pw".into());
        let signed_in = session.generation();
        assert_ne!(initial, signed_in);
        session.set_display_name("Renamed".into());
        assert_eq!(signed_in, session.generation());
        session.clear();
        assert_ne!(signed_in, session.generation());
        let signed_out = session.generation();
        session.set("new token".into(), user, "pw".into());
        assert_ne!(signed_out, session.generation());
    }
}

/// Managed Tauri state: the auth service + the current in-memory session. (The legacy
/// contact/message/network services were retired with the legacy stack; the
/// node runs out of [`crate::chat_commands::NodeState`].)
#[derive(Clone)]
pub struct AppState {
    #[cfg(test)]
    pub(crate) rename_hot_gate: HostOperationGate,
    #[cfg(test)]
    pub(crate) logout_retirement_gate: HostOperationGate,
    #[cfg(test)]
    pub(crate) auto_admission_gate: HostOperationGate,
    #[cfg(test)]
    pub(crate) owner_command_captured: Arc<Mutex<Option<std::sync::mpsc::Sender<&'static str>>>>,
    #[cfg(test)]
    pub(crate) owner_file_progress_hook: FileProgressHook,
    auth_service: AuthService,
    session_state: SessionState,
    pub(crate) auth_operation: Arc<tokio::sync::Mutex<()>>,
}

impl AppState {
    pub fn new(auth_service: AuthService) -> Self {
        Self {
            #[cfg(test)]
            rename_hot_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            logout_retirement_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            auto_admission_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            owner_command_captured: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            owner_file_progress_hook: Arc::new(Mutex::new(None)),
            auth_service,
            session_state: SessionState::default(),
            auth_operation: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn auth_service(&self) -> &AuthService {
        &self.auth_service
    }

    pub fn session(&self) -> &SessionState {
        &self.session_state
    }
}
