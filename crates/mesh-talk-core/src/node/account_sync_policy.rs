//! Device-local choices for which account data this node shares and imports.

use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTACPL";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncCategory {
    History,
    Attachments,
    Profile,
    Appearance,
    Contacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceSyncPolicy {
    pub history: bool,
    pub attachments: bool,
    pub profile: bool,
    pub appearance: bool,
    pub contacts: bool,
}

impl Default for DeviceSyncPolicy {
    fn default() -> Self {
        Self {
            history: true,
            attachments: false,
            profile: true,
            appearance: false,
            contacts: false,
        }
    }
}

impl DeviceSyncPolicy {
    pub fn enabled(self, category: SyncCategory) -> bool {
        match category {
            SyncCategory::History => self.history,
            SyncCategory::Attachments => self.attachments,
            SyncCategory::Profile => self.profile,
            SyncCategory::Appearance => self.appearance,
            SyncCategory::Contacts => self.contacts,
        }
    }

    fn set(&mut self, category: SyncCategory, enabled: bool) {
        match category {
            SyncCategory::History => self.history = enabled,
            SyncCategory::Attachments => self.attachments = enabled,
            SyncCategory::Profile => self.profile = enabled,
            SyncCategory::Appearance => self.appearance = enabled,
            SyncCategory::Contacts => self.contacts = enabled,
        }
    }
}

pub(super) struct DeviceSyncPolicyStore {
    file: EncryptedRecordLog<DeviceSyncPolicy>,
    current: DeviceSyncPolicy,
    // Process-local invalidation only; persisted rows and wire policy stay unchanged.
    revision: u64,
}

impl DeviceSyncPolicyStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, rows) = EncryptedRecordLog::open(path, password, MAGIC)?;
        Ok(Self {
            file,
            current: rows.last().copied().unwrap_or_default(),
            revision: 0,
        })
    }

    pub(super) fn current(&self) -> DeviceSyncPolicy {
        self.current
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn set(&mut self, category: SyncCategory, enabled: bool) -> Result<(), LogError> {
        let mut next = self.current;
        next.set(category, enabled);
        if next != self.current {
            self.file.append_durable(&next)?;
            self.current = next;
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_successful_policy_changes_invalidate_in_flight_sync() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DeviceSyncPolicyStore::open(&dir.path().join("policy"), "pw").unwrap();
        let original = store.current();
        let revision = store.revision();
        store.file.fail_appends_for_test(true).unwrap();
        assert!(store.set(SyncCategory::Appearance, true).is_err());
        assert_eq!(store.current(), original);
        assert_eq!(store.revision(), revision);
        store.file.fail_appends_for_test(false).unwrap();
        store.set(SyncCategory::Appearance, false).unwrap();
        assert_eq!(store.revision(), revision, "unchanged category is a no-op");
        store.set(SyncCategory::Appearance, true).unwrap();
        store.set(SyncCategory::Appearance, false).unwrap();
        assert_eq!(store.current(), original);
        assert_ne!(
            store.revision(),
            revision,
            "off/on ABA still invalidates sync"
        );
    }

    #[test]
    fn policy_is_local_and_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.log");
        let b = dir.path().join("b.log");
        let mut first = DeviceSyncPolicyStore::open(&a, "pw").unwrap();
        let second = DeviceSyncPolicyStore::open(&b, "pw").unwrap();
        first.set(SyncCategory::History, false).unwrap();
        assert!(!first.current().history);
        assert!(second.current().history);
        drop(first);
        assert!(
            !DeviceSyncPolicyStore::open(&a, "pw")
                .unwrap()
                .current()
                .history
        );
    }
}
