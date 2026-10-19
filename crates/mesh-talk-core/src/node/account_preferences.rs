//! Versioned, encrypted account preferences exchanged only with certified siblings.
//! Local sync choices remain in `DeviceSyncPolicyStore` and are never imported.

use super::account_sync_policy::SyncCategory;
use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

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
        let record = AccountPreference {
            category,
            key,
            value,
            revision: now_ms.max(
                old.as_ref()
                    .map_or(0, |entry| entry.revision)
                    .saturating_add(1),
            ),
            device_id,
        };
        self.merge(record.clone())?;
        Ok(record)
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

#[cfg(test)]
mod tests {
    use super::*;

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
