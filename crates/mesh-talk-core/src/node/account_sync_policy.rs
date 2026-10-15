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
}

impl DeviceSyncPolicyStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, rows) = EncryptedRecordLog::open(path, password, MAGIC)?;
        Ok(Self {
            file,
            current: rows.last().copied().unwrap_or_default(),
        })
    }

    pub(super) fn current(&self) -> DeviceSyncPolicy {
        self.current
    }

    pub(super) fn set(&mut self, category: SyncCategory, enabled: bool) -> Result<(), LogError> {
        let mut next = self.current;
        next.set(category, enabled);
        if next != self.current {
            self.file.append_durable(&next)?;
            self.current = next;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
