use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub public_key: String,
    pub alias: Option<String>,
    pub added_at: u64,
    pub is_online: bool,
    #[serde(default)]
    pub groups: Vec<String>, // List of group names this contact belongs to
}

impl Contact {
    pub fn new(public_key: String, alias: Option<String>) -> Self {
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        Contact {
            public_key,
            alias,
            added_at,
            is_online: false,
            groups: Vec::new(),
        }
    }

    pub fn validate_public_key(public_key: &str) -> bool {
        // Basic validation - in a real implementation, we'd validate the actual key format
        !public_key.is_empty() && public_key.len() <= 1000
    }

    pub fn validate_alias(alias: &Option<String>) -> bool {
        match alias {
            Some(a) => a.len() <= 50,
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
}
