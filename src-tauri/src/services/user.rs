//! The authenticated user record — display name, id, address, timestamps — returned by
//! [`crate::services::auth_service::AuthService`] and held in the in-memory session.
//! Relocated here from the retired `domain` module (it is the only `domain` type the
//! kept auth path needs).

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A Mesh-Talk user as known to the auth/session layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Unique identifier for the user.
    pub user_id: String,
    /// The login username — the immutable handle the on-disk store is namespaced by and
    /// the key the keychain / auto-login use. Not shown to peers; see `display_name`.
    pub name: String,
    /// The editable, peer-facing display name (nickname). Defaults to `name` until the
    /// user changes it. This is what the node advertises in its announce.
    pub display_name: String,
    /// User's unique identifier/address.
    pub address: String,
    /// Timestamp when the user was created.
    pub created_at: u64,
    /// Timestamp when the user was last seen online.
    pub last_seen: u64,
    /// Whether the user is currently online.
    pub is_online: bool,
}

impl User {
    /// Update the last-seen timestamp to now and mark the user online.
    pub fn update_last_seen(&mut self) {
        self.last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.is_online = true;
    }
}
