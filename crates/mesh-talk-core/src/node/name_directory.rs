//! A local, password-encrypted directory of the display names peers have ANNOUNCED,
//! keyed by device `user_id` (and, when known, account id). It survives restart and,
//! crucially, outlives the live discovery roster — which evicts a peer ~30s after it
//! stops announcing (see `discovery::service::PEER_TTL`). Without this, a channel member
//! who goes offline (or hasn't re-announced since launch) loses their name and shows a raw
//! hex id; here we remember the last name we saw so members stay named while offline.
//!
//! On-disk framing is the shared [`EncryptedRecordLog`] (append-only). `record` is a no-op
//! when the name is unchanged, so a peer re-announcing every ~2s never grows the log — it
//! only appends on an actual rename. Never synced or served.

use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTNAME";

/// Defensive cap on a stored display name (the announce already bounds this; belt-and-braces
/// so a malformed record can't bloat the directory).
const MAX_NAME_BYTES: usize = 256;

/// One peer's last-known display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NameEntry {
    user_id: String,
    name: String,
}

/// The known-names directory: append-only on disk, collapsed to the latest name per
/// `user_id` in memory.
pub struct NameDirectory {
    file: EncryptedRecordLog<NameEntry>,
    by_user: HashMap<String, String>,
}

impl NameDirectory {
    /// Open (or create) the directory at `path`, collapsing stored records to the latest
    /// name per user (append order is write order, so the last record wins).
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, entries) = EncryptedRecordLog::<NameEntry>::open(path, password, MAGIC)?;
        let mut by_user = HashMap::new();
        for entry in entries {
            by_user.insert(entry.user_id, entry.name);
        }
        Ok(Self { file, by_user })
    }

    /// Record the name a peer announced. No-op (returns `false`, no write) when we already
    /// hold the same name for this `user_id`, so steady re-announces don't grow the log;
    /// appends + updates the index on a new name or a rename.
    pub fn record(&mut self, user_id: &str, name: &str) -> Result<bool, LogError> {
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Ok(false);
        }
        if matches!(self.by_user.get(user_id), Some(cur) if cur == name) {
            return Ok(false);
        }
        self.file.append(&NameEntry {
            user_id: user_id.to_string(),
            name: name.to_string(),
        })?;
        self.by_user.insert(user_id.to_string(), name.to_string());
        Ok(true)
    }

    /// The last-known display name for a device `user_id`, if we've ever seen it announce.
    pub fn name_for_user(&self, user_id: &str) -> Option<String> {
        self.by_user.get(user_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_resolves_when_not_in_live_roster_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names.log");
        {
            let mut d = NameDirectory::open(&path, "pw").unwrap();
            assert!(d.record("user-bob", "Bob").unwrap());
            // A steady re-announce of the same name is a no-op (doesn't grow the log).
            assert!(!d.record("user-bob", "Bob").unwrap());
            // A rename wins.
            assert!(d.record("user-bob", "Bobby").unwrap());
            assert_eq!(d.name_for_user("user-bob").as_deref(), Some("Bobby"));
        }
        // The whole point: after a restart (the live roster is empty), an offline member's
        // name still resolves from the durable directory.
        let d = NameDirectory::open(&path, "pw").unwrap();
        assert_eq!(d.name_for_user("user-bob").as_deref(), Some("Bobby"));
        assert_eq!(d.name_for_user("user-unknown"), None);
    }

    #[test]
    fn empty_or_oversize_names_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = NameDirectory::open(&dir.path().join("names.log"), "pw").unwrap();
        assert!(!d.record("u1", "").unwrap());
        assert!(!d.record("u2", &"x".repeat(MAX_NAME_BYTES + 1)).unwrap());
        assert_eq!(d.name_for_user("u1"), None);
        assert_eq!(d.name_for_user("u2"), None);
    }
}
