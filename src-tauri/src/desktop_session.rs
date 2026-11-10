//! Single live desktop session: identity, credential token, retained password and
//! exact-generation admission move together under one lock. Credential/storage
//! verification belongs to AuthService; runtime retirement belongs to the host.

use crate::services::user::User;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Authentication adapts these transition failures to its legacy public errors.
pub(crate) enum SessionTransitionError {
    AlreadyLoggedIn,
    NotLoggedIn,
    InvalidToken,
}

#[derive(Clone)]
pub struct SessionInfo {
    pub token: String,
    pub user: User,
    /// Retained identity-store secret. It may differ from the shared login password
    /// and raw node-storage secret for an upgraded installation.
    pub password: String,
    /// Retained raw secret for existing node stores.
    pub node_storage_password: String,
    /// Shared account login secret used by saved login and private linking.
    pub account_login_password: String,
}

impl std::fmt::Debug for SessionInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInfo")
            .field("token", &"[REDACTED]")
            .field("user", &self.user)
            .field("password", &"[REDACTED]")
            .field("node_storage_password", &"[REDACTED]")
            .field("account_login_password", &"[REDACTED]")
            .finish()
    }
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

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

impl SessionState {
    /// Retain exact signed-out authority through a synchronous publication.
    pub(crate) fn without_session<T>(
        &self,
        generation: u64,
        operation: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let guard = self.inner.lock().map_err(|_| "Session lock unavailable")?;
        if guard.is_some() || self.generation() != generation {
            return Err("session replaced".into());
        }
        Ok(operation())
    }

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
        self.replace(&mut guard, token, user, password)
    }

    fn replace(
        &self,
        guard: &mut Option<SessionInfo>,
        token: String,
        user: User,
        password: String,
    ) -> SessionLease {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let lease = SessionLease {
            owner: user.user_id.clone(),
            generation,
        };
        *guard = Some(SessionInfo {
            token,
            user,
            node_storage_password: password.clone(),
            account_login_password: password.clone(),
            password,
        });
        lease
    }

    /// Install an authenticated user atomically with its host admission generation.
    /// Reauthentication replaces the token, even when the owner is unchanged.
    pub(crate) fn activate_with_credentials(
        &self,
        token: String,
        user: User,
        identity_password: String,
        node_storage_password: String,
        account_login_password: String,
    ) -> Result<SessionLease, SessionTransitionError> {
        let mut guard = self.inner.lock().unwrap();
        if guard
            .as_ref()
            .is_some_and(|info| info.user.user_id != user.user_id)
        {
            return Err(SessionTransitionError::AlreadyLoggedIn);
        }
        let lease = self.replace(&mut guard, token, user, identity_password);
        let info = guard.as_mut().expect("session was just activated");
        info.node_storage_password = node_storage_password;
        info.account_login_password = account_login_password;
        Ok(lease)
    }

    pub(crate) fn logout(&self, token: &str) -> Result<(), SessionTransitionError> {
        let mut guard = self.inner.lock().unwrap();
        let info = guard.as_ref().ok_or(SessionTransitionError::NotLoggedIn)?;
        if info.token != token {
            return Err(SessionTransitionError::InvalidToken);
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        *guard = None;
        Ok(())
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
            account_login_name: None,
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
        assert!(
            session
                .matching(&lease, |info| info.user.display_name =
                    "Alice renamed".into())
                .is_err()
        );
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
            account_login_name: None,
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
