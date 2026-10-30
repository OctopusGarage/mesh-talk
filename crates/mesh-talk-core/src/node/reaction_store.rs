//! Durable projection of locally authored reactions that only peers can decrypt.

use super::reaction::ReactionPayload;
#[cfg(test)]
use crate::eventlog::LogError;
use crate::eventlog::event::ConversationId;
#[cfg(test)]
use crate::eventlog::event::EventId;
#[cfg(test)]
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
type ReactionKey = (ConversationId, EventId, String);
#[cfg(test)]
type ReactionValue = (u64, ReactionPayload);
#[cfg(test)]
type Entries = HashMap<ReactionKey, ReactionValue>;
const MAGIC: &[u8; 6] = b"MTREAC";

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct Record {
    pub conversation: ConversationId,
    pub wall: u64,
    pub payload: ReactionPayload,
}

#[cfg(test)]
pub(super) struct ReactionStore {
    file: EncryptedRecordLog<Record>,
    entries: Entries,
    records: usize,
}

#[cfg(test)]
impl ReactionStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::open(path, password, MAGIC)?;
        let mut store = Self {
            file,
            entries: HashMap::new(),
            records: records.len(),
        };
        for record in records {
            store.merge(record);
        }
        Ok(store)
    }

    fn merge(&mut self, record: Record) {
        let key = (
            record.conversation,
            record.payload.target,
            record.payload.emoji.clone(),
        );
        if self.entries.get(&key).is_none_or(|(wall, _)| {
            record.wall > *wall || (record.wall == *wall && record.payload.remove)
        }) {
            self.entries.insert(key, (record.wall, record.payload));
        }
    }

    pub(super) fn record(
        &mut self,
        conversation: ConversationId,
        wall: u64,
        payload: ReactionPayload,
    ) -> Result<(), LogError> {
        let key = (conversation, payload.target, payload.emoji.clone());
        if self.entries.get(&key).is_some_and(|(current, old)| {
            wall < *current || (wall == *current && (old.remove || !payload.remove))
        }) {
            return Ok(());
        }
        // Compact before accepting the next action, so rewrite failure cannot
        // turn a failed call into a newly visible reaction.
        if self.records >= self.entries.len().saturating_mul(2).saturating_add(64) {
            let current = self.records_kept(|_, _, _| false);
            self.file.rewrite(&current)?;
            self.records = current.len();
        }
        let record = Record {
            conversation,
            wall,
            payload,
        };
        self.file.append_durable(&record)?;
        self.records += 1;
        self.merge(record);
        Ok(())
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&ReactionKey, &ReactionValue)> {
        self.entries.iter()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    fn records_kept(
        &self,
        remove: impl Fn(ConversationId, u64, &ReactionPayload) -> bool,
    ) -> Vec<Record> {
        self.entries
            .iter()
            .filter(|((conv, _, _), (wall, payload))| !remove(*conv, *wall, payload))
            .map(|((conv, _, _), (wall, payload))| Record {
                conversation: *conv,
                wall: *wall,
                payload: payload.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_projection_compacts_and_preserves_remove_ties() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reactions.log");
        let conv = ConversationId::new([1; 32]);
        let payload = |remove| ReactionPayload {
            target: EventId::new([2; 32]),
            emoji: "👍".into(),
            remove,
        };
        let mut store = ReactionStore::open(&path, "pw").unwrap();
        for wall in 0..200 {
            store.record(conv, wall, payload(wall % 2 == 1)).unwrap();
        }
        store.record(conv, 199, payload(false)).unwrap();
        drop(store);
        let (_, records) = EncryptedRecordLog::<Record>::open(&path, "pw", MAGIC).unwrap();
        assert!(records.len() < 70, "toggle history is compacted");
        let reopened = ReactionStore::open(&path, "pw").unwrap();
        assert_eq!(reopened.len(), 1);
        assert!(
            reopened.iter().next().unwrap().1.1.remove,
            "equal-time add cannot resurrect remove"
        );
    }
}

mod indexed;
pub(in crate::node) use indexed::IndexedReactionStore;
