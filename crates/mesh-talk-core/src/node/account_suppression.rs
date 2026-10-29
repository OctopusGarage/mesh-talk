//! Local tombstones prevent sibling history catch-up from undoing an erase.

use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, EventId};
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTACSP";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
struct Suppressed {
    conversation: ConversationId,
    id: EventId,
}

pub(super) struct AccountSuppression {
    file: EncryptedRecordLog<Suppressed>,
    ids: HashSet<Suppressed>,
}

impl AccountSuppression {
    pub(super) fn len(&self) -> usize {
        self.ids.len()
    }

    /// Export exact local facts without cloning a second historical ID set.
    pub(super) fn records(&self) -> impl Iterator<Item = (ConversationId, EventId)> + '_ {
        self.ids.iter().map(|row| (row.conversation, row.id))
    }

    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::open(path, password, MAGIC)?;
        Ok(Self {
            file,
            ids: records.into_iter().collect(),
        })
    }

    pub(super) fn contains(&self, conversation: ConversationId, id: EventId) -> bool {
        self.ids.contains(&Suppressed { conversation, id })
    }

    pub(super) fn suppress(
        &mut self,
        conversation: ConversationId,
        id: EventId,
    ) -> Result<(), LogError> {
        let record = Suppressed { conversation, id };
        if self.ids.insert(record)
            && let Err(error) = self.file.append_durable(&record)
        {
            self.ids.remove(&record);
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppression_survives_reopen_and_failed_append_does_not_take_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suppression.log");
        let conversation = ConversationId::new([1; 32]);
        let erased = EventId::new([2; 32]);
        let failed = EventId::new([3; 32]);
        let mut store = AccountSuppression::open(&path, "pw").unwrap();
        store.suppress(conversation, erased).unwrap();
        store.file.fail_appends_for_test(true).unwrap();
        assert!(store.suppress(conversation, failed).is_err());
        assert!(!store.contains(conversation, failed));
        drop(store);
        let reopened = AccountSuppression::open(&path, "pw").unwrap();
        assert!(reopened.contains(conversation, erased));
        assert!(!reopened.contains(conversation, failed));
    }
}
