//! Notification settings for Mesh-Talk
//!
//! This module handles notification preferences and configuration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Notification level
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NotificationLevel {
    /// No notifications
    None,
    /// Only important notifications
    Important,
    /// All notifications
    All,
}

/// Sound settings for notifications
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SoundSetting {
    /// No sound
    None,
    /// Default system sound
    Default,
    /// Custom sound file path
    Custom(String),
}

/// Notification settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// Notification level
    pub level: NotificationLevel,
    /// Sound settings
    pub sound: SoundSetting,
    /// Whether to show notifications in the system tray
    pub show_in_tray: bool,
    /// Custom notification templates
    pub templates: HashMap<String, String>,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            level: NotificationLevel::All,
            sound: SoundSetting::Default,
            show_in_tray: true,
            templates: HashMap::new(),
        }
    }
}

impl NotificationSettings {
    /// Create new notification settings
    pub fn new() -> Self {
        Self::default()
    }

    /// Set notification level
    pub fn set_level(&mut self, level: NotificationLevel) {
        self.level = level;
    }

    /// Set sound setting
    pub fn set_sound(&mut self, sound: SoundSetting) {
        self.sound = sound;
    }

    /// Enable or disable tray notifications
    pub fn set_show_in_tray(&mut self, show: bool) {
        self.show_in_tray = show;
    }

    /// Add a custom notification template
    pub fn add_template(&mut self, name: String, template: String) {
        self.templates.insert(name, template);
    }

    /// Get a notification template
    pub fn get_template(&self, name: &str) -> Option<&String> {
        self.templates.get(name)
    }
}
