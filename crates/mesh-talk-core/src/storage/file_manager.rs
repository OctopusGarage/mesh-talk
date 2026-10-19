use crate::storage::encryption::{EncryptionKey, decrypt_data, encrypt_data, generate_salt};
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

    /// Construct a new account off to the side, then publish its complete directory.
    /// Existing directories (including interrupted legacy registrations) are retained
    /// for recovery rather than merged with newly generated identity material.
    pub(crate) fn publish_user(
        &self,
        username: &str,
        populate: impl FnOnce(&FileManager) -> Result<(), StorageError>,
    ) -> Result<(), StorageError> {
        let destination = self.user_data_path(username);
        let check_destination = || -> Result<(), StorageError> {
            match fs::symlink_metadata(&destination) {
                Ok(_) => Err(StorageError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "Account directory {} already exists; preserve it for account recovery before registering again",
                        destination.display()
                    ),
                ))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        };
        check_destination()?;
        let users = self.base_path.join("users");
        fs::create_dir_all(&users)?;
        let staging = tempfile::Builder::new()
            .prefix(".registration-")
            .tempdir_in(&users)?;
        let staged = FileManager::new(staging.path().to_path_buf());
        populate(&staged)?;
        let source = staged.user_data_path(username);
        sync_directory(&source)?;
        check_destination()?;
        fs::rename(&source, &destination)?;
        // The rename is the publication boundary; do not report a recoverable
        // pre-publication failure after the account is already visible.
        let _ = sync_directory(&users);
        Ok(())
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

        // Fully write and synchronize a sibling before atomically replacing the
        // destination. Failed writes retain the previous decryptable file.
        let parent = full_path.parent().expect("user file has a parent");
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&salt)?;
        file.write_all(&nonce)?;
        file.write_all(&ciphertext)?;
        file.as_file().sync_all()?;
        file.persist(&full_path)
            .map_err(|e| StorageError::Io(e.error))?;
        let _ = sync_directory(parent);
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

// Opening directories for fsync is supported on Unix. Windows publication still
// uses same-volume rename, with each encrypted file flushed before publication.
fn sync_directory(path: &std::path::Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_replacement_retains_a_readers_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let manager = FileManager::new(dir.path().to_path_buf());
        manager
            .write_encrypted_file("alice", "meta/value.enc", &"before", "pw")
            .unwrap();
        let root = manager.user_data_path("alice");
        // A hard link retains the same inode as an already-open reader, and
        // makes the assertion portable without relying on read/write timing.
        fs::hard_link(root.join("meta/value.enc"), root.join("meta/previous.enc")).unwrap();
        manager
            .write_encrypted_file("alice", "meta/value.enc", &"after", "pw")
            .unwrap();
        assert_eq!(
            manager
                .read_encrypted_file::<String>("alice", "meta/value.enc", "pw")
                .unwrap(),
            "after"
        );
        assert_eq!(
            manager
                .read_encrypted_file::<String>("alice", "meta/previous.enc", "pw")
                .unwrap(),
            "before"
        );
    }

    #[test]
    fn failed_user_construction_is_unpublished_and_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let manager = FileManager::new(dir.path().to_path_buf());
        let result = manager.publish_user("alice", |staged| {
            staged.write_encrypted_file("alice", "meta/user.json", &"user", "pw")?;
            assert!(!manager.file_exists("alice", "meta/user.json"));
            // A real filesystem failure after metadata construction: a parent
            // component is a file, so writing credentials cannot complete.
            fs::write(staged.user_data_path("alice").join("blocked"), b"blocked")?;
            staged.write_encrypted_file("alice", "blocked/credentials.json", &"credentials", "pw")
        });
        assert!(result.is_err());
        assert!(!manager.user_data_path("alice").exists());
        assert_eq!(fs::read_dir(dir.path().join("users")).unwrap().count(), 0);
        let identity = crate::identity::manager::IdentityManager::new(manager);
        let user = identity.register_user("alice", "pw").unwrap();
        assert_eq!(
            identity.authenticate_user("alice", "pw").unwrap().user_id,
            user.user_id
        );
        assert!(identity.get_user_private_key("alice", "pw").is_ok());
    }
}
