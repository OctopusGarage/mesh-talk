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
