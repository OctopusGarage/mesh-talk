//! Versioned, encrypted account preferences exchanged only with certified siblings.
//! Local sync choices remain in `DeviceSyncPolicyStore` and are never imported.

use super::account_sync_policy::SyncCategory;
use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use zeroize::Zeroizing;

const MAGIC: &[u8; 6] = b"MTAPRF";
pub(crate) const MAX_KEYS: usize = 2048;
const MAX_VALUE: usize = 4 * 1024;
const MAX_KEY: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountPreference {
    pub category: SyncCategory,
    pub key: String,
    pub value: Option<String>,
    pub revision: u64,
    pub device_id: String,
}

impl AccountPreference {
    pub(crate) fn valid(&self) -> bool {
        matches!(
            self.category,
            SyncCategory::Appearance | SyncCategory::Contacts
        ) && !self.key.is_empty()
            && self.key.len() <= MAX_KEY
            && self
                .key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
            && self
                .value
                .as_ref()
                .is_none_or(|value| value.len() <= MAX_VALUE)
            && self.device_id.len() == 32
            && self.device_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            && self.revision > 0
    }

    fn order(&self) -> (u64, &str) {
        (self.revision, &self.device_id)
    }
}

pub(crate) struct AccountPreferences {
    file: EncryptedRecordLog<AccountPreference>,
    current: HashMap<(u8, String), AccountPreference>,
}

fn category_tag(category: SyncCategory) -> u8 {
    match category {
        SyncCategory::Appearance => 0,
        SyncCategory::Contacts => 1,
        _ => unreachable!("only account preferences are stored"),
    }
}

impl AccountPreferences {
    /// A local storage owner may adopt a different cryptographic account. Its
    /// previous account's rows must not be re-exported under the new identity.
    /// Legacy unscoped journals remain untouched: their rows do not contain an
    /// account binding, so assigning them after an adoption would guess ownership.
    pub(crate) fn open_for_account(
        directory: &Path,
        password: &str,
        account_id: &str,
    ) -> Result<Self, LogError> {
        if account_id.len() != 32 || !account_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(LogError::CorruptFile(
                "invalid preference account owner".into(),
            ));
        }
        let account_id = account_id.to_ascii_lowercase();
        // Bind decryption to the account as well as the filename. Renaming a
        // journal cannot turn another account's rows into this account's state.
        let scoped_password = Zeroizing::new(format!(
            "mesh-talk-account-preferences-v1:{account_id}:{password}"
        ));
        Self::open(
            &directory.join(format!("account-preferences-{account_id}.log")),
            &scoped_password,
        )
    }

    pub(crate) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<AccountPreference>::open(path, password, MAGIC)?;
        let mut current: HashMap<(u8, String), AccountPreference> = HashMap::new();
        for record in records {
            if !record.valid() {
                return Err(LogError::CorruptFile("invalid account preference".into()));
            }
            let key = (category_tag(record.category), record.key.clone());
            if !current.contains_key(&key) && current.len() >= MAX_KEYS {
                return Err(LogError::CorruptFile("too many account preferences".into()));
            }
            if current
                .get(&key)
                .is_none_or(|old| old.order() < record.order())
            {
                current.insert(key, record);
            }
        }
        Ok(Self { file, current })
    }

    pub(crate) fn get(&self, category: SyncCategory, key: &str) -> Option<AccountPreference> {
        if !matches!(category, SyncCategory::Appearance | SyncCategory::Contacts) {
            return None;
        }
        self.current
            .get(&(category_tag(category), key.to_string()))
            .cloned()
    }

    pub(crate) fn entries(&self, category: SyncCategory) -> Vec<AccountPreference> {
        if !matches!(category, SyncCategory::Appearance | SyncCategory::Contacts) {
            return Vec::new();
        }
        self.current
            .values()
            .filter(|entry| entry.category == category)
            .cloned()
            .collect()
    }

    pub(crate) fn set_local(
        &mut self,
        category: SyncCategory,
        key: String,
        value: Option<String>,
        device_id: String,
        now_ms: u64,
    ) -> Result<AccountPreference, LogError> {
        let old = self.get(category, &key);
        let next_revision = old
            .as_ref()
            .map_or(0, |entry| entry.revision)
            .checked_add(1)
            .ok_or_else(|| LogError::CorruptFile("account preference revision exhausted".into()))?;
        let record = AccountPreference {
            category,
            key,
            value,
            revision: now_ms.max(next_revision),
            device_id,
        };
        self.merge(record.clone())?;
        Ok(record)
    }

    fn seed_local(
        &mut self,
        category: SyncCategory,
        key: String,
        value: Option<String>,
        device_id: String,
        now_ms: u64,
    ) -> Result<(AccountPreference, bool), LogError> {
        if let Some(existing) = self.get(category, &key) {
            return Ok((existing, false));
        }
        self.set_local(category, key, value, device_id, now_ms)
            .map(|record| (record, true))
    }

    pub(crate) fn merge(&mut self, record: AccountPreference) -> Result<bool, LogError> {
        if !record.valid() {
            return Err(LogError::CorruptFile("invalid account preference".into()));
        }
        let key = (category_tag(record.category), record.key.clone());
        if !self.current.contains_key(&key) && self.current.len() >= MAX_KEYS {
            return Err(LogError::CorruptFile("too many account preferences".into()));
        }
        if self
            .current
            .get(&key)
            .is_some_and(|old| old.order() >= record.order())
        {
            return Ok(false);
        }
        self.file.append_durable(&record)?;
        self.current.insert(key, record);
        Ok(true)
    }
}

impl super::Node {
    /// Seed a saved local choice without replacing any account value, including
    /// a tombstone received concurrently with the caller's earlier snapshot.
    pub fn set_account_preference_if_absent(
        &self,
        category: SyncCategory,
        key: String,
        value: Option<String>,
    ) -> Result<AccountPreference, super::NodeError> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |duration| {
                duration.as_millis().min(u64::MAX as u128) as u64
            });
        let (record, created) = self
            .account_preferences
            .lock()
            .expect("account preferences mutex not poisoned")
            .seed_local(category, key, value, self.user_id(), now_ms)
            .map_err(super::NodeError::Log)?;
        if created {
            if self.device_sync_policy().enabled(category) {
                self.account_sync_notify.notify_one();
            }
            self.note_ui_change();
            self.note_account_local_change();
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_seed_preserves_existing_tombstone_and_record_arriving_after_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preferences.log");
        let mut store = AccountPreferences::open(&path, "pw").unwrap();
        assert!(store.entries(SyncCategory::Appearance).is_empty());
        let existing = store
            .set_local(
                SyncCategory::Appearance,
                "theme".into(),
                None,
                "ab".repeat(16),
                10,
            )
            .unwrap();
        let (seeded, created) = store
            .seed_local(
                SyncCategory::Appearance,
                "theme".into(),
                Some("dark".into()),
                "cd".repeat(16),
                20,
            )
            .unwrap();
        assert!(!created);
        assert_eq!(seeded, existing);
        let (first, created) = store
            .seed_local(
                SyncCategory::Appearance,
                "settings.ringtone".into(),
                Some("\"marimba\"".into()),
                "ab".repeat(16),
                30,
            )
            .unwrap();
        assert!(created);
        let (second, created) = store
            .seed_local(
                SyncCategory::Appearance,
                "settings.ringtone".into(),
                Some("\"digital\"".into()),
                "cd".repeat(16),
                40,
            )
            .unwrap();
        assert!(!created);
        assert_eq!(first, second);
        drop(store);
        let reopened = AccountPreferences::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.get(SyncCategory::Appearance, "theme"),
            Some(existing)
        );
        assert_eq!(
            reopened.get(SyncCategory::Appearance, "settings.ringtone"),
            Some(first)
        );
    }

    #[test]
    fn account_scoped_preferences_never_import_unattributed_legacy_or_another_account() {
        let root = tempfile::tempdir().unwrap();
        let legacy_path = root.path().join("account-preferences.log");
        let mut legacy = AccountPreferences::open(&legacy_path, "pw").unwrap();
        legacy
            .set_local(
                SyncCategory::Contacts,
                "favorite:old".into(),
                Some("old alias".into()),
                "aa".repeat(16),
                1,
            )
            .unwrap();
        drop(legacy);
        let legacy_bytes = std::fs::read(&legacy_path).unwrap();
        let a = "11".repeat(16);
        let b = "22".repeat(16);
        let mut first = AccountPreferences::open_for_account(root.path(), "pw", &a).unwrap();
        assert!(
            first.entries(SyncCategory::Contacts).is_empty(),
            "legacy rows have no provable account owner"
        );
        first
            .set_local(
                SyncCategory::Contacts,
                "favorite:current".into(),
                Some("Mac remark".into()),
                "aa".repeat(16),
                2,
            )
            .unwrap();
        drop(first);
        let other = AccountPreferences::open_for_account(root.path(), "pw", &b).unwrap();
        assert!(other.entries(SyncCategory::Contacts).is_empty());
        drop(other);
        let reopened = AccountPreferences::open_for_account(root.path(), "pw", &a).unwrap();
        assert_eq!(
            reopened
                .get(SyncCategory::Contacts, "favorite:current")
                .unwrap()
                .value
                .as_deref(),
            Some("Mac remark")
        );
        assert_eq!(
            std::fs::read(legacy_path).unwrap(),
            legacy_bytes,
            "ambiguous legacy data is preserved for recovery"
        );
    }

    #[test]
    fn account_scoped_preferences_reject_renamed_ciphertext_and_invalid_owner() {
        let root = tempfile::tempdir().unwrap();
        let a = "11".repeat(16);
        let b = "22".repeat(16);
        let mut first = AccountPreferences::open_for_account(root.path(), "pw", &a).unwrap();
        first
            .set_local(
                SyncCategory::Appearance,
                "theme".into(),
                Some("dark".into()),
                "aa".repeat(16),
                1,
            )
            .unwrap();
        drop(first);
        std::fs::copy(
            root.path().join(format!("account-preferences-{a}.log")),
            root.path().join(format!("account-preferences-{b}.log")),
        )
        .unwrap();
        assert!(AccountPreferences::open_for_account(root.path(), "pw", &b).is_err());
        assert!(AccountPreferences::open_for_account(root.path(), "pw", "../escape").is_err());
    }

    #[test]
    fn exhausted_revision_rejects_local_edit_without_false_success() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AccountPreferences::open(&dir.path().join("prefs.log"), "pw").unwrap();
        let original = AccountPreference {
            category: SyncCategory::Appearance,
            key: "theme".into(),
            value: Some("dark".into()),
            revision: u64::MAX,
            device_id: "ab".repeat(16),
        };
        assert!(store.merge(original.clone()).unwrap());
        assert!(
            store
                .set_local(
                    SyncCategory::Appearance,
                    "theme".into(),
                    Some("light".into()),
                    original.device_id.clone(),
                    1
                )
                .is_err()
        );
        assert_eq!(store.get(SyncCategory::Appearance, "theme"), Some(original));
    }

    #[test]
    fn newer_preference_and_clear_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.log");
        let device = "ab".repeat(16);
        let mut store = AccountPreferences::open(&path, "pw").unwrap();
        let first = store
            .set_local(
                SyncCategory::Appearance,
                "theme".into(),
                Some("dark".into()),
                device.clone(),
                1,
            )
            .unwrap();
        let mut remote = first.clone();
        remote.value = Some("light".into());
        remote.revision += 1;
        assert!(store.merge(remote.clone()).unwrap());
        assert!(!store.merge(first).unwrap());
        store
            .set_local(SyncCategory::Appearance, "theme".into(), None, device, 1)
            .unwrap();
        drop(store);
        let reopened = AccountPreferences::open(&path, "pw").unwrap();
        assert_eq!(
            reopened
                .get(SyncCategory::Appearance, "theme")
                .unwrap()
                .value,
            None
        );
    }
}
