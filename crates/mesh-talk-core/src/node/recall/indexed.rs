//! Exact device-local recall facts. They remain independently published history
//! authority and never expire when their message body is erased.
use super::{RECALL_LOG_MAGIC, RecallMark};
use crate::node::transcript_repository::{RECALL_MARKS, publication::PublishedHistorySource};
use crate::{
    eventlog::{
        LogError,
        event::{ConversationId, EventId},
    },
    storage::{
        profile::{context::ProfileStorage, manifest},
        record_log::EncryptedRecordLog,
    },
};
use rusqlite::{Connection, params};
use std::{path::Path, sync::Arc};
const SCHEMA: &str = "CREATE TABLE recall_marks(conversation BLOB NOT NULL CHECK(length(conversation)=32),target BLOB NOT NULL CHECK(length(target)=32),clock BLOB NOT NULL CHECK(length(clock)=8),PRIMARY KEY(conversation,target)) WITHOUT ROWID;";
pub(crate) struct IndexedRecallLog {
    source: PublishedHistorySource,
    revision: u64,
}
impl IndexedRecallLog {
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let registry: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",
                [],
                |r| r.get(0),
            )?;
            let imported = registry
                && c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM transcript_sources WHERE kind=?1)",
                    [RECALL_MARKS],
                    |r| r.get::<_, bool>(0),
                )?;
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='recall_marks')",
                [],
                |r| r.get(0),
            )?;
            if !present && !imported {
                c.execute_batch(SCHEMA)?;
            }
            manifest::validate_ddl(c, SCHEMA)
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            RECALL_MARKS,
            |c, path, password| {
                let log = EncryptedRecordLog::<RecallMark>::open_with_visitor(
                    path,
                    password,
                    RECALL_LOG_MAGIC,
                    |mark| Self::install_on(c, &mark).map(|_| ()),
                )?;
                log.sync()
            },
        )?;
        let revision = source.profile().database().read(|c| {
            Ok(c.query_row(
                "SELECT revision FROM transcript_sources WHERE kind=?1",
                [RECALL_MARKS],
                |r| r.get::<_, i64>(0),
            )?
            .try_into()
            .map_err(|_| LogError::CorruptFile("invalid history counter".into()))?)
        })?;
        Ok(Self { source, revision })
    }
    pub(crate) fn install_on(c: &Connection, mark: &RecallMark) -> Result<bool, LogError> {
        let inserted = c.execute(
            "INSERT OR IGNORE INTO recall_marks VALUES(?1,?2,?3)",
            params![
                mark.conversation.as_bytes().as_slice(),
                mark.target.as_bytes().as_slice(),
                mark.wall_clock.to_be_bytes().as_slice()
            ],
        )? != 0;
        if inserted {
            c.execute("UPDATE transcript_sources SET record_count=record_count+1,revision=revision+1 WHERE kind=?1",[RECALL_MARKS])?;
        }
        Ok(inserted)
    }
    pub(crate) fn check_write(&self) -> Result<(), LogError> {
        self.source.check_write()
    }
    pub(crate) fn note_committed(&mut self, inserted: usize) {
        self.revision = self.revision.saturating_add(inserted as u64);
    }
    pub(crate) fn record(
        &mut self,
        conversation: ConversationId,
        target: EventId,
        wall_clock: u64,
    ) -> Result<(), LogError> {
        self.source.check_write()?;
        let inserted = self.source.profile().database().transaction(|c| {
            Self::install_on(
                c,
                &RecallMark {
                    conversation,
                    target,
                    wall_clock,
                },
            )
        })?;
        self.revision += u64::from(inserted);
        Ok(())
    }
    pub(crate) fn contains(
        &self,
        conversation: ConversationId,
        target: EventId,
    ) -> Result<bool, LogError> {
        self.source.profile().database().read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM recall_marks WHERE conversation=?1 AND target=?2)",
                params![
                    conversation.as_bytes().as_slice(),
                    target.as_bytes().as_slice()
                ],
                |r| r.get(0),
            )?)
        })
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn len(&self) -> Result<u64, LogError> {
        self.source.profile().database().read(|c| {
            Ok(c.query_row(
                "SELECT record_count FROM transcript_sources WHERE kind=?1",
                [RECALL_MARKS],
                |r| r.get::<_, i64>(0),
            )?
            .try_into()
            .map_err(|_| LogError::CorruptFile("invalid history counter".into()))?)
        })
    }
    pub(crate) fn page(
        &self,
        conversation: Option<ConversationId>,
        after: Option<(ConversationId, EventId)>,
        limit: usize,
    ) -> Result<Vec<RecallMark>, LogError> {
        if !(1..=128).contains(&limit) {
            return Err(LogError::InvalidPageBudget);
        }
        self.source.profile().database().read(|c|{
            let scope=if conversation.is_some(){"conversation=?1 AND "}else{""};
            let query=format!("SELECT conversation,target,clock FROM recall_marks WHERE {scope}(conversation,target)>(?2,?3) ORDER BY conversation,target LIMIT ?4");
            let mut statement=c.prepare(&query)?;
            let mut rows=statement.query(params![conversation.map(|v|v.as_bytes().to_vec()),after.map_or(Vec::new(),|v|v.0.as_bytes().to_vec()),after.map_or(Vec::new(),|v|v.1.as_bytes().to_vec()),limit as i64])?;
            let mut result=Vec::new();
            while let Some(row)=rows.next()? {
                let bytes=|column|->Result<[u8;32],LogError>{row.get_ref(column)?.as_blob().map_err(|_|LogError::CorruptFile("invalid recall identifier".into()))?.try_into().map_err(|_|LogError::CorruptFile("invalid recall identifier width".into()))};
                let clock:[u8;8]=row.get_ref(2)?.as_blob().map_err(|_|LogError::CorruptFile("invalid recall clock".into()))?.try_into().map_err(|_|LogError::CorruptFile("invalid recall clock width".into()))?;
                result.push(RecallMark{conversation:ConversationId::new(bytes(0)?),target:EventId::new(bytes(1)?),wall_clock:u64::from_be_bytes(clock)});
            }Ok(result)
        })
    }
    pub(crate) fn visit(
        &self,
        mut visit: impl FnMut(RecallMark) -> Result<(), LogError>,
    ) -> Result<(), LogError> {
        let mut after = None;
        loop {
            let page = self.page(None, after, 128)?;
            if page.is_empty() {
                return Ok(());
            }
            for mark in page {
                after = Some((mark.conversation, mark.target));
                visit(mark)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    use crate::storage::profile::ProfileStorageLease;
    #[test]
    fn indexed_recall_migration_reopens_without_history_and_rolls_back_channel_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recalls.log");
        let conv = ConversationId::new([4; 32]);
        let mut legacy = super::super::RecallLog::open(&path, "pw").unwrap();
        for n in 0u64..260 {
            let mut key = [0; 32];
            key[24..].copy_from_slice(&n.to_be_bytes());
            legacy.record(conv, EventId::new(key), n).unwrap();
        }
        drop(legacy);
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let open = || {
            let profile = ProfileStorage::open_participant(
                ProfileStorageLease::acquire(dir.path()).unwrap(),
                &identity.public(),
                &account.public(),
                &dir.path().join("messages.log"),
                &dir.path().join("sent.log"),
                "pw",
            )
            .unwrap();
            profile.complete_participant("pw", None).unwrap();
            profile
        };
        let profile = open();
        let mut store = IndexedRecallLog::from_profile(profile.clone(), &path, "pw").unwrap();
        assert_eq!(store.len().unwrap(), 260);
        store.record(conv, EventId::new([0; 32]), 999).unwrap();
        assert_eq!(store.len().unwrap(), 260);
        assert_eq!(store.page(Some(conv), None, 1).unwrap()[0].wall_clock, 0);
        let missing = RecallMark {
            conversation: conv,
            target: EventId::new([9; 32]),
            wall_clock: 77,
        };
        let result: Result<(), LogError> = profile.database().transaction(|c| {
            IndexedRecallLog::install_on(c, &missing)?;
            Err(LogError::CorruptFile(
                "injected channel transaction rollback".into(),
            ))
        });
        assert!(result.is_err());
        assert!(!store.contains(conv, missing.target).unwrap());
        drop(store);
        drop(profile);
        crate::storage::record_log::frame_observations::reset();
        let store = IndexedRecallLog::from_profile(open(), &path, "pw").unwrap();
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
        assert_eq!(store.len().unwrap(), 260);
        let mut cursor = None;
        let mut count = 0;
        loop {
            let page = store.page(Some(conv), cursor, 128).unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 128);
            count += page.len();
            cursor = page.last().map(|m| (m.conversation, m.target));
        }
        assert_eq!(count, 260);
        assert!(
            !store
                .contains(ConversationId::new([5; 32]), EventId::new([0; 32]))
                .unwrap()
        );
        assert!(store.page(None, None, usize::MAX).is_err());
    }
}

#[cfg(test)]
impl IndexedRecallLog {
    pub(crate) fn targets(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut cursor = None;
        let mut result = Vec::new();
        loop {
            let page = self
                .page(Some(*conversation), cursor, 128)
                .expect("fixture recall marks");
            if page.is_empty() {
                return result;
            }
            cursor = page.last().map(|m| (m.conversation, m.target));
            result.extend(page.into_iter().map(|m| m.target));
        }
    }
}
