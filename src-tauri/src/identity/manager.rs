use crate::identity::auth::AuthManager;
use crate::identity::errors::IdentityError;
use crate::identity::keys::KeyManager;
use crate::identity::user::User;
use crate::storage::file_manager::FileManager;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCredentials {
    pub username: String,
    pub password_hash: String,
    pub private_key: Vec<u8>, // Encrypted private key
}

#[derive(Debug, Clone)]
pub struct IdentityManager {
    file_manager: FileManager,
}

impl IdentityManager {
    pub fn new(file_manager: FileManager) -> Self {
        IdentityManager { file_manager }
    }

    pub fn register_user(&self, username: &str, password: &str) -> Result<User, IdentityError> {
        // Validate username
        if !User::validate_username(username) {
            return Err(IdentityError::InvalidUsername);
        }

        // Check if user already exists
        if self.file_manager.file_exists(username, "meta/user.json") {
            return Err(IdentityError::UserAlreadyExists(username.to_string()));
        }

        // Generate key pair
        let (public_key, signing_key) =
            KeyManager::generate_keypair().map_err(|_| IdentityError::KeyGenerationFailed)?;

        // Hash password
        let password_hash = AuthManager::hash_password(password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Serialize and encrypt private key
        let secret_key_bytes = KeyManager::serialize_secret_key(&signing_key);
        self.file_manager
            .write_encrypted_file(
                username,
                "meta/private_key.enc",
                &secret_key_bytes,
                password,
            )
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Create user object
        let public_key_str = KeyManager::serialize_public_key(&public_key);
        let user = User::new(username.to_string(), public_key_str);

        // Save user metadata
        self.file_manager
            .write_encrypted_file(username, "meta/user.json", &user, password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Save user credentials
        let credentials = UserCredentials {
            username: username.to_string(),
            password_hash,
            private_key: secret_key_bytes,
        };
        self.file_manager
            .write_encrypted_file(username, "meta/credentials.json", &credentials, password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Persist user identifier separately for quick lookup
        self.file_manager
            .write_encrypted_file(username, "meta/user_id.enc", &user.user_id, password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Initialize user data directories
        self.initialize_user_data(username, password)?;

        Ok(user)
    }

    /// Authenticate a user and return their information
    pub fn authenticate_user(&self, username: &str, password: &str) -> Result<User, IdentityError> {
        // Validate username
        if !User::validate_username(username) {
            return Err(IdentityError::InvalidUsername);
        }

        // Check if user exists
        if !self.file_manager.file_exists(username, "meta/user.json") {
            return Err(IdentityError::UserNotFound(username.to_string()));
        }

        // Read credentials
        let credentials: UserCredentials = self
            .file_manager
            .read_encrypted_file(username, "meta/credentials.json", password)
            .map_err(|e| match e {
                crate::storage::errors::StorageError::Decryption(_) => {
                    IdentityError::InvalidPassword
                }
                _ => IdentityError::StorageError(e.to_string()),
            })?;

        // Verify password
        let is_valid = AuthManager::verify_password(password, &credentials.password_hash)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        if !is_valid {
            return Err(IdentityError::InvalidPassword);
        }

        // Read user metadata
        let user: User = self
            .file_manager
            .read_encrypted_file(username, "meta/user.json", password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        Ok(user)
    }

    /// Get the user's private key
    pub fn get_user_private_key(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Vec<u8>, IdentityError> {
        // Authenticate the user first
        let _user = self.authenticate_user(username, password)?;

        // Read the private key file
        let private_key: Vec<u8> = self
            .file_manager
            .read_encrypted_file(username, "meta/private_key.enc", password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        Ok(private_key)
    }

    pub fn get_user_public_key(&self, username: &str) -> Result<String, IdentityError> {
        // Read user metadata
        // We need a way to read user data without password for public key
        // For now, we'll assume there's a public user info file
        let _user_info_path = format!("users/{}/public_info.json", username);

        // This is a simplified approach - in a real implementation, we'd have
        // a public directory with user public keys
        Err(IdentityError::UserNotFound(username.to_string()))
    }

    pub fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), IdentityError> {
        // Authenticate with old password
        self.authenticate_user(username, old_password)?;

        // Read credentials with old password
        let mut credentials: UserCredentials = self
            .file_manager
            .read_encrypted_file(username, "meta/credentials.json", old_password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Hash new password
        let new_password_hash = AuthManager::hash_password(new_password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Update credentials
        credentials.password_hash = new_password_hash;

        // Save updated credentials with new password
        self.file_manager
            .write_encrypted_file(
                username,
                "meta/credentials.json",
                &credentials,
                new_password,
            )
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Re-encrypt user metadata with new password
        let user: User = self
            .file_manager
            .read_encrypted_file(username, "meta/user.json", old_password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        self.file_manager
            .write_encrypted_file(username, "meta/user.json", &user, new_password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        Ok(())
    }

    fn initialize_user_data(&self, username: &str, password: &str) -> Result<(), IdentityError> {
        // Create initial user directories
        let directories = ["contacts", "chats", "files"];

        for _dir in &directories {
            let _dir_path = format!("data/{}", _dir);
            // We don't actually need to create empty directories since our file manager
            // will create them when writing files
        }

        // Create initial empty files
        let empty_contacts: HashMap<String, String> = HashMap::new(); // contact_id -> contact_data
        self.file_manager
            .write_encrypted_file(username, "data/contacts.json", &empty_contacts, password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        let empty_chats: HashMap<String, Vec<String>> = HashMap::new(); // contact_id -> messages
        self.file_manager
            .write_encrypted_file(username, "data/chats.json", &empty_chats, password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        Ok(())
    }
}
