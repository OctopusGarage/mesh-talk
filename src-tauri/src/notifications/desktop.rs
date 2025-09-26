//! Desktop notifications for Mesh-Talk
//!
//! This module handles desktop notifications using platform-native APIs.

use super::{NotificationError, NotificationResult};
use crate::platform::{get_notification_provider, NotificationProvider};

/// Desktop notification manager
pub struct DesktopNotificationManager {
    provider: Box<dyn NotificationProvider>,
}

impl DesktopNotificationManager {
    /// Create a new desktop notification manager
    pub fn new() -> Self {
        let provider = get_notification_provider();
        Self { provider }
    }

    /// Send a simple notification
    pub fn send_notification(&self, message: &str) -> NotificationResult<()> {
        self.send_notification_with_title("Mesh-Talk", message)
    }

    /// Send a notification with title and body
    pub fn send_notification_with_title(&self, title: &str, body: &str) -> NotificationResult<()> {
        self.provider.send_notification(title, body).map_err(|e| {
            NotificationError::SendFailed(format!("Failed to send notification: {:?}", e))
        })
    }

    /// Check if notifications are supported on this platform
    pub fn is_supported(&self) -> bool {
        self.provider.is_supported()
    }
}

impl Default for DesktopNotificationManager {
    fn default() -> Self {
        Self::new()
    }
}
