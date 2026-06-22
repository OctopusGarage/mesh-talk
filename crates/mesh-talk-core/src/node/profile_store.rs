//! A local, password-encrypted store of the avatars peers PROPAGATED to us, keyed by the
//! sender's account id. One current avatar per account (newest `updated_at` wins), so a
//! peer spamming profile updates can never grow this without bound. Survives restart.
//!
//! On-disk framing is the shared [`EncryptedRecordLog`] (append-only); on load and on
//! `record` we collapse to the newest record per account, so reads are O(1) and an old
//! update that arrived after a newer one never overwrites it. Never synced or served.

use crate::storage::record_log::EncryptedRecordLog;
use crate::{eventlog::LogError, node::profile::MAX_AVATAR_BYTES};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTPROF";

/// One account's current avatar, as last received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProfileEntry {
    account_id: String,
    updated_at: u64,
    /// The avatar data-URL bytes, or `None` if the peer CLEARED their avatar.
    avatar: Option<Vec<u8>>,
}

/// The received-profiles store: append-only on disk, collapsed to newest-per-account
/// in memory.
pub struct ProfileStore {
    file: EncryptedRecordLog<ProfileEntry>,
    by_account: HashMap<String, ProfileEntry>,
}

impl ProfileStore {
    /// Open (or create) the store at `path`, collapsing stored records to the newest
    /// per account.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, entries) = EncryptedRecordLog::<ProfileEntry>::open(path, password, MAGIC)?;
        let mut by_account: HashMap<String, ProfileEntry> = HashMap::new();
        for entry in entries {
            Self::merge(&mut by_account, entry);
        }
        Ok(Self { file, by_account })
    }

    /// Insert `entry` only if it is newer than what we hold (newest-wins, ties keep the
    /// existing one).
    fn merge(by_account: &mut HashMap<String, ProfileEntry>, entry: ProfileEntry) {
        match by_account.get(&entry.account_id) {
            Some(cur) if cur.updated_at >= entry.updated_at => {}
            _ => {
                by_account.insert(entry.account_id.clone(), entry);
            }
        }
    }

    /// Record a received avatar for `account_id`. Returns `true` if it was applied
    /// (strictly newer than what we held), `false` if it was a stale/duplicate update
    /// (in which case nothing is written). Oversized avatars are rejected (defensive;
    /// the payload signature check already bounds size).
    pub fn record(
        &mut self,
        account_id: &str,
        updated_at: u64,
        avatar: Option<Vec<u8>>,
    ) -> Result<bool, LogError> {
        if let Some(a) = &avatar {
            if a.len() > MAX_AVATAR_BYTES {
                return Ok(false);
            }
        }
        // Stale/duplicate? Don't write — keeps the log from growing on a replay.
        if matches!(self.by_account.get(account_id), Some(cur) if cur.updated_at >= updated_at) {
            return Ok(false);
        }
        let entry = ProfileEntry {
            account_id: account_id.to_string(),
            updated_at,
            avatar,
        };
        self.file.append(&entry)?;
        self.by_account.insert(account_id.to_string(), entry);
        Ok(true)
    }

    /// The current avatar for `account_id` (`None` if unknown OR the peer cleared it).
    pub fn avatar(&self, account_id: &str) -> Option<Vec<u8>> {
        self.by_account
            .get(account_id)
            .and_then(|e| e.avatar.clone())
    }

    /// Every account's current avatar, as `account_id -> data-URL bytes` (cleared
    /// avatars are omitted). For seeding the frontend on startup.
    pub fn all(&self) -> HashMap<String, Vec<u8>> {
        self.by_account
            .iter()
            .filter_map(|(id, e)| e.avatar.clone().map(|a| (id.clone(), a)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_wins_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.log");
        {
            let mut s = ProfileStore::open(&path, "pw").unwrap();
            assert!(s.record("alice", 100, Some(b"v1".to_vec())).unwrap());
            // A newer update wins.
            assert!(s.record("alice", 200, Some(b"v2".to_vec())).unwrap());
            // An older update is ignored (newest-wins).
            assert!(!s.record("alice", 150, Some(b"stale".to_vec())).unwrap());
            assert_eq!(s.avatar("alice"), Some(b"v2".to_vec()));
        }
        let s = ProfileStore::open(&path, "pw").unwrap();
        assert_eq!(s.avatar("alice"), Some(b"v2".to_vec()));
    }

    #[test]
    fn cleared_avatar_is_recorded_and_omitted_from_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.log");
        let mut s = ProfileStore::open(&path, "pw").unwrap();
        s.record("bob", 100, Some(b"photo".to_vec())).unwrap();
        // A newer CLEAR removes the avatar.
        s.record("bob", 200, None).unwrap();
        assert_eq!(s.avatar("bob"), None);
        assert!(!s.all().contains_key("bob"));
    }

    #[test]
    fn oversize_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.log");
        let mut s = ProfileStore::open(&path, "pw").unwrap();
        let big = vec![0u8; MAX_AVATAR_BYTES + 1];
        assert!(!s.record("eve", 100, Some(big)).unwrap());
        assert_eq!(s.avatar("eve"), None);
    }
}
