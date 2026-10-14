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
