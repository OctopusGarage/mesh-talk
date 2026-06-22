use crate::storage::encryption::{decrypt_data, encrypt_data, generate_salt, EncryptionKey};
use crate::storage::errors::StorageError;
use crate::storage::serialization::{deserialize_data, serialize_data};
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FileManager {
    base_path: PathBuf,
}

impl FileManager {
    pub fn base_path(&self) -> &PathBuf {
        &self.base_path
    }
}

impl FileManager {
    pub fn new(base_path: PathBuf) -> Self {
        FileManager { base_path }
    }

    pub fn create_user_directory(&self, username: &str) -> Result<(), StorageError> {
        let user_dir = self.user_data_path(username);

        if !user_dir.exists() {
            fs::create_dir_all(&user_dir)
                .map_err(|_| StorageError::DirectoryCreationFailed(user_dir.clone()))?;
        }

        Ok(())
    }

    pub fn user_data_path(&self, username: &str) -> PathBuf {
        self.base_path.join("users").join(username)
    }

    pub fn write_encrypted_file<T>(
        &self,
        username: &str,
        filepath: &str,
        data: &T,
        password: &str,
    ) -> Result<(), StorageError>
    where
        T: Serialize,
    {
        // Create user directory if it doesn't exist
        self.create_user_directory(username)?;

        // Serialize the data
        let serialized_data = serialize_data(data)?;

        // Generate salt and derive key
        let salt = generate_salt();
        let key = EncryptionKey::from_password(password, &salt)?;

        // Encrypt the data
        let (ciphertext, nonce) = encrypt_data(&serialized_data, &key)?;

        // Prepare the full file path
        let full_path = self.user_data_path(username).join(filepath);

        // Create parent directories if they don't exist
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| StorageError::DirectoryCreationFailed(parent.to_path_buf()))?;
        }

        // Write salt, nonce, and ciphertext to file
        let mut file = File::create(&full_path)?;
        file.write_all(&salt)?;
        file.write_all(&nonce)?;
        file.write_all(&ciphertext)?;

        Ok(())
    }

    pub fn read_encrypted_file<T>(
        &self,
        username: &str,
        filepath: &str,
        password: &str,
    ) -> Result<T, StorageError>
    where
        T: for<'de> Deserialize<'de>,
    {
        // Prepare the full file path
        let full_path = self.user_data_path(username).join(filepath);

        // Check if file exists
        if !full_path.exists() {
            return Err(StorageError::FileNotFound(full_path));
        }

        // Read the file
        let mut file = File::open(&full_path)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;

        // Check if file is large enough to contain salt and nonce
        if contents.len() < 16 + 12 {
            return Err(StorageError::Deserialization(
                "File is too small to contain salt and nonce".to_string(),
            ));
        }

        // Extract salt, nonce, and ciphertext — parse fallibly (no panic) so a future
        // change to the length check above can't turn a truncated/corrupt file into a
        // slice-index panic.
        let salt: [u8; 16] = contents
            .get(0..16)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| StorageError::Deserialization("missing salt".to_string()))?;
        let nonce: [u8; 12] = contents
            .get(16..28)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| StorageError::Deserialization("missing nonce".to_string()))?;
        let ciphertext = &contents[28..];

        // Derive key from password and salt
        let key = EncryptionKey::from_password(password, &salt)?;

        // Decrypt the data
        let decrypted_data = decrypt_data(ciphertext, &nonce, &key)?;

        // Deserialize the data
        let deserialized_data = deserialize_data(&decrypted_data)?;

        Ok(deserialized_data)
    }

    pub fn delete_file(&self, username: &str, filepath: &str) -> Result<(), StorageError> {
        let full_path = self.user_data_path(username).join(filepath);

        if full_path.exists() {
            fs::remove_file(&full_path)?;
        }

        Ok(())
    }

    pub fn file_exists(&self, username: &str, filepath: &str) -> bool {
        let full_path = self.user_data_path(username).join(filepath);
        full_path.exists()
    }
}
