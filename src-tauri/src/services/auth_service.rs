use crate::services::user::User;
use mesh_talk_core::identity::manager::IdentityManager;
use mesh_talk_core::storage::file_manager::FileManager;

use rand::distr::{Alphanumeric, Distribution};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Issued token duration in seconds (24 hours).
const SESSION_DURATION_SECS: u64 = 24 * 60 * 60;

#[cfg(test)]
type RenamePersistenceGate =
    Arc<Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>>;

/// Authentication error types
#[derive(Debug, Clone, PartialEq)]
pub enum AuthError {
    /// User already exists with the given name
    UserAlreadyExists,
    /// Provided password does not meet security requirements
    PasswordTooWeak,
    /// Provided registration or profile data is invalid
    InvalidInput(String),
    /// Invalid credentials provided
    InvalidCredentials,
    /// User not found
    UserNotFound,
    /// User is already logged in
    AlreadyLoggedIn,
    /// User is not logged in
    NotLoggedIn,
    /// Storage error
    StorageError(String),
    /// Internal error
    InternalError(String),
}

/// Authentication result type
pub type AuthResult<T> = Result<T, AuthError>;

/// Session information
#[derive(Debug, Clone)]
pub struct Session {
    /// User ID associated with this session
    pub user_id: String,
    /// Session token
    pub token: String,
    /// Timestamp when the session was created
    pub created_at: u64,
    /// Timestamp when the session expires
    pub expires_at: u64,
}

impl Session {
    pub fn new(user_id: String, duration_secs: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Generate a cryptographically secure random token
        let token: String = Alphanumeric
            .sample_iter(rand::rng())
            .take(32)
            .map(char::from)
            .collect();

        Self {
            user_id,
            token,
            created_at: now,
            expires_at: now + duration_secs,
        }
    }

    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now > self.expires_at
    }
}

/// Authentication service for managing user registration, login, and logout
#[derive(Clone)]
pub struct AuthService {
    #[cfg(test)]
    rename_persisted: RenamePersistenceGate,
    /// Identity manager for user authentication
    identity_manager: Arc<IdentityManager>,
    /// Sole live desktop-session owner, shared with host admission.
    session: crate::desktop_session::SessionState,
}

impl From<crate::desktop_session::SessionTransitionError> for AuthError {
    fn from(error: crate::desktop_session::SessionTransitionError) -> Self {
        use crate::desktop_session::SessionTransitionError;
        match error {
            SessionTransitionError::AlreadyLoggedIn => Self::AlreadyLoggedIn,
            SessionTransitionError::NotLoggedIn => Self::NotLoggedIn,
            SessionTransitionError::InvalidToken => Self::InvalidCredentials,
        }
    }
}

static INSTANCE: std::sync::OnceLock<AuthService> = std::sync::OnceLock::new();

impl AuthService {
    /// Check a lock-screen credential without creating a new session or runtime.
    pub fn verify_credentials(&self, name: &str, password: &str) -> AuthResult<String> {
        self.identity_manager
            .authenticate_account_login(name, password)
            .map(|identity| identity.user.user_id.clone())
            .map_err(|_| AuthError::InvalidCredentials)
    }
    /// Create a new authentication service
    pub fn new(identity_manager: Arc<IdentityManager>) -> Self {
        Self {
            #[cfg(test)]
            rename_persisted: Arc::new(Mutex::new(None)),
            identity_manager,
            session: crate::desktop_session::SessionState::default(),
        }
    }

    pub(crate) fn identity_manager(&self) -> &IdentityManager {
        &self.identity_manager
    }

    pub(crate) fn session(&self) -> &crate::desktop_session::SessionState {
        &self.session
    }

    /// Get the global authentication service instance
    pub fn global() -> &'static AuthService {
        INSTANCE.get().expect("AuthService not initialized")
    }

    /// Initialize the global authentication service instance
    pub fn init_global(file_manager: FileManager) {
        let identity_manager = Arc::new(IdentityManager::new(file_manager));
        INSTANCE.set(AuthService::new(identity_manager)).ok();
    }

    /// Register a new user
    pub fn register(&self, name: String, password: String, address: String) -> AuthResult<User> {
        let normalized_name = name.trim();
        if normalized_name.is_empty() {
            return Err(AuthError::InvalidInput(
                "Username cannot be empty".to_string(),
            ));
        }

        let normalized_address = address.trim();
        if normalized_address.is_empty() {
            return Err(AuthError::InvalidInput(
                "Address cannot be empty".to_string(),
            ));
        }

        let password = password.trim().to_string();
        if password.len() < 8 {
            return Err(AuthError::PasswordTooWeak);
        }

        // Check if user already exists using IdentityManager
        match self
            .identity_manager
            .authenticate_user(normalized_name, &password)
        {
            Ok(_) => return Err(AuthError::UserAlreadyExists),
            Err(_) => {} // User doesn't exist, which is what we want
        }

        // Register the user with IdentityManager
        let identity_user = self
            .identity_manager
            .register_user(normalized_name, &password)
            .map_err(|e| match e {
                // A user can already exist under a different password, in which
                // case the authenticate_user pre-check above does not catch it.
                // register_user is the authoritative existence check.
                mesh_talk_core::identity::errors::IdentityError::UserAlreadyExists(_) => {
                    AuthError::UserAlreadyExists
                }
                mesh_talk_core::identity::errors::IdentityError::InvalidUsername => {
                    AuthError::InvalidInput("Invalid username".to_string())
                }
                other => AuthError::StorageError(format!("Failed to register user: {}", other)),
            })?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let user = User {
            user_id: identity_user.user_id.clone(),
            display_name: identity_user.effective_display_name().to_string(),
            name: identity_user.username,
            account_login_name: None,
            address: normalized_address.to_string(),
            created_at: identity_user.created_at,
            last_seen: now,
            is_online: false,
        };

        Ok(user)
    }

    /// Login a user
    pub fn login(&self, name: String, password: String) -> AuthResult<(User, String)> {
        self.login_session(name, password)
            .map(|(user, token, _)| (user, token))
    }

    pub(crate) fn login_session(
        &self,
        name: String,
        password: String,
    ) -> AuthResult<(User, String, crate::desktop_session::SessionLease)> {
        let normalized_name = name.trim();
        if normalized_name.is_empty() {
            return Err(AuthError::InvalidInput(
                "Username cannot be empty".to_string(),
            ));
        }

        // Preserve the raw login secret: legacy identity and node stores may
        // intentionally use trimmed and raw forms respectively.
        let resolved = self
            .identity_manager
            .authenticate_account_login(normalized_name, &password)
            .map_err(|e| match e {
                mesh_talk_core::identity::errors::IdentityError::UserNotFound(_) => {
                    AuthError::UserNotFound
                }
                mesh_talk_core::identity::errors::IdentityError::InvalidPassword => {
                    AuthError::InvalidCredentials
                }
                _ => AuthError::StorageError(format!("Failed to authenticate user: {}", e)),
            })?;

        let identity_user = &resolved.user;
        let mut user = User {
            user_id: identity_user.user_id.clone(),
            display_name: identity_user.effective_display_name().to_string(),
            name: identity_user.username.clone(),
            account_login_name: Some(resolved.login_name.clone()),
            address: identity_user.public_key.clone(), // Using public key as address for now
            created_at: identity_user.created_at,
            last_seen: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            is_online: true,
        };

        user.update_last_seen();
        // Reauthentication revokes the previous token. The desktop has one live
        // session; the former registry never expired active admission or logout.
        let token = Session::new(user.user_id.clone(), SESSION_DURATION_SECS).token;
        let lease = self.session.activate_with_credentials(
            token.clone(),
            user.clone(),
            resolved.identity_password.clone(),
            resolved.node_storage_password.clone(),
            password,
        )?;
        Ok((user, token, lease))
    }

    /// Change the current user's editable display name (nickname). Persists it to the
    /// identity store (verifying `password`) and updates the in-memory current user.
    /// The login `username` is unchanged. Returns the updated session user.
    pub fn set_display_name(&self, password: &str, new_display_name: &str) -> AuthResult<User> {
        let lease = self.session.capture().map_err(|_| AuthError::NotLoggedIn)?;
        self.rename_session(&lease, password, new_display_name)
    }

    pub(crate) fn rename_session(
        &self,
        lease: &crate::desktop_session::SessionLease,
        password: &str,
        new_display_name: &str,
    ) -> AuthResult<User> {
        let username = self
            .session
            .matching(lease, |info| info.user.name.clone())
            .map_err(|_| AuthError::NotLoggedIn)?;

        let identity_user = self
            .identity_manager
            .set_display_name(&username, password, new_display_name)
            .map_err(|e| match e {
                mesh_talk_core::identity::errors::IdentityError::InvalidDisplayName => {
                    AuthError::InvalidInput("Invalid display name".to_string())
                }
                mesh_talk_core::identity::errors::IdentityError::InvalidPassword => {
                    AuthError::InvalidCredentials
                }
                mesh_talk_core::identity::errors::IdentityError::UserNotFound(_) => {
                    AuthError::UserNotFound
                }
                other => AuthError::StorageError(format!("Failed to set display name: {}", other)),
            })?;

        #[cfg(test)]
        if let Some((entered, release)) = self.rename_persisted.lock().unwrap().take() {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        self.session
            .matching(lease, |info| {
                info.user.display_name = identity_user.effective_display_name().to_string();
                info.user.clone()
            })
            .map_err(|_| AuthError::NotLoggedIn)
    }

    /// Logout the current user and invalidate its host admission generation.
    pub fn logout(&self, token: String) -> AuthResult<()> {
        self.session.logout(&token).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::storage::file_manager::FileManager;
    use std::sync::Arc;

    #[test]
    fn linked_account_login_uses_shared_credentials_and_preserves_local_storage_owner() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(IdentityManager::new(FileManager::new(
            dir.path().to_owned(),
        )));
        let auth = AuthService::new(manager.clone());
        let local = auth
            .register(
                "Messi10".into(),
                "old-local-password".into(),
                "fixture".into(),
            )
            .unwrap();
        manager
            .set_display_name("Messi10", "old-local-password", "Shared nickname")
            .unwrap();
        manager
            .install_account_login(
                "Messi10",
                "old-local-password",
                " old-local-password ",
                &"a".repeat(32),
                "Federer",
                "source-account-password",
            )
            .unwrap();
        let result = auth.login("Federer".into(), "source-account-password".into());
        assert!(
            result.is_ok(),
            "the account login must resolve the retained local identity"
        );
        let (user, _) = result.unwrap();
        assert_eq!(user.user_id, local.user_id);
        assert_eq!(user.name, "Messi10", "the private storage key is preserved");
        let session = auth.session.get().unwrap();
        assert_eq!(session.password, "old-local-password");
        assert_eq!(session.node_storage_password, " old-local-password ");
        assert_eq!(session.account_login_password, "source-account-password");
        let debug = format!("{session:?}");
        assert!(!debug.contains("old-local-password"));
        assert!(!debug.contains("source-account-password"));
        assert!(!debug.contains(&session.token));
        let info = crate::commands::UserInfo::from(user);
        assert_eq!(info.username, "Federer");
        assert_eq!(info.display_name, "Shared nickname");
        assert_eq!(
            auth.verify_credentials("Federer", "source-account-password")
                .unwrap(),
            local.user_id
        );
        assert!(
            auth.verify_credentials("Messi10", "old-local-password")
                .is_err()
        );
    }

    #[test]
    fn legacy_login_preserves_raw_node_and_saved_login_secrets() {
        let (auth, _) = setup_test_context();
        auth.register("legacy".into(), "password-legacy".into(), "fixture".into())
            .unwrap();
        auth.login("legacy".into(), " password-legacy ".into())
            .unwrap();
        let session = auth.session.get().unwrap();
        assert_eq!(session.password, "password-legacy");
        assert_eq!(session.node_storage_password, " password-legacy ");
        assert_eq!(session.account_login_password, " password-legacy ");
    }

    #[test]
    fn delayed_persisted_rename_does_not_retarget_current_service_user() {
        let dir = tempfile::tempdir().unwrap();
        let auth = AuthService::new(Arc::new(IdentityManager::new(FileManager::new(
            dir.path().to_owned(),
        ))));
        auth.register("alice".into(), "password-a".into(), "fixture".into())
            .unwrap();
        auth.register("bob".into(), "password-b".into(), "fixture".into())
            .unwrap();
        let (_, token) = auth.login("alice".into(), "password-a".into()).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *auth.rename_persisted.lock().unwrap() = Some((entered_tx, release_rx));
        let rename_auth = auth.clone();
        let rename =
            std::thread::spawn(move || rename_auth.set_display_name("password-a", "Alice renamed"));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        auth.logout(token).unwrap();
        auth.login("bob".into(), "password-b".into()).unwrap();
        release_tx.send(()).unwrap();
        assert!(
            rename.join().unwrap().is_err(),
            "completed A persistence must reject a replaced current user"
        );
        assert_eq!(auth.session.get().unwrap().user.display_name, "bob");
    }

    #[test]
    fn active_session_is_shared_by_authentication_and_host_admission() {
        let (auth, _) = setup_test_context();
        auth.register("alice".into(), "password-a".into(), "fixture".into())
            .unwrap();
        let app = crate::state::AppState::new(auth.clone());
        let (user, token) = auth.login("alice".into(), " password-a ".into()).unwrap();
        let info = app
            .session()
            .get()
            .expect("authentication publishes host session");
        assert_eq!(info.user.user_id, user.user_id);
        assert_eq!(info.token, token);
        assert_eq!(info.password, "password-a");
        let lease = app.session().capture().unwrap();
        auth.register("bob".into(), "password-b".into(), "fixture".into())
            .unwrap();
        assert!(matches!(
            auth.login("bob".into(), "password-b".into()),
            Err(AuthError::AlreadyLoggedIn)
        ));
        assert!(matches!(
            auth.login("alice".into(), "wrong-password".into()),
            Err(AuthError::InvalidCredentials)
        ));
        assert!(app.session().matching(&lease, |_| ()).is_ok());

        auth.set_display_name("password-a", "Alice renamed")
            .unwrap();
        assert_eq!(
            app.session().get().unwrap().user.display_name,
            "Alice renamed"
        );
        assert!(app.session().matching(&lease, |_| ()).is_ok());
        let (_, replacement) = auth.login("alice".into(), "password-a".into()).unwrap();
        assert!(app.session().matching(&lease, |_| ()).is_err());
        assert!(matches!(
            auth.logout(token),
            Err(AuthError::InvalidCredentials)
        ));
        assert!(app.session().get().is_some());
        auth.logout(replacement).unwrap();
        assert!(app.session().get().is_none());
    }

    #[test]
    fn delayed_rename_rejects_same_owner_reauthentication() {
        let dir = tempfile::tempdir().unwrap();
        let auth = AuthService::new(Arc::new(IdentityManager::new(FileManager::new(
            dir.path().to_owned(),
        ))));
        auth.register("alice".into(), "password-a".into(), "fixture".into())
            .unwrap();
        auth.login("alice".into(), "password-a".into()).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *auth.rename_persisted.lock().unwrap() = Some((entered_tx, release_rx));
        let worker = auth.clone();
        let rename = std::thread::spawn(move || worker.set_display_name("password-a", "Renamed"));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        auth.login("alice".into(), "password-a".into()).unwrap();
        release_tx.send(()).unwrap();
        assert!(matches!(
            rename.join().unwrap(),
            Err(AuthError::NotLoggedIn)
        ));
    }

    fn setup_test_context() -> (
        AuthService,
        Arc<mesh_talk_core::identity::manager::IdentityManager>,
    ) {
        // Create a temporary directory for test data
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory");
        let data_path = temp_dir.path().to_path_buf();

        let file_manager = FileManager::new(data_path);
        let identity_manager = Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
            file_manager,
        ));
        (
            AuthService::new(Arc::clone(&identity_manager)),
            identity_manager,
        )
    }

    #[test]
    fn test_user_registration() {
        let (auth_service, _) = setup_test_context();
        let result = auth_service.register(
            "testuser".to_string(),
            "supersafepass".to_string(),
            "192.168.1.1:7000".to_string(),
        );
        assert!(result.is_ok());

        let user = result.unwrap();
        assert_eq!(user.name, "testuser");
        // Note: Address is now derived from public key, so this assertion may need to change
        // assert_eq!(user.address, "192.168.1.1:7000");
    }

    #[test]
    fn test_duplicate_user_registration() {
        let (auth_service, _) = setup_test_context();
        let _ = auth_service.register(
            "testuser".to_string(),
            "supersafepass".to_string(),
            "192.168.1.1:7000".to_string(),
        );
        let result = auth_service.register(
            "testuser".to_string(),
            "anothersafepass".to_string(),
            "192.168.1.2:7000".to_string(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), AuthError::UserAlreadyExists);
    }

    #[test]
    fn test_user_registration_rejects_weak_password() {
        let (auth_service, _) = setup_test_context();
        let result = auth_service.register(
            "testuser".to_string(),
            "short".to_string(),
            "192.168.1.1:7000".to_string(),
        );
        assert!(matches!(result, Err(AuthError::PasswordTooWeak)));
    }

    #[test]
    fn test_user_registration_rejects_empty_username() {
        let (auth_service, _) = setup_test_context();
        let result = auth_service.register(
            "   ".to_string(),
            "supersafepass".to_string(),
            "192.168.1.1:7000".to_string(),
        );
        assert!(matches!(
            result,
            Err(AuthError::InvalidInput(ref msg)) if msg.contains("Username")
        ));
    }

    #[test]
    fn test_user_registration_rejects_empty_address() {
        let (auth_service, _) = setup_test_context();
        let result = auth_service.register(
            "testuser".to_string(),
            "supersafepass".to_string(),
            "   ".to_string(),
        );
        assert!(matches!(
            result,
            Err(AuthError::InvalidInput(ref msg)) if msg.contains("Address")
        ));
    }

    #[test]
    fn test_user_login() {
        let (auth_service, _) = setup_test_context();
        let _ = auth_service.register(
            "testuser".to_string(),
            "supersafepass".to_string(),
            "192.168.1.1:7000".to_string(),
        );
        let result = auth_service.login("testuser".to_string(), "supersafepass".to_string());
        assert!(result.is_ok());

        let (user, token) = result.unwrap();
        assert_eq!(user.name, "testuser");
        assert!(!token.is_empty());
    }

    #[test]
    fn test_user_login_not_found() {
        let (auth_service, _) = setup_test_context();
        let result = auth_service.login("nonexistent".to_string(), "password".to_string());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), AuthError::UserNotFound);
    }

    #[test]
    fn test_user_login_with_invalid_password() {
        let (auth_service, _) = setup_test_context();
        let _ = auth_service.register(
            "testuser".to_string(),
            "supersafepass".to_string(),
            "192.168.1.1:7000".to_string(),
        );
        let result = auth_service.login("testuser".to_string(), "wrongpass".to_string());
        assert!(matches!(result, Err(AuthError::InvalidCredentials)));
    }

    #[test]
    fn test_user_logout() {
        let (auth_service, _) = setup_test_context();
        let _ = auth_service.register(
            "testuser".to_string(),
            "supersafepass".to_string(),
            "192.168.1.1:7000".to_string(),
        );
        let (_, token) = auth_service
            .login("testuser".to_string(), "supersafepass".to_string())
            .unwrap();
        let result = auth_service.logout(token);
        assert!(result.is_ok());
    }

    #[test]
    fn test_user_logout_not_logged_in() {
        let (auth_service, _) = setup_test_context();
        let result = auth_service.logout("nonexistent_token".to_string());
        assert!(matches!(result, Err(AuthError::NotLoggedIn)));
    }

    #[test]
    fn test_set_display_name_updates_current_user_and_keeps_username() {
        let (auth_service, _) = setup_test_context();
        auth_service
            .register(
                "testuser".to_string(),
                "supersafepass".to_string(),
                "127.0.0.1:7000".to_string(),
            )
            .unwrap();
        auth_service
            .login("testuser".to_string(), "supersafepass".to_string())
            .unwrap();

        let updated = auth_service
            .set_display_name("supersafepass", "  Test User 🎉 ")
            .unwrap();
        assert_eq!(updated.display_name, "Test User 🎉"); // trimmed, emoji kept
        assert_eq!(updated.name, "testuser"); // login username unchanged

        // A fresh login reads the persisted display name back.
        let (relogged, _) = auth_service
            .login("testuser".to_string(), "supersafepass".to_string())
            .unwrap();
        assert_eq!(relogged.display_name, "Test User 🎉");
    }

    #[test]
    fn test_set_display_name_requires_a_session() {
        let (auth_service, _) = setup_test_context();
        let result = auth_service.set_display_name("pw", "Whoever");
        assert!(matches!(result, Err(AuthError::NotLoggedIn)));
    }
}
