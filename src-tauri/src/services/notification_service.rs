use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Notification error types
#[derive(Debug, Clone, PartialEq)]
pub enum NotificationError {
    /// Notification permission not granted
    PermissionDenied,
    /// Failed to send notification
    SendFailed(String),
    /// Internal error
    InternalError(String),
}

/// Notification result type
pub type NotificationResult<T> = Result<T, NotificationError>;

#[derive(Debug, Serialize, Deserialize, Default)]
struct NotificationPersistedState {
    unread_count: u32,
    notifications_enabled: bool,
}

/// Shared state for the notification service
#[derive(Clone)]
struct NotificationServiceState {
    /// Flag to indicate if notifications are enabled
    enabled: Arc<Mutex<bool>>,
    /// Count of unread messages
    unread_count: Arc<Mutex<u32>>,
    /// Optional app handle for Tauri integration
    app_handle: Arc<RwLock<Option<AppHandle>>>,
    /// Optional path for persisted state
    storage_path: Arc<RwLock<Option<PathBuf>>>,
}

/// Notification service for sending system notifications
pub struct NotificationService {
    state: NotificationServiceState,
}

impl NotificationService {
    /// Create a new notification service
    pub fn new() -> Self {
        Self {
            state: NotificationServiceState {
                enabled: Arc::new(Mutex::new(true)),
                unread_count: Arc::new(Mutex::new(0)),
                app_handle: Arc::new(RwLock::new(None)),
                storage_path: Arc::new(RwLock::new(None)),
            },
        }
    }

    /// Set the app handle for Tauri integration
    pub fn set_app_handle(&self, app_handle: AppHandle) {
        if let Ok(mut handle) = self.state.app_handle.write() {
            *handle = Some(app_handle.clone());
        }

        let path = resolve_storage_path(&app_handle);
        if let Ok(mut storage) = self.state.storage_path.write() {
            *storage = path.clone();
        }

        if let Some(path) = path {
            if let Err(e) = self.load_state(&path) {
                eprintln!("Failed to load notification state from {:?}: {}", path, e);
            }
        }

        self.sync_tray_ui();
    }

    /// Enable or disable notifications
    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(mut enabled_flag) = self.state.enabled.lock() {
            *enabled_flag = enabled;
        }
        self.persist_state();
        self.update_notification_menu();
    }

    /// Toggle notifications and return the new state
    pub fn toggle_notifications(&self) -> bool {
        let enabled = {
            let mut enabled_flag = self.state.enabled.lock().unwrap();
            *enabled_flag = !*enabled_flag;
            *enabled_flag
        };
        self.persist_state();
        self.update_notification_menu();
        enabled
    }

    /// Check if notifications are enabled
    pub fn is_enabled(&self) -> bool {
        if let Ok(enabled_flag) = self.state.enabled.lock() {
            *enabled_flag
        } else {
            true // Default to enabled if we can't acquire the lock
        }
    }

    /// Increment unread message count
    pub fn increment_unread_count(&self) {
        let count = {
            let mut guard = self.state.unread_count.lock().unwrap();
            *guard = guard.saturating_add(1);
            *guard
        };
        self.sync_unread(count);
    }

    /// Reset unread message count
    pub fn reset_unread_count(&self) {
        {
            let mut guard = self.state.unread_count.lock().unwrap();
            *guard = 0;
        }
        self.sync_unread(0);
    }

    /// Set unread message count explicitly
    pub fn set_unread_count(&self, count: u32) {
        {
            let mut guard = self.state.unread_count.lock().unwrap();
            *guard = count;
        }
        self.sync_unread(count);
    }

    /// Get unread message count
    pub fn get_unread_count(&self) -> u32 {
        if let Ok(count) = self.state.unread_count.lock() {
            *count
        } else {
            0
        }
    }

    /// Send a simple notification
    pub fn send_notification(&self, message: &str) -> NotificationResult<()> {
        self.dispatch_notification(None, message, || {
            println!("[NOTIFICATION] {}", message);
        })
    }

    /// Send a notification with title and body
    pub fn send_notification_with_title(&self, title: &str, body: &str) -> NotificationResult<()> {
        self.dispatch_notification(Some(title), body, || {
            println!("[NOTIFICATION] {}: {}", title, body);
        })
    }

    /// Send a new message notification
    pub fn send_new_message_notification(
        &self,
        from: &str,
        message: &str,
    ) -> NotificationResult<()> {
        let title = format!("New message from {}", from);
        let body = if message.len() > 100 {
            format!("{}...", &message[..100])
        } else {
            message.to_string()
        };

        self.dispatch_notification(Some(&title), &body, || {
            // println!("[NOTIFICATION] {}: {}", title, body);
        })
    }

    fn dispatch_notification<F>(
        &self,
        title: Option<&str>,
        body: &str,
        fallback: F,
    ) -> NotificationResult<()>
    where
        F: FnOnce(),
    {
        if !self.is_enabled() {
            return Ok(());
        }

        self.increment_unread_count();

        let mut fallback_needed = true;

        if let Some(result) = self.with_app_handle_result(|app_handle| {
            let builder = app_handle.notification().builder();
            let builder = if let Some(title) = title {
                builder.title(title)
            } else {
                builder
            };
            builder.body(body.to_string()).show()
        }) {
            match result {
                Ok(()) => fallback_needed = false,
                Err(err) => {
                    eprintln!("Failed to show system notification: {}", err);
                }
            }
        }

        if fallback_needed {
            fallback();
        }

        Ok(())
    }

    fn sync_unread(&self, count: u32) {
        self.update_tray_icon(count);
        self.persist_state();
    }

    fn sync_tray_ui(&self) {
        let count = self.get_unread_count();
        self.update_tray_icon(count);
        self.update_notification_menu();
    }

    /// Update the tray icon with unread count
    fn update_tray_icon(&self, count: u32) {
        self.with_app_handle(|app_handle| {
            crate::tray::update_unread_count(app_handle, count);
        });
    }

    fn update_notification_menu(&self) {
        let enabled = self.is_enabled();
        self.with_app_handle(|app_handle| {
            crate::tray::update_notification_menu(app_handle, enabled);
        });
    }

    fn with_app_handle<F>(&self, f: F)
    where
        F: FnOnce(&AppHandle),
    {
        let _ = self.with_app_handle_result(|handle| {
            f(handle);
        });
    }

    fn with_app_handle_result<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&AppHandle) -> R,
    {
        if let Ok(handle) = self.state.app_handle.read() {
            if let Some(app_handle) = &*handle {
                return Some(f(app_handle));
            }
        }
        None
    }

    fn load_state(&self, path: &PathBuf) -> std::io::Result<()> {
        if !path.exists() {
            return Ok(());
        }

        let contents = fs::read_to_string(path)?;
        let persisted: NotificationPersistedState = serde_json::from_str(&contents)?;

        if let Ok(mut enabled_flag) = self.state.enabled.lock() {
            *enabled_flag = persisted.notifications_enabled;
        }

        if let Ok(mut unread) = self.state.unread_count.lock() {
            *unread = persisted.unread_count;
        }

        Ok(())
    }

    fn persist_state(&self) {
        let path = {
            let guard = self.state.storage_path.read().unwrap();
            guard.clone()
        };

        let Some(path) = path else {
            return;
        };

        let state = NotificationPersistedState {
            unread_count: self.get_unread_count(),
            notifications_enabled: self.is_enabled(),
        };

        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!(
                    "Failed to create notification state directory {:?}: {}",
                    parent, e
                );
                return;
            }
        }

        if let Err(e) = fs::write(&path, serde_json::to_vec(&state).unwrap_or_default()) {
            eprintln!("Failed to persist notification state {:?}: {}", path, e);
        }
    }
}

impl Default for NotificationService {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_storage_path(app_handle: &AppHandle) -> Option<PathBuf> {
    use tauri::Manager as _;
    app_handle
        .path()
        .app_config_dir()
        .ok()
        .or_else(|| app_handle.path().app_data_dir().ok())
        .map(|dir| dir.join("notification_state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_service_creation() {
        let notification_service = NotificationService::new();
        assert!(notification_service.is_enabled());
        assert_eq!(notification_service.get_unread_count(), 0);
    }

    #[test]
    fn test_notification_service_enable_disable() {
        let notification_service = NotificationService::new();
        assert!(notification_service.is_enabled());

        notification_service.set_enabled(false);
        assert!(!notification_service.is_enabled());

        notification_service.set_enabled(true);
        assert!(notification_service.is_enabled());
    }

    #[test]
    fn test_send_notification_increments_unread() {
        let notification_service = NotificationService::new();
        let result = notification_service.send_notification("Test message");
        assert!(result.is_ok());
        assert_eq!(notification_service.get_unread_count(), 1);
    }

    #[test]
    fn test_unread_reset() {
        let notification_service = NotificationService::new();
        notification_service.increment_unread_count();
        notification_service.reset_unread_count();
        assert_eq!(notification_service.get_unread_count(), 0);
    }
}
