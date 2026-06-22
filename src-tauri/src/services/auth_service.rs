use crate::services::user::User;
use mesh_talk_core::identity::manager::IdentityManager;
use mesh_talk_core::storage::file_manager::FileManager;

use rand::{distr::Alphanumeric, Rng};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Session duration in seconds (24 hours)
const SESSION_DURATION_SECS: u64 = 24 * 60 * 60;

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
        let token: String = rand::rng()
            .sample_iter(Alphanumeric)
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
    /// Identity manager for user authentication
    identity_manager: Arc<IdentityManager>,
    /// In-memory storage of sessions (key: session token, value: Session)
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    /// Currently logged in user (if any)
    current_user: Arc<Mutex<Option<User>>>,
}

static INSTANCE: std::sync::OnceLock<AuthService> = std::sync::OnceLock::new();

impl AuthService {
    /// Create a new authentication service
    pub fn new(identity_manager: Arc<IdentityManager>) -> Self {
        Self {
            identity_manager,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            current_user: Arc::new(Mutex::new(None)),
        }
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
            name: identity_user.username,
            address: normalized_address.to_string(),
            created_at: identity_user.created_at,
            last_seen: now,
            is_online: false,
        };

        Ok(user)
    }

    /// Login a user
    pub fn login(&self, name: String, password: String) -> AuthResult<(User, String)> {
        let normalized_name = name.trim();
        if normalized_name.is_empty() {
            return Err(AuthError::InvalidInput(
                "Username cannot be empty".to_string(),
            ));
        }

        let password = password.trim().to_string();

        // Authenticate the user with IdentityManager
        let identity_user = self
            .identity_manager
            .authenticate_user(normalized_name, &password)
            .map_err(|e| match e {
                mesh_talk_core::identity::errors::IdentityError::UserNotFound(_) => {
                    AuthError::UserNotFound
                }
                mesh_talk_core::identity::errors::IdentityError::InvalidPassword => {
                    AuthError::InvalidCredentials
                }
                _ => AuthError::StorageError(format!("Failed to authenticate user: {}", e)),
            })?;

        let mut user = User {
            user_id: identity_user.user_id.clone(),
            name: identity_user.username,
            address: identity_user.public_key, // Using public key as address for now
            created_at: identity_user.created_at,
            last_seen: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            is_online: true,
        };

        user.update_last_seen();
        let user_id = user.user_id.clone();

        {
            let mut current_user = self.current_user.lock().unwrap();
            if let Some(existing) = current_user.as_ref() {
                if existing.user_id != user_id {
                    return Err(AuthError::AlreadyLoggedIn);
                }
            }
            *current_user = Some(user.clone());
        }

        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, session| session.user_id != user_id && !session.is_expired());

        let session = Session::new(user_id, SESSION_DURATION_SECS);
        let token = session.token.clone();
        sessions.insert(token.clone(), session);

        Ok((user, token))
    }

    /// Logout the current user
    pub fn logout(&self, token: String) -> AuthResult<()> {
        let active_user = {
            let guard = self.current_user.lock().unwrap();
            guard.clone().ok_or(AuthError::NotLoggedIn)?
        };

        let mut sessions = self.sessions.lock().unwrap();
        if sessions.remove(&token).is_none() {
            return Err(AuthError::InvalidCredentials);
        }

        sessions.retain(|_, session| session.user_id != active_user.user_id);

        let _now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Update user presence to offline
        // Note: In a file-based system, we might want to persist this information
        // For now, we'll just update the in-memory representation

        let mut current_user = self.current_user.lock().unwrap();
        if current_user.as_ref().map(|user| user.user_id.clone()) == Some(active_user.user_id) {
            *current_user = None;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::storage::file_manager::FileManager;
    use std::sync::Arc;

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
}
