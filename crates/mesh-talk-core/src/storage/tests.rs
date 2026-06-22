#[cfg(test)]
mod tests {
    use crate::storage::file_manager::FileManager;
    use tempfile::TempDir;

    #[test]
    fn test_file_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());

        // We can't directly access the private field, so we'll test by using the user_data_path method
        let user_path = file_manager.user_data_path("testuser");
        assert_eq!(user_path, temp_dir.path().join("users").join("testuser"));
    }

    #[test]
    fn test_create_user_directory() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());

        let result = file_manager.create_user_directory("testuser");
        assert!(result.is_ok());

        let user_dir = file_manager.user_data_path("testuser");
        assert!(user_dir.exists());
        assert!(user_dir.is_dir());
    }

    #[test]
    fn test_write_and_read_encrypted_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());

        let test_data = "Hello, World!".to_string();
        let username = "testuser";
        let filepath = "test.txt";
        let password = "testpassword";

        // Write the encrypted file
        let write_result =
            file_manager.write_encrypted_file(username, filepath, &test_data, password);
        assert!(write_result.is_ok());

        // Read the encrypted file
        let read_result: Result<String, _> =
            file_manager.read_encrypted_file(username, filepath, password);
        assert!(read_result.is_ok());

        let read_data = read_result.unwrap();
        assert_eq!(read_data, test_data);
    }

    #[test]
    fn test_delete_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());

        let test_data = "Hello, World!".to_string();
        let username = "testuser";
        let filepath = "test.txt";
        let password = "testpassword";

        // Write the encrypted file
        let write_result =
            file_manager.write_encrypted_file(username, filepath, &test_data, password);
        assert!(write_result.is_ok());

        // Delete the file
        let delete_result = file_manager.delete_file(username, filepath);
        assert!(delete_result.is_ok());

        // Try to read the deleted file (should fail)
        let read_result: Result<String, _> =
            file_manager.read_encrypted_file(username, filepath, password);
        assert!(read_result.is_err());
    }

    #[test]
    fn test_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());

        let test_data = "Hello, World!".to_string();
        let username = "testuser";
        let filepath = "test.txt";
        let password = "testpassword";

        // File should not exist initially
        assert!(!file_manager.file_exists(username, filepath));

        // Write the encrypted file
        let write_result =
            file_manager.write_encrypted_file(username, filepath, &test_data, password);
        assert!(write_result.is_ok());

        // File should now exist
        assert!(file_manager.file_exists(username, filepath));
    }

    #[test]
    fn test_encryption_with_wrong_password() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());

        let test_data = "Hello, World!".to_string();
        let username = "testuser";
        let filepath = "test.txt";
        let password = "testpassword";
        let wrong_password = "wrongpassword";

        // Write the encrypted file
        let write_result =
            file_manager.write_encrypted_file(username, filepath, &test_data, password);
        assert!(write_result.is_ok());

        // Try to read with wrong password (should fail)
        let read_result: Result<String, _> =
            file_manager.read_encrypted_file(username, filepath, wrong_password);
        assert!(read_result.is_err());
    }
}
