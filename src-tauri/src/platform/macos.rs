//! macOS-specific platform implementations

use super::{NotificationProvider, PlatformResult, SecureStorage};

/// macOS secure storage implementation using Keychain Services
pub struct MacOSSecureStorage;

impl MacOSSecureStorage {
    pub fn new() -> Self {
        Self
    }
}

impl SecureStorage for MacOSSecureStorage {
    fn store_secure(&self, key: &str, _value: &str) -> PlatformResult<()> {
        #[cfg(feature = "macos-platform")]
        {
            use super::PlatformError;
            use keyring::Entry;
            let entry = Entry::new("com.mesh-talk.app", key).map_err(|e| {
                PlatformError::SecureStorageError(format!(
                    "Failed to create keyring entry: {:?}",
                    e
                ))
            })?;
            entry.set_password(_value).map_err(|e| {
                PlatformError::SecureStorageError(format!("Failed to store in keychain: {:?}", e))
            })?;
            Ok(())
        }

        #[cfg(not(feature = "macos-platform"))]
        {
            // Fallback implementation for when the feature is not enabled
            println!("Storing key '{}' in macOS Keychain (placeholder)", key);
            Ok(())
        }
    }

    fn retrieve_secure(&self, key: &str) -> PlatformResult<Option<String>> {
        #[cfg(feature = "macos-platform")]
        {
            use super::PlatformError;
            use keyring::Entry;
            let entry = Entry::new("com.mesh-talk.app", key).map_err(|e| {
                PlatformError::SecureStorageError(format!(
                    "Failed to create keyring entry: {:?}",
                    e
                ))
            })?;
            match entry.get_password() {
                Ok(password) => Ok(Some(password)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(PlatformError::SecureStorageError(format!(
                    "Failed to retrieve from keychain: {:?}",
                    e
                ))),
            }
        }

        #[cfg(not(feature = "macos-platform"))]
        {
            // Fallback implementation for when the feature is not enabled
            println!("Retrieving key '{}' from macOS Keychain (placeholder)", key);
            Ok(None)
        }
    }

    fn delete_secure(&self, key: &str) -> PlatformResult<()> {
        #[cfg(feature = "macos-platform")]
        {
            use super::PlatformError;
            use keyring::Entry;
            let entry = Entry::new("com.mesh-talk.app", key).map_err(|e| {
                PlatformError::SecureStorageError(format!(
                    "Failed to create keyring entry: {:?}",
                    e
                ))
            })?;
            entry.delete_credential().map_err(|e| {
                PlatformError::SecureStorageError(format!(
                    "Failed to delete from keychain: {:?}",
                    e
                ))
            })?;
            Ok(())
        }

        #[cfg(not(feature = "macos-platform"))]
        {
            // Fallback implementation for when the feature is not enabled
            println!("Deleting key '{}' from macOS Keychain (placeholder)", key);
            Ok(())
        }
    }
}

/// macOS notification provider implementation
pub struct MacOSNotificationProvider;

impl MacOSNotificationProvider {
    pub fn new() -> Self {
        Self
    }
}

impl NotificationProvider for MacOSNotificationProvider {
    fn send_notification(&self, title: &str, body: &str) -> PlatformResult<()> {
        // For macOS, we'll use the system's native notification capabilities
        // which are already handled by Tauri's notification system
        println!("Sending macOS notification: {} - {}", title, body);
        // In a real implementation, we would integrate with Tauri's notification system
        // or use a native macOS notification library
        Ok(())
    }

    fn is_supported(&self) -> bool {
        // macOS notifications are supported
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_secure_storage_compilation() {
        // This test ensures that the macOS secure storage implementation compiles correctly
        // It doesn't test the actual functionality, just that the code is valid

        let storage = MacOSSecureStorage::new();
        // Test that we can call the methods without compilation errors
        let _ = storage.store_secure("test_key", "test_value");
        let _ = storage.retrieve_secure("test_key");
        let _ = storage.delete_secure("test_key");
    }
}
