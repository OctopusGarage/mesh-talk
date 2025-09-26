//! Notifications module for Mesh-Talk
//!
//! This module provides a comprehensive notifications system with desktop notifications,
//! system tray integration, and configurable settings.

pub mod desktop;
pub mod settings;
pub mod tray;

#[cfg(test)]
mod tests;

/// Notifications module error types
#[derive(Debug, Clone, PartialEq)]
pub enum NotificationError {
    /// Notification permission not granted
    PermissionDenied,
    /// Failed to send notification
    SendFailed(String),
    /// Internal error
    InternalError(String),
}

/// Notifications result type
pub type NotificationResult<T> = Result<T, NotificationError>;
