//! Secure key storage for Mesh-Talk
//!
//! This module handles secure storage of keys using platform-specific secure storage.

use super::{CryptoError, CryptoResult};
use crate::platform::{get_secure_storage, SecureStorage};

/// Secure storage manager
pub struct SecureStorageManager {
    storage: Box<dyn SecureStorage>,
}

impl SecureStorageManager {
    /// Create a new secure storage manager
    pub fn new() -> CryptoResult<Self> {
        let storage = get_secure_storage();
        Ok(Self { storage })
    }

    /// Store a key securely
    pub fn store_key(&self, key_id: &str, key_data: &str) -> CryptoResult<()> {
        self.storage
            .store_secure(key_id, key_data)
            .map_err(|e| CryptoError::StorageError(format!("Failed to store key: {:?}", e)))
    }

    /// Retrieve a key from secure storage
    pub fn retrieve_key(&self, key_id: &str) -> CryptoResult<Option<String>> {
        self.storage
            .retrieve_secure(key_id)
            .map_err(|e| CryptoError::StorageError(format!("Failed to retrieve key: {:?}", e)))
    }

    /// Delete a key from secure storage
    pub fn delete_key(&self, key_id: &str) -> CryptoResult<()> {
        self.storage
            .delete_secure(key_id)
            .map_err(|e| CryptoError::StorageError(format!("Failed to delete key: {:?}", e)))
    }
}
