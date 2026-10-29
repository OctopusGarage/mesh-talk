//! Original logical IDs for plaintext sent on a device retired by recovery.
//! The new device has different event sequences, so its normal seq-to-id map
//! cannot identify these archived sidecar rows after a restore or later rejoin.

use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, EventId};
use crate::node::sentlog::SentEntry;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTRSID";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct SentFingerprint {
    conversation: ConversationId,
    seq: u64,
    wall_clock: u64,
    plaintext_sha256: [u8; 32],
}

impl SentFingerprint {
    fn of(entry: &SentEntry) -> Self {
        Self {
            conversation: entry.conversation,
            seq: entry.seq,
            wall_clock: entry.wall_clock,
            plaintext_sha256: Sha256::digest(&entry.plaintext).into(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RecoveredSentId {
    fingerprint: SentFingerprint,
    id: EventId,
}

pub struct RecoveredSentIds {
    file: EncryptedRecordLog<RecoveredSentId>,
    ids: HashMap<SentFingerprint, EventId>,
}

impl RecoveredSentIds {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<RecoveredSentId>::open(path, password, MAGIC)?;
        let mut ids = HashMap::new();
        for record in records {
            if ids.insert(record.fingerprint, record.id).is_some() {
                return Err(LogError::CorruptFile("duplicate recovered sent id".into()));
            }
        }
        Ok(Self { file, ids })
    }

    pub fn record(&mut self, entry: &SentEntry, id: EventId) -> Result<(), LogError> {
        let fingerprint = SentFingerprint::of(entry);
        if let Some(existing) = self.ids.get(&fingerprint) {
            if *existing == id {
                return Ok(());
            }
            return Err(LogError::CorruptFile(
                "conflicting recovered sent id".into(),
            ));
        }
        self.file
            .append_durable(&RecoveredSentId { fingerprint, id })?;
        self.ids.insert(fingerprint, id);
        Ok(())
    }

    pub fn id_for(&self, entry: &SentEntry) -> Option<EventId> {
        self.ids.get(&SentFingerprint::of(entry)).copied()
    }
}

/// Exact recovered identity lookup on the participant's encrypted connection.
/// Legacy records remain readable only for authenticated source migration.
pub(crate) struct IndexedRecoveredSentIds {
    source: super::transcript_repository::publication::PublishedHistorySource,
}
const INDEX_SCHEMA: &str = "CREATE TABLE recovered_sent_ids(conversation BLOB NOT NULL CHECK(length(conversation)=32), sequence BLOB NOT NULL CHECK(length(sequence)=8), clock BLOB NOT NULL CHECK(length(clock)=8), plaintext_sha256 BLOB NOT NULL CHECK(length(plaintext_sha256)=32), id BLOB NOT NULL CHECK(length(id)=32), PRIMARY KEY(conversation,sequence,clock,plaintext_sha256)) WITHOUT ROWID;";
impl IndexedRecoveredSentIds {
    pub(crate) fn authenticate_legacy(
        profile: &crate::storage::profile::context::ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        super::transcript_repository::publication::PublishedHistorySource::authenticate_legacy::<
            RecoveredSentId,
        >(
            profile,
            path,
            password,
            super::transcript_repository::RECOVERED_IDS,
            MAGIC,
        )
    }

    pub(crate) fn from_profile(
        profile: std::sync::Arc<crate::storage::profile::context::ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        use super::transcript_repository::{RECOVERED_IDS, publication::PublishedHistorySource};
        profile
            .participant_database()
            .ok_or_else(|| {
                LogError::CorruptFile("recovered identity profile is not initialized".into())
            })?
            .transaction(|c| {
                let registry: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",
                    [],
                    |r| r.get(0),
                )?;
                let imported = registry
                    && c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM transcript_sources WHERE kind=?1)",
                        [RECOVERED_IDS],
                        |r| r.get::<_, bool>(0),
                    )?;
                let present: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='recovered_sent_ids')",
                    [],
                    |r| r.get(0),
                )?;
                if !present && !imported {
                    c.execute_batch(INDEX_SCHEMA)?;
                }
                crate::storage::profile::manifest::validate_ddl(c, INDEX_SCHEMA)
            })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            RECOVERED_IDS,
            |c, path, password| {
                let writer = EncryptedRecordLog::<RecoveredSentId>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |record| Self::insert_on(c, &record.fingerprint, record.id, true),
                )?;
                writer.sync()
            },
        )?;
        Ok(Self { source })
    }
    fn lookup_on(
        c: &rusqlite::Connection,
        fingerprint: &SentFingerprint,
    ) -> Result<Option<EventId>, LogError> {
        use rusqlite::OptionalExtension;
        let id:Option<Vec<u8>>=c.query_row("SELECT id FROM recovered_sent_ids WHERE conversation=?1 AND sequence=?2 AND clock=?3 AND plaintext_sha256=?4",rusqlite::params![fingerprint.conversation.as_bytes().as_slice(),fingerprint.seq.to_be_bytes().as_slice(),fingerprint.wall_clock.to_be_bytes().as_slice(),fingerprint.plaintext_sha256.as_slice()],|r|r.get(0)).optional()?;
        id.map(|id| {
            id.try_into()
                .map(EventId::new)
                .map_err(|_| LogError::CorruptFile("invalid recovered identity width".into()))
        })
        .transpose()
    }
    fn insert_on(
        c: &rusqlite::Connection,
        fingerprint: &SentFingerprint,
        id: EventId,
        importing: bool,
    ) -> Result<(), LogError> {
        if let Some(previous) = Self::lookup_on(c, fingerprint)? {
            if !importing && previous == id {
                return Ok(());
            }
            return Err(LogError::CorruptFile(
                "conflicting or duplicate recovered sent id".into(),
            ));
        }
        c.execute(
            "INSERT INTO recovered_sent_ids VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                fingerprint.conversation.as_bytes().as_slice(),
                fingerprint.seq.to_be_bytes().as_slice(),
                fingerprint.wall_clock.to_be_bytes().as_slice(),
                fingerprint.plaintext_sha256.as_slice(),
                id.as_bytes().as_slice()
            ],
        )?;
        c.execute("UPDATE transcript_sources SET record_count=record_count+1,revision=revision+1 WHERE kind=?1",[super::transcript_repository::RECOVERED_IDS])?;
        Ok(())
    }
    pub(crate) fn record(&mut self, entry: &SentEntry, id: EventId) -> Result<(), LogError> {
        self.source.check_write()?;
        let fingerprint = SentFingerprint::of(entry);
        self.source
            .profile()
            .database()
            .transaction(|c| Self::insert_on(c, &fingerprint, id, false))
    }
    pub(crate) fn id_for(&self, entry: &SentEntry) -> Result<Option<EventId>, LogError> {
        let fingerprint = SentFingerprint::of(entry);
        self.source
            .profile()
            .database()
            .read(|c| Self::lookup_on(c, &fingerprint))
    }
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.source.profile().database().read(|c| {
            Ok(c.query_row(
                "SELECT revision FROM transcript_sources WHERE kind=?1",
                [super::transcript_repository::RECOVERED_IDS],
                |r| r.get::<_, i64>(0),
            )?
            .try_into()
            .map_err(|_| LogError::CorruptFile("invalid history counter".into()))?)
        })
    }
    pub(crate) fn id_for_header(
        &self,
        header: &super::transcript_repository::TranscriptHeader,
    ) -> Result<Option<EventId>, LogError> {
        let fingerprint = SentFingerprint {
            conversation: header.conversation,
            seq: header.sequence,
            wall_clock: header.wall_clock,
            plaintext_sha256: header.plaintext_sha256,
        };
        self.source
            .profile()
            .database()
            .read(|c| Self::lookup_on(c, &fingerprint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovered_id_index_is_idempotent_and_rejects_conflicts_on_write_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery-sent-ids.log");
        let entry = SentEntry {
            conversation: ConversationId::new([3; 32]),
            seq: 7,
            wall_clock: 42,
            plaintext: b"original text".to_vec(),
        };
        let original = EventId::new([4; 32]);
        let conflicting = EventId::new([5; 32]);
        let mut index = RecoveredSentIds::open(&path, "password").unwrap();
        index.record(&entry, original).unwrap();
        index.record(&entry, original).unwrap();
        assert!(index.record(&entry, conflicting).is_err());
        drop(index);
        assert_eq!(
            RecoveredSentIds::open(&path, "password")
                .unwrap()
                .id_for(&entry),
            Some(original)
        );

        let (mut raw, _) =
            EncryptedRecordLog::<RecoveredSentId>::open(&path, "password", MAGIC).unwrap();
        raw.append_durable(&RecoveredSentId {
            fingerprint: SentFingerprint::of(&entry),
            id: conflicting,
        })
        .unwrap();
        drop(raw);
        assert!(RecoveredSentIds::open(&path, "password").is_err());
    }
}
