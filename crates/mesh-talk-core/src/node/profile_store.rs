//! Current compatibility avatars, selected from the participant's encrypted index.
//! The legacy encrypted record writer remains available to migration fixtures;
//! live readers use bounded pages or one account lookup and preserve clear ties.

#[cfg(test)]
use crate::storage::record_log::EncryptedRecordLog;
#[cfg(test)]
use crate::{eventlog::LogError, node::profile::MAX_AVATAR_BYTES};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;

mod indexed;
#[cfg(test)]
pub(crate) fn reset_indexed_body_reads() {
    indexed::reset_body_reads();
}
#[cfg(test)]
pub(crate) fn indexed_body_reads() -> usize {
    indexed::body_reads()
}

pub(super) use indexed::IndexedProfileStore;

const MAGIC: &[u8; 6] = b"MTPROF";

/// One account's current avatar, as last received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct ProfileEntry {
    pub(in crate::node) account_id: String,
    pub(in crate::node) updated_at: u64,
    /// The avatar data-URL bytes, or `None` if the peer CLEARED their avatar.
    pub(in crate::node) avatar: Option<Vec<u8>>,
}

fn avatar_order(revision: u64, avatar: &Option<Vec<u8>>) -> (u64, bool, Option<&[u8]>) {
    (revision, avatar.is_none(), avatar.as_deref())
}

/// The received-profiles store: append-only on disk, collapsed to newest-per-account
/// in memory.
#[cfg(test)]
pub struct ProfileStore {
    file: EncryptedRecordLog<ProfileEntry>,
    by_account: HashMap<String, ProfileEntry>,
    records: usize,
    revision: u64,
}

pub(super) struct AvatarPage {
    pub rows: Vec<(String, Option<Vec<u8>>)>,
    pub after: Option<String>,
    pub complete: bool,
    pub revision: u64,
}

#[cfg(test)]
impl ProfileStore {
    /// Open (or create) the store at `path`, collapsing stored records to the newest
    /// per account.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, entries) = EncryptedRecordLog::<ProfileEntry>::open(path, password, MAGIC)?;
        let records = entries.len();
        let mut by_account: HashMap<String, ProfileEntry> = HashMap::new();
        for entry in entries {
            Self::merge(&mut by_account, entry);
        }
        let mut store = Self {
            file,
            by_account,
            records,
            revision: records as u64,
        };
        // Upgrade older append-only journals, preserving clear tombstones.
        if store.records > store.by_account.len() {
            store.compact()?;
        }
        Ok(store)
    }

    /// Signed revisions order profiles; equal revisions use a deterministic
    /// content order, with clear tombstones winning so replay cannot resurrect them.
    ///
    /// Use the same total order during journal replay and live admission.
    fn merge(by_account: &mut HashMap<String, ProfileEntry>, entry: ProfileEntry) {
        match by_account.get(&entry.account_id) {
            Some(cur)
                if avatar_order(cur.updated_at, &cur.avatar)
                    >= avatar_order(entry.updated_at, &entry.avatar) => {}
            _ => {
                by_account.insert(entry.account_id.clone(), entry);
            }
        }
    }

    /// Record a received avatar for `account_id`. Returns `true` if it was applied
    /// (greater in the revision/content order), `false` for a stale/duplicate update
    /// (in which case nothing is written). Oversized avatars are rejected (defensive;
    /// the payload signature check already bounds size).
    pub fn record(
        &mut self,
        account_id: &str,
        updated_at: u64,
        avatar: Option<Vec<u8>>,
    ) -> Result<bool, LogError> {
        if let Some(a) = &avatar
            && a.len() > MAX_AVATAR_BYTES
        {
            return Ok(false);
        }
        // Stale/duplicate? Don't write — keeps the log from growing on a replay.
        if matches!(self.by_account.get(account_id), Some(cur) if avatar_order(cur.updated_at, &cur.avatar) >= avatar_order(updated_at, &avatar))
        {
            return Ok(false);
        }
        let entry = ProfileEntry {
            account_id: account_id.to_string(),
            updated_at,
            avatar,
        };
        // Reclaim obsolete versions before publishing the next update, so a
        // failed rewrite cannot make an unsuccessful update visible in memory.
        if self.records >= self.by_account.len().saturating_mul(2).saturating_add(64) {
            self.compact()?;
        }
        self.file.append_durable(&entry)?;
        self.records += 1;
        self.revision = self.revision.wrapping_add(1);
        self.by_account.insert(account_id.to_string(), entry);
        Ok(true)
    }

    fn compact(&mut self) -> Result<(), LogError> {
        let mut current: Vec<_> = self.by_account.values().cloned().collect();
        current.sort_by(|a, b| a.account_id.cmp(&b.account_id));
        self.file.rewrite(&current)?;
        self.records = current.len();
        Ok(())
    }

    /// The current avatar for `account_id` (`None` if unknown OR the peer cleared it).
    pub fn avatar(&self, account_id: &str) -> Option<Vec<u8>> {
        self.by_account
            .get(account_id)
            .and_then(|e| e.avatar.clone())
    }

    pub fn updated_at(&self, account_id: &str) -> Option<u64> {
        self.by_account
            .get(account_id)
            .map(|entry| entry.updated_at)
    }

    /// Every account's current avatar, as `account_id -> data-URL bytes` (cleared
    /// avatars are omitted). For seeding the frontend on startup.
    pub fn all(&self) -> HashMap<String, Vec<u8>> {
        self.by_account
            .iter()
            .filter_map(|(id, e)| e.avatar.clone().map(|a| (id.clone(), a)))
            .collect()
    }
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }
    /// Select only bounded key references before cloning current avatar bytes.
    /// Obsolete profile versions never enter this presentation page.
    pub(super) fn page(&self, after: Option<&str>) -> Result<AvatarPage, LogError> {
        const ROWS: usize = 16;
        const BYTES: usize = 1024 * 1024;
        let mut selected = std::collections::BTreeMap::new();
        let mut candidates = 0usize;
        for (id, entry) in &self.by_account {
            if after.is_some_and(|after| id.as_str() <= after) {
                continue;
            }
            candidates += 1;
            selected.insert(id.as_str(), entry);
            if selected.len() > ROWS {
                selected.pop_last();
            }
        }
        let mut rows = Vec::new();
        let mut charged = 0;
        let mut cursor = after.map(str::to_owned);
        for (id, entry) in selected {
            let charge = id
                .len()
                .saturating_add(entry.avatar.as_ref().map_or(0, Vec::len));
            if charge > BYTES - charged {
                if rows.is_empty() {
                    return Err(LogError::PageBudgetTooSmall { required: charge });
                }
                break;
            }
            charged += charge;
            cursor = Some(id.to_owned());
            rows.push((id.to_owned(), entry.avatar.clone()));
        }
        Ok(AvatarPage {
            complete: rows.len() == candidates,
            rows,
            after: cursor,
            revision: self.revision,
        })
    }

    /// Current profile state including clear tombstones and their timestamps.
    pub(crate) fn snapshot_entries(&self) -> Vec<(String, u64, Option<Vec<u8>>)> {
        let mut entries: Vec<_> = self
            .by_account
            .values()
            .map(|entry| {
                (
                    entry.account_id.clone(),
                    entry.updated_at,
                    entry.avatar.clone(),
                )
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }
}

#[cfg(test)]
mod tests {
    pub(super) fn profile(
        root: &std::path::Path,
        identity: &crate::identity::device::DeviceIdentity,
        account: &Account,
    ) -> std::sync::Arc<crate::storage::profile::context::ProfileStorage> {
        let profile = crate::storage::profile::context::ProfileStorage::open_participant(
            crate::storage::profile::ProfileStorageLease::acquire(root).unwrap(),
            &identity.public(),
            &account.public(),
            &root.join("messages.log"),
            &root.join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        profile
    }
    #[test]
    fn warm_open_reads_no_avatar_payloads() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("avatars");
        let mut store = ProfileStore::open(&path, "pw").unwrap();
        for n in 0..64 {
            store
                .record(&format!("{n:032x}"), 1, Some(vec![7; 8192]))
                .unwrap();
        }
        drop(store);
        let identity = crate::identity::device::DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(root.path(), &identity, &account);
        let imported = IndexedProfileStore::from_profile(owner.clone(), &path, "pw").unwrap();
        drop(imported);
        drop(owner);
        indexed::reset_body_reads();
        crate::storage::record_log::frame_observations::reset();
        let owner = profile(root.path(), &identity, &account);
        let store = IndexedProfileStore::from_profile(owner, &path, "pw").unwrap();
        assert_eq!(indexed::body_reads(), 0);
        let page = store.page(None).unwrap();
        assert_eq!(page.rows.len(), 16);
        assert_eq!(indexed::body_reads(), 16);
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0,
            "warm current-avatar open must not decrypt every account payload"
        );
    }

    use super::*;
    use crate::identity::account::Account;

    #[test]
    fn current_pages_bound_rows_and_bytes_and_include_clear_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::open(&dir.path().join("profiles"), "pw").unwrap();
        for n in 0..40 {
            store
                .record(
                    &format!("account-{n:02}"),
                    1,
                    Some(vec![42; MAX_AVATAR_BYTES]),
                )
                .unwrap();
        }
        store.record("account-05", 2, None).unwrap();
        let revision = store.revision();
        let mut after = None;
        let mut observed = Vec::new();
        loop {
            let page = store.page(after.as_deref()).unwrap();
            assert!(page.rows.len() <= 16);
            assert!(
                page.rows
                    .iter()
                    .map(|(id, avatar)| id.len() + avatar.as_ref().map_or(0, Vec::len))
                    .sum::<usize>()
                    <= 1024 * 1024
            );
            assert_eq!(page.revision, revision);
            observed.extend(page.rows);
            if page.complete {
                break;
            }
            assert_ne!(page.after, after);
            after = page.after;
        }
        assert_eq!(observed.len(), 40);
        assert_eq!(observed[5], ("account-05".into(), None));
        assert!(observed.windows(2).all(|rows| rows[0].0 < rows[1].0));
        assert!(!store.record("account-05", 1, Some(vec![1])).unwrap());
        assert_eq!(store.revision(), revision);
    }

    #[test]
    fn repeated_avatar_updates_have_bounded_journal_and_keep_clear_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.log");
        let mut store = ProfileStore::open(&path, "pw").unwrap();
        for revision in 1..=200 {
            store
                .record("alice", revision, Some(vec![42; 1024]))
                .unwrap();
        }
        store.record("bob", 1, Some(b"keep".to_vec())).unwrap();
        store.record("alice", 201, None).unwrap();
        drop(store);
        let (_, records) = EncryptedRecordLog::<ProfileEntry>::open(&path, "pw", MAGIC).unwrap();
        assert!(records.len() < 100, "obsolete avatars must be reclaimed");
        let mut reopened = ProfileStore::open(&path, "pw").unwrap();
        assert_eq!(reopened.avatar("bob"), Some(b"keep".to_vec()));
        assert_eq!(reopened.updated_at("alice"), Some(201));
        assert!(
            !reopened
                .record("alice", 200, Some(b"stale".to_vec()))
                .unwrap()
        );
    }

    #[test]
    fn opening_legacy_avatar_journal_reclaims_obsolete_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.log");
        let (mut file, _) = EncryptedRecordLog::<ProfileEntry>::open(&path, "pw", MAGIC).unwrap();
        for revision in 1..=100 {
            file.append(&ProfileEntry {
                account_id: "alice".into(),
                updated_at: revision,
                avatar: None,
            })
            .unwrap();
        }
        drop(file);
        let store = ProfileStore::open(&path, "pw").unwrap();
        assert_eq!(store.updated_at("alice"), Some(100));
        drop(store);
        let (_, records) = EncryptedRecordLog::<ProfileEntry>::open(&path, "pw", MAGIC).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn failed_avatar_compaction_does_not_publish_the_next_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.log");
        let mut store = ProfileStore::open(&path, "pw").unwrap();
        for revision in 1..=66 {
            store.record("alice", revision, None).unwrap();
        }
        std::fs::create_dir(path.with_extension("compact-tmp")).unwrap();
        assert!(store.record("alice", 67, Some(b"new".to_vec())).is_err());
        assert_eq!(store.updated_at("alice"), Some(66));
        std::fs::remove_dir(path.with_extension("compact-tmp")).unwrap();
        assert!(store.record("alice", 67, Some(b"new".to_vec())).unwrap());
    }

    #[test]
    fn equal_revision_conflicts_converge_in_both_orders_and_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let variants = [None, Some(b"first".to_vec()), Some(b"second".to_vec())];
        for (index, reverse) in [false, true].into_iter().enumerate() {
            let path = dir.path().join(format!("profiles-{index}.log"));
            let mut store = ProfileStore::open(&path, "pw").unwrap();
            let mut updates = variants.clone();
            if reverse {
                updates.reverse();
            }
            for avatar in updates {
                store.record("alice", 100, avatar).unwrap();
            }
            // Clear wins equal revisions; a replay cannot resurrect an avatar.
            assert_eq!(store.avatar("alice"), None);
            drop(store);
            let reopened = ProfileStore::open(&path, "pw").unwrap();
            assert_eq!(reopened.avatar("alice"), None);
            assert_eq!(reopened.updated_at("alice"), Some(100));
        }
    }

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
        assert_eq!(s.snapshot_entries(), vec![("bob".to_string(), 200, None)]);
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
