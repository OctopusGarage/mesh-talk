//! Platform abstraction layer for Mesh-Talk
//!
//! This module provides a unified interface for platform-specific functionality
//! such as secure storage, notifications, and system integration.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
pub use windows::*;

/// Platform-specific error types
#[derive(Debug, Clone, PartialEq)]
pub enum PlatformError {
    /// Failed to access secure storage
    SecureStorageError(String),
    /// Failed to send notification
    NotificationError(String),
    /// Platform-specific feature not supported
    UnsupportedFeature(String),
    /// Internal error
    InternalError(String),
}

/// Platform result type
pub type PlatformResult<T> = Result<T, PlatformError>;

/// Trait for platform-specific secure storage operations
pub trait SecureStorage {
    /// Store a value in secure storage
    fn store_secure(&self, key: &str, value: &str) -> PlatformResult<()>;

    /// Retrieve a value from secure storage
    fn retrieve_secure(&self, key: &str) -> PlatformResult<Option<String>>;

    /// Delete a value from secure storage
    fn delete_secure(&self, key: &str) -> PlatformResult<()>;
}

/// Trait for platform-specific notification operations
pub trait NotificationProvider {
    /// Send a notification with title and body
    fn send_notification(&self, title: &str, body: &str) -> PlatformResult<()>;

    /// Check if notifications are supported on this platform
    fn is_supported(&self) -> bool;
}

/// Get the platform-specific secure storage implementation
pub fn get_secure_storage() -> Box<dyn SecureStorage> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacOSSecureStorage::new())
    }

    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsSecureStorage::new())
    }

    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxSecureStorage::new())
    }

    // Fallback for unsupported platforms
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Box::new(crate::platform::fallback::FallbackSecureStorage::new())
    }
}

/// Get the platform-specific notification provider
pub fn get_notification_provider() -> Box<dyn NotificationProvider> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacOSNotificationProvider::new())
    }

    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsNotificationProvider::new())
    }

    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxNotificationProvider::new())
    }

    // Fallback for unsupported platforms
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Box::new(crate::platform::fallback::FallbackNotificationProvider::new())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod fallback {
    use super::{NotificationProvider, PlatformError, PlatformResult, SecureStorage};

    pub struct FallbackSecureStorage;

    impl FallbackSecureStorage {
        pub fn new() -> Self {
            Self
        }
    }

    impl SecureStorage for FallbackSecureStorage {
        fn store_secure(&self, _key: &str, _value: &str) -> PlatformResult<()> {
            Err(PlatformError::UnsupportedFeature(
                "Secure storage not supported on this platform".to_string(),
            ))
        }

        fn retrieve_secure(&self, _key: &str) -> PlatformResult<Option<String>> {
            Err(PlatformError::UnsupportedFeature(
                "Secure storage not supported on this platform".to_string(),
            ))
        }

        fn delete_secure(&self, _key: &str) -> PlatformResult<()> {
            Err(PlatformError::UnsupportedFeature(
                "Secure storage not supported on this platform".to_string(),
            ))
        }
    }

    pub struct FallbackNotificationProvider;

    impl FallbackNotificationProvider {
        pub fn new() -> Self {
            Self
        }
    }

    impl NotificationProvider for FallbackNotificationProvider {
        fn send_notification(&self, _title: &str, _body: &str) -> PlatformResult<()> {
            Err(PlatformError::UnsupportedFeature(
                "Notifications not supported on this platform".to_string(),
            ))
        }

        fn is_supported(&self) -> bool {
            false
        }
    }
}
