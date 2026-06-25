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
        let mut user: User = self
            .file_manager
            .read_encrypted_file(username, "meta/user.json", password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        // Load the editable display name from its side file, if one was ever set. Absent
        // for accounts that never set a nickname (and for all accounts created before the
        // feature) — those fall back to the username via `effective_display_name`.
        if self
            .file_manager
            .file_exists(username, "meta/display_name.enc")
        {
            if let Ok(display_name) = self.file_manager.read_encrypted_file::<String>(
                username,
                "meta/display_name.enc",
                password,
            ) {
                user.display_name = display_name;
            }
        }

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

        // Re-encrypt the nickname side file too, if present, so it stays readable under the
        // new password (it's encrypted with the account password like every other file).
        if self
            .file_manager
            .file_exists(username, "meta/display_name.enc")
        {
            let display_name: String = self
                .file_manager
                .read_encrypted_file(username, "meta/display_name.enc", old_password)
                .map_err(|e| IdentityError::StorageError(e.to_string()))?;
            self.file_manager
                .write_encrypted_file(
                    username,
                    "meta/display_name.enc",
                    &display_name,
                    new_password,
                )
                .map_err(|e| IdentityError::StorageError(e.to_string()))?;
        }

        Ok(())
    }

    /// Update the editable display name (nickname) for `username`, verifying `password`
    /// and persisting it to its own side file (`meta/display_name.enc`). `username` — the
    /// login key and on-disk namespace — is unchanged, and `user.json` is left untouched so
    /// its on-disk layout never changes. Returns the updated user.
    pub fn set_display_name(
        &self,
        username: &str,
        password: &str,
        display_name: &str,
    ) -> Result<User, IdentityError> {
        let validated =
            User::validate_display_name(display_name).ok_or(IdentityError::InvalidDisplayName)?;

        // Authenticate (verifies password + existence) and load current metadata.
        let mut user = self.authenticate_user(username, password)?;

        // The nickname lives in its own side file (see `User::display_name`); `user.json`
        // is left untouched so its bincode layout never changes.
        self.file_manager
            .write_encrypted_file(username, "meta/display_name.enc", &validated, password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        user.display_name = validated;
        Ok(user)
    }

    fn initialize_user_data(&self, username: &str, password: &str) -> Result<(), IdentityError> {
        // Create initial user directories
        let directories = ["contacts", "chats", "files"];

        for _dir in &directories {
            let _dir_path = format!("data/{}", _dir);
            // We don't actually need to create empty directories since our file manager
            // will create them when writing files
        }

        // Note: data/contacts.json is intentionally NOT pre-created here.
        // ContactManager owns that file and stores it using public-key (base64)
        // encryption via PublicKeyFileManager. Pre-creating it with the
        // password-based binary FileManager format caused a format mismatch
        // ("stream did not contain valid UTF-8") on the first contact load.
        // ContactManager.load_contacts() already treats a missing file as an
        // empty contact list.

        let empty_chats: HashMap<String, Vec<String>> = HashMap::new(); // contact_id -> messages
        self.file_manager
            .write_encrypted_file(username, "data/chats.json", &empty_chats, password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> (IdentityManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let fm = FileManager::new(dir.path().to_path_buf());
        (IdentityManager::new(fm), dir)
    }

    #[test]
    fn set_display_name_persists_and_leaves_username_intact() {
        let (mgr, _dir) = manager();
        mgr.register_user("alice", "supersafepass").unwrap();
        // Default: no display name set yet → effective name falls back to the username.
        let before = mgr.authenticate_user("alice", "supersafepass").unwrap();
        assert_eq!(before.display_name, "");
        assert_eq!(before.effective_display_name(), "alice");

        let updated = mgr
            .set_display_name("alice", "supersafepass", "  老王 🐻 ")
            .unwrap();
        assert_eq!(updated.display_name, "老王 🐻"); // trimmed, unicode/emoji kept
        assert_eq!(updated.username, "alice"); // login key unchanged

        // Persisted: a fresh authenticate reads the new display name back.
        let reloaded = mgr.authenticate_user("alice", "supersafepass").unwrap();
        assert_eq!(reloaded.display_name, "老王 🐻");
        // And login still works under the unchanged username.
        assert!(mgr.authenticate_user("alice", "supersafepass").is_ok());
    }

    #[test]
    fn set_display_name_rejects_invalid_and_wrong_password() {
        let (mgr, _dir) = manager();
        mgr.register_user("bob", "supersafepass").unwrap();
        assert!(matches!(
            mgr.set_display_name("bob", "supersafepass", "  "),
            Err(IdentityError::InvalidDisplayName)
        ));
        assert!(matches!(
            mgr.set_display_name("bob", "wrongpass", "Bob"),
            Err(IdentityError::InvalidPassword)
        ));
    }
}
