use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub username: String,
    pub user_id: String, // UUID
    pub public_key: String,
    pub created_at: u64,
}

impl User {
    pub fn new(username: String, public_key: String) -> Self {
        let user_id = Uuid::new_v4().to_string();
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        User {
            username,
            user_id,
            public_key,
            created_at,
        }
    }

    pub fn validate_username(username: &str) -> bool {
        !username.is_empty()
            && username.len() <= 50
            && username
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    }

    pub fn validate_user_id(user_id: &str) -> bool {
        Uuid::parse_str(user_id).is_ok()
    }
}
