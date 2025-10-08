use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub ip: String,              // IP address of the contact
    pub port: u16,               // Port number of the contact
    pub username: String,        // User's display name
    pub user_id: Option<String>, // Optional unique user identifier
    pub added_at: u64,
    pub is_online: bool,
    #[serde(default)]
    pub groups: Vec<String>, // List of group names this contact belongs to
}

impl Contact {
    pub fn new(ip: String, port: u16, username: String, user_id: Option<String>) -> Self {
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        Contact {
            ip,
            port,
            username,
            user_id,
            added_at,
            is_online: false,
            groups: Vec::new(),
        }
    }

    pub fn validate_ip(ip: &str) -> bool {
        // Basic validation - IP should not be empty and should be a valid format
        !ip.is_empty() && ip.len() <= 45 // IPv6 max length is 39, but allowing some buffer
    }

    pub fn validate_port(port: u16) -> bool {
        // Port should be in valid range (typically 1-65535)
        port > 0
    }

    pub fn validate_username(username: &str) -> bool {
        // Basic validation - username should not be empty and not too long
        !username.is_empty() && username.len() <= 100
    }

    pub fn validate_user_id(user_id: &Option<String>) -> bool {
        match user_id {
            Some(id) => !id.is_empty() && id.len() <= 100,
            None => true,
        }
    }

    pub fn set_online_status(&mut self, is_online: bool) {
        self.is_online = is_online;
    }

    /// Add a group to this contact
    pub fn add_group(&mut self, group: &str) {
        if !self.groups.contains(&group.to_string()) {
            self.groups.push(group.to_string());
        }
    }

    /// Remove a group from this contact
    pub fn remove_group(&mut self, group: &str) {
        self.groups.retain(|g| g != group);
    }

    /// Check if this contact belongs to a specific group
    pub fn in_group(&self, group: &str) -> bool {
        self.groups.contains(&group.to_string())
    }

    /// Get the full address as IP:Port string
    pub fn get_address(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }
}
