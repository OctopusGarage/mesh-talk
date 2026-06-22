#[cfg(test)]
mod tests {
    use crate::identity::auth::AuthManager;
    use crate::identity::keys::KeyManager;
    use crate::identity::manager::IdentityManager;
    use crate::identity::user::User;
    use crate::storage::file_manager::FileManager;
    use tempfile::TempDir;

    #[test]
    fn test_user_creation() {
        let public_key = "test_public_key".to_string();
        let user = User::new("testuser".to_string(), public_key.clone());

        assert_eq!(user.username, "testuser");
        assert_eq!(user.public_key, public_key);
        assert!(User::validate_username(&user.username));
        assert!(User::validate_user_id(&user.user_id));
    }

    #[test]
    fn test_user_validation() {
        assert!(User::validate_username("valid_username"));
        assert!(User::validate_username("valid-username123"));
        assert!(!User::validate_username(""));
        assert!(!User::validate_username(&"a".repeat(51))); // Too long
    }

    #[test]
    fn test_password_hashing() {
        let password = "test_password";
        let hash_result = AuthManager::hash_password(password);
        assert!(hash_result.is_ok());

        let hash = hash_result.unwrap();
        let verify_result = AuthManager::verify_password(password, &hash);
        assert!(verify_result.is_ok());
        assert!(verify_result.unwrap());

        // Test with wrong password
        let verify_result = AuthManager::verify_password("wrong_password", &hash);
        assert!(verify_result.is_ok());
        assert!(!verify_result.unwrap());
    }

    #[test]
    fn test_key_generation() {
        let keypair_result = KeyManager::generate_keypair();
        assert!(keypair_result.is_ok());

        let (verifying_key, signing_key) = keypair_result.unwrap();

        // Test serialization/deserialization
        let verifying_key_str = KeyManager::serialize_public_key(&verifying_key);
        let deserialized_verifying = KeyManager::deserialize_public_key(&verifying_key_str);
        assert!(deserialized_verifying.is_ok());

        let signing_key_bytes = KeyManager::serialize_secret_key(&signing_key);
        let deserialized_signing = KeyManager::deserialize_secret_key(&signing_key_bytes);
        assert!(deserialized_signing.is_ok());
    }

    #[test]
    fn test_identity_manager_registration() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager);

        let username = "testuser";
        let password = "testpassword";

        // Register a new user
        let register_result = identity_manager.register_user(username, password);
        assert!(register_result.is_ok());

        let user = register_result.unwrap();
        assert_eq!(user.username, username);

        // Try to register the same user again (should fail)
        let register_result = identity_manager.register_user(username, password);
        assert!(register_result.is_err());
    }

    #[test]
    fn test_identity_manager_authentication() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager);

        let username = "testuser";
        let password = "testpassword";

        // Register a new user
        let register_result = identity_manager.register_user(username, password);
        assert!(register_result.is_ok());

        // Authenticate with correct password
        let auth_result = identity_manager.authenticate_user(username, password);
        assert!(auth_result.is_ok());

        // Authenticate with wrong password
        let auth_result = identity_manager.authenticate_user(username, "wrongpassword");
        assert!(auth_result.is_err());
    }

    #[test]
    fn test_password_change() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager);

        let username = "testuser";
        let old_password = "oldpassword";
        let new_password = "newpassword";

        // Register a new user
        let register_result = identity_manager.register_user(username, old_password);
        assert!(register_result.is_ok());

        // Change password
        let change_result = identity_manager.change_password(username, old_password, new_password);
        assert!(change_result.is_ok());

        // Authenticate with new password
        let auth_result = identity_manager.authenticate_user(username, new_password);
        assert!(auth_result.is_ok());

        // Authenticate with old password (should fail)
        let auth_result = identity_manager.authenticate_user(username, old_password);
        assert!(auth_result.is_err());
    }
}
