//! Locally encrypted evidence that an authenticated channel member reported durable
//! possession of an exact event. These are historical custody confirmations, not
//! delivery or read receipts; a peer can later delete its copy or lose its device.

use crate::eventlog::LogError;
use crate::eventlog::event::EventId;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTCUST";

#[derive(Clone, Serialize, Deserialize)]
struct CustodyRecord {
    event: EventId,
    peer: String,
}

pub struct ChannelCustodyStore {
    file: EncryptedRecordLog<CustodyRecord>,
    confirmed: HashMap<EventId, HashSet<String>>,
}

impl ChannelCustodyStore {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<CustodyRecord>::open(path, password, MAGIC)?;
        let mut confirmed: HashMap<EventId, HashSet<String>> = HashMap::new();
        for row in records {
            confirmed.entry(row.event).or_default().insert(row.peer);
        }
        Ok(Self { file, confirmed })
    }

    pub fn peers(&self, event: EventId) -> HashSet<String> {
        self.confirmed.get(&event).cloned().unwrap_or_default()
    }

    pub fn confirm(&mut self, event: EventId, peer: &str) -> Result<(), LogError> {
        if self
            .confirmed
            .get(&event)
            .is_some_and(|set| set.contains(peer))
        {
            return Ok(());
        }
        self.file.append(&CustodyRecord {
            event,
            peer: peer.to_owned(),
        })?;
        self.confirmed
            .entry(event)
            .or_default()
            .insert(peer.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmations_are_distinct_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custody.log");
        let event = EventId::new([7; 32]);
        let mut store = ChannelCustodyStore::open(&path, "pw").unwrap();
        store.confirm(event, "bob").unwrap();
        store.confirm(event, "bob").unwrap();
        store.confirm(event, "carol").unwrap();
        assert_eq!(store.peers(event).len(), 2);
        drop(store);
        let store = ChannelCustodyStore::open(&path, "pw").unwrap();
        assert_eq!(store.peers(event).len(), 2);
        assert!(store.peers(EventId::new([8; 32])).is_empty());
    }
}
