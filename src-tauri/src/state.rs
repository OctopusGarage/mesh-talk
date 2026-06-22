use crate::services::auth_service::AuthService;
use crate::services::user::User;
use std::sync::{Arc, Mutex};

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
}

impl SessionState {
    pub fn set(&self, token: String, user: User, password: String) {
        let mut guard = self.inner.lock().unwrap();
        *guard = Some(SessionInfo {
            token,
            user,
            password,
        });
    }

    pub fn clear(&self) {
        let mut guard = self.inner.lock().unwrap();
        *guard = None;
    }

    pub fn get(&self) -> Option<SessionInfo> {
        self.inner.lock().unwrap().clone()
    }
}

/// Managed Tauri state: the auth service + the current in-memory session. (The legacy
/// contact/message/network services were retired with the legacy stack; the
/// node runs out of [`crate::chat_commands::NodeState`].)
#[derive(Clone)]
pub struct AppState {
    auth_service: AuthService,
    session_state: SessionState,
}

impl AppState {
    pub fn new(auth_service: AuthService) -> Self {
        Self {
            auth_service,
            session_state: SessionState::default(),
        }
    }

    pub fn auth_service(&self) -> &AuthService {
        &self.auth_service
    }

    pub fn session(&self) -> &SessionState {
        &self.session_state
    }
}
