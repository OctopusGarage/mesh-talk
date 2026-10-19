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
            if !self
                .file_manager
                .file_exists(username, "meta/credentials.json")
                || !self
                    .file_manager
                    .file_exists(username, "meta/private_key.enc")
            {
                return Err(IdentityError::StorageError(
                    "Incomplete account directory exists; preserve it for account recovery before registering again".into(),
                ));
            }
            return Err(IdentityError::UserAlreadyExists(username.to_string()));
        }

        // Generate key pair
        let (public_key, signing_key) =
            KeyManager::generate_keypair().map_err(|_| IdentityError::KeyGenerationFailed)?;

        // Hash password
        let password_hash = AuthManager::hash_password(password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

        let secret_key_bytes = KeyManager::serialize_secret_key(&signing_key);
        let public_key_str = KeyManager::serialize_public_key(&public_key);
        let user = User::new(username.to_string(), public_key_str);
        let credentials = UserCredentials {
            username: username.to_string(),
            password_hash,
            private_key: secret_key_bytes.clone(),
        };

        // Publish the complete aggregate in one rename. A failed write or process
        // exit while staging must never reserve the account's login namespace.
        self.file_manager
            .publish_user(username, |staged| {
                staged.write_encrypted_file(
                    username,
                    "meta/private_key.enc",
                    &secret_key_bytes,
                    password,
                )?;
                staged.write_encrypted_file(username, "meta/user.json", &user, password)?;
                staged.write_encrypted_file(
                    username,
                    "meta/credentials.json",
                    &credentials,
                    password,
                )?;
                staged.write_encrypted_file(
                    username,
                    "meta/user_id.enc",
                    &user.user_id,
                    password,
                )?;
                // ContactManager owns contacts.json and its public-key format.
                let empty_chats: HashMap<String, Vec<String>> = HashMap::new();
                staged.write_encrypted_file(username, "data/chats.json", &empty_chats, password)
            })
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;

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
            && let Ok(display_name) = self.file_manager.read_encrypted_file::<String>(
                username,
                "meta/display_name.enc",
                password,
            )
        {
            user.display_name = display_name;
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

        // Re-encrypt the private key under the new password — it's stored password-encrypted
        // like every other meta file, so without this it would become permanently unreadable.
        let private_key: Vec<u8> = self
            .file_manager
            .read_encrypted_file(username, "meta/private_key.enc", old_password)
            .map_err(|e| IdentityError::StorageError(e.to_string()))?;
        self.file_manager
            .write_encrypted_file(username, "meta/private_key.enc", &private_key, new_password)
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
    fn registration_preserves_an_existing_partial_account() {
        let (mgr, dir) = manager();
        let path = dir.path().join("users/alice/meta/private_key.enc");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"recoverable old key").unwrap();
        assert!(mgr.register_user("alice", "password").is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"recoverable old key");
        assert!(!mgr.file_manager.file_exists("alice", "meta/user.json"));
    }

    #[test]
    fn registration_reports_incomplete_legacy_metadata_for_recovery() {
        let (mgr, dir) = manager();
        let metadata = dir.path().join("users/alice/meta/user.json");
        std::fs::create_dir_all(metadata.parent().unwrap()).unwrap();
        std::fs::write(&metadata, b"recoverable metadata").unwrap();
        let error = mgr.register_user("alice", "password").unwrap_err();
        assert!(matches!(error, IdentityError::StorageError(_)));
        assert!(error.to_string().contains("recovery"));
        assert_eq!(std::fs::read(metadata).unwrap(), b"recoverable metadata");
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

    #[test]
    fn change_password_rotates_credentials_and_reencrypts_nickname() {
        let (mgr, _dir) = manager();
        mgr.register_user("carol", "oldpassword123").unwrap();
        mgr.set_display_name("carol", "oldpassword123", "Carol C")
            .unwrap();

        // The wrong old password is rejected (authenticates with old first).
        assert!(matches!(
            mgr.change_password("carol", "wrongold", "newpassword456"),
            Err(IdentityError::InvalidPassword)
        ));

        // Rotate to a new password.
        mgr.change_password("carol", "oldpassword123", "newpassword456")
            .unwrap();

        // The old password no longer authenticates; the new one does.
        assert!(matches!(
            mgr.authenticate_user("carol", "oldpassword123"),
            Err(IdentityError::InvalidPassword)
        ));
        let user = mgr.authenticate_user("carol", "newpassword456").unwrap();
        // The nickname side file was re-encrypted under the new password and reads back.
        assert_eq!(user.display_name, "Carol C");
        // The private key is readable under the new password.
        assert!(mgr.get_user_private_key("carol", "newpassword456").is_ok());
    }

    #[test]
    fn register_and_authenticate_reject_bad_input() {
        let (mgr, _dir) = manager();
        // Invalid username on both register and authenticate.
        assert!(matches!(
            mgr.register_user("", "supersafepass"),
            Err(IdentityError::InvalidUsername)
        ));
        assert!(matches!(
            mgr.authenticate_user("", "supersafepass"),
            Err(IdentityError::InvalidUsername)
        ));

        mgr.register_user("dave", "supersafepass").unwrap();
        // Duplicate registration is refused.
        assert!(matches!(
            mgr.register_user("dave", "supersafepass"),
            Err(IdentityError::UserAlreadyExists(_))
        ));
        // Unknown user and wrong password.
        assert!(matches!(
            mgr.authenticate_user("nobody", "supersafepass"),
            Err(IdentityError::UserNotFound(_))
        ));
        assert!(matches!(
            mgr.authenticate_user("dave", "wrongpass"),
            Err(IdentityError::InvalidPassword)
        ));
        // The private key reads back for the right password; the public-key getter is the
        // documented not-implemented path (returns UserNotFound).
        assert!(mgr.get_user_private_key("dave", "supersafepass").is_ok());
        assert!(matches!(
            mgr.get_user_public_key("dave"),
            Err(IdentityError::UserNotFound(_))
        ));
    }
}
