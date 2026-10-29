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

/// Exact device-local deletion facts in the profile database. No history-sized
/// in-memory set, expiry policy, or probabilistic membership structure is used.
pub(super) struct IndexedAccountSuppression {
    repository: super::transcript_repository::TranscriptRepository,
}
impl IndexedAccountSuppression {
    pub(super) fn from_profile(
        profile: std::sync::Arc<crate::storage::profile::context::ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        use super::transcript_repository::{SUPPRESSION, TranscriptRepository};
        let repository = TranscriptRepository::open(
            profile,
            path,
            password,
            SUPPRESSION,
            |c, path, password| {
                let writer = EncryptedRecordLog::<Suppressed>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |entry| {
                        Self::insert_on(c, entry.conversation, entry.id)?;
                        Ok(())
                    },
                )?;
                writer.sync()
            },
        )?;
        Ok(Self { repository })
    }
    fn insert_on(
        c: &rusqlite::Connection,
        conversation: ConversationId,
        id: EventId,
    ) -> Result<(), LogError> {
        let added = c.execute(
            "INSERT OR IGNORE INTO transcript_suppression VALUES(?1,?2)",
            rusqlite::params![conversation.as_bytes().as_slice(), id.as_bytes().as_slice()],
        )?;
        if added != 0 {
            c.execute(
                "UPDATE transcript_sources SET record_count=record_count+1 WHERE kind=?1",
                [super::transcript_repository::SUPPRESSION],
            )?;
        }
        Ok(())
    }
    pub(super) fn len(&self) -> Result<usize, LogError> {
        self.repository.count(None)
    }
    pub(super) fn marker_bytes(&self) -> Result<u64, LogError> {
        self.repository
            .usage()?
            .records
            .checked_mul(64)
            .ok_or_else(|| LogError::CorruptFile("suppression byte count overflow".into()))
    }
    pub(super) fn contains(
        &self,
        conversation: ConversationId,
        id: EventId,
    ) -> Result<bool, LogError> {
        self.repository.profile.database().read(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM transcript_suppression WHERE conversation=?1 AND id=?2)",rusqlite::params![conversation.as_bytes().as_slice(),id.as_bytes().as_slice()],|r|r.get(0))?))
    }
    pub(super) fn suppress(
        &mut self,
        conversation: ConversationId,
        id: EventId,
    ) -> Result<(), LogError> {
        self.repository.check_write()?;
        self.repository
            .profile
            .database()
            .transaction(|c| Self::insert_on(c, conversation, id))
    }
    /// Exact 64-byte keys in lexicographic order, at most 128 per owned page.
    pub(super) fn page(
        &self,
        after: Option<(ConversationId, EventId)>,
        limit: usize,
    ) -> Result<Vec<(ConversationId, EventId)>, LogError> {
        if limit == 0 || limit > super::transcript_repository::MAX_PAGE_ROWS {
            return Err(LogError::CorruptFile(
                "suppression page exceeds key budget".into(),
            ));
        }
        self.repository.profile.database().read(|c| {
            let seek=if after.is_some(){" WHERE (conversation,id)>(?1,?2)"}else{""};
            let mut statement=c.prepare(&format!("SELECT conversation,id FROM transcript_suppression{seek} ORDER BY conversation,id LIMIT ?3"))?;
            let mut rows=statement.query(rusqlite::params![after.as_ref().map(|(host,_)|host.as_bytes().as_slice()),after.as_ref().map(|(_,id)|id.as_bytes().as_slice()),limit as i64])?;
            let mut keys=Vec::new();
            while let Some(row)=rows.next()? {
                let key=|index|->Result<[u8;32],LogError>{row.get_ref(index)?.as_blob().map_err(|_|LogError::CorruptFile("invalid suppression key".into()))?.try_into().map_err(|_|LogError::CorruptFile("invalid suppression key width".into()))};
                keys.push((ConversationId::new(key(0)?),EventId::new(key(1)?)));
            }
            Ok(keys)
        })
    }
}
