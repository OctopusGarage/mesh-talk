use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub username: String,
    pub user_id: String, // UUID
    pub public_key: String,
    pub created_at: u64,
    /// Editable, peer-facing display name (the "nickname"). Empty means "not set" — fall
    /// back to `username`. Decoupled from `username`, the immutable login key the on-disk
    /// store is namespaced by.
    ///
    /// `#[serde(skip)]` — NOT persisted inside `user.json`. The store uses positional
    /// bincode, so appending a field would break decoding of `user.json` files written
    /// before it existed (they'd hit EOF). Instead the nickname lives in its own side file
    /// (`meta/display_name.enc`), which the manager loads into this field on authenticate.
    #[serde(skip)]
    pub display_name: String,
}

/// The most permissive a display name may be: non-empty after trimming, at most this
/// many characters, and free of control characters. Spaces, Unicode and emoji are
/// allowed — it's a human label, not an identifier.
pub const MAX_DISPLAY_NAME_CHARS: usize = 50;

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
            display_name: String::new(),
        }
    }

    /// The effective peer-facing name: the set `display_name`, or the `username` when none
    /// has been set.
    pub fn effective_display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }

    pub fn validate_username(username: &str) -> bool {
        !username.is_empty()
            && username.len() <= 50
            && username
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    }

    /// Validate an editable display name (nickname). Allows spaces, Unicode and emoji;
    /// rejects empty/whitespace-only, over-long (by character count), and any string
    /// containing control characters (newlines, NUL, etc.). Returns the trimmed value.
    pub fn validate_display_name(name: &str) -> Option<String> {
        let trimmed = name.trim();
        if trimmed.is_empty()
            || trimmed.chars().count() > MAX_DISPLAY_NAME_CHARS
            || trimmed.chars().any(|c| c.is_control())
        {
            return None;
        }
        Some(trimmed.to_string())
    }

    pub fn validate_user_id(user_id: &str) -> bool {
        Uuid::parse_str(user_id).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_allows_spaces_unicode_and_emoji() {
        // The nickname is a human label, not an identifier, so it's far more permissive
        // than the login username (which stays alphanumeric).
        for name in ["老王 🐻", "Alice Smith", "José", "한글", "a b c"] {
            assert_eq!(
                User::validate_display_name(name).as_deref(),
                Some(name),
                "{name:?} should be accepted as-is"
            );
        }
    }

    #[test]
    fn display_name_is_trimmed() {
        assert_eq!(
            User::validate_display_name("  spaced out  ").as_deref(),
            Some("spaced out")
        );
    }

    #[test]
    fn display_name_rejects_empty_control_and_overlong() {
        assert_eq!(User::validate_display_name(""), None);
        assert_eq!(User::validate_display_name("   "), None);
        assert_eq!(User::validate_display_name("bad\nname"), None); // newline is a control char
        assert_eq!(User::validate_display_name("nul\0byte"), None);
        let too_long: String = "x".repeat(MAX_DISPLAY_NAME_CHARS + 1);
        assert_eq!(User::validate_display_name(&too_long), None);
        // Exactly at the limit (counted by chars, incl. multi-byte) is fine.
        let at_limit: String = "あ".repeat(MAX_DISPLAY_NAME_CHARS);
        assert!(User::validate_display_name(&at_limit).is_some());
    }

    #[test]
    fn display_name_is_never_serialized_into_the_user_body() {
        // The store uses positional bincode, so the nickname MUST stay out of the User
        // body — otherwise decoding a user.json written before the feature hits EOF (the
        // exact regression this guards). Two users differing only in display_name must
        // serialize byte-identically.
        let mut with = User::new("alice".to_string(), "pk".to_string());
        with.user_id = "fixed".to_string(); // pin the random UUID so only display_name differs
        let mut without = with.clone();
        with.display_name = "Alice 🐻".to_string();
        without.display_name = String::new();
        assert_eq!(
            bincode::serialize(&with).unwrap(),
            bincode::serialize(&without).unwrap(),
        );
    }

    #[test]
    fn effective_display_name_falls_back_to_username() {
        let mut user = User::new("alice".to_string(), "pk".to_string());
        assert_eq!(user.effective_display_name(), "alice");
        user.display_name = "Alice 🐻".to_string();
        assert_eq!(user.effective_display_name(), "Alice 🐻");
        // Whitespace-only display name is treated as unset.
        user.display_name = "   ".to_string();
        assert_eq!(user.effective_display_name(), "alice");
    }
}
