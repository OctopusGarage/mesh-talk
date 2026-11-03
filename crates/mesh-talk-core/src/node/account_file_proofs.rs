//! Signed file-manifest events retained separately when the original device-pair DAG
//! cannot be grafted into this sibling's event log.

use crate::eventlog::LogError;
use crate::eventlog::event::{Event, EventId, EventKind};
use crate::storage::record_log::EncryptedRecordLog;
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTAFPR";

pub(crate) struct AccountFileProofs {
    file: EncryptedRecordLog<Event>,
    events: HashMap<EventId, Event>,
}

impl AccountFileProofs {
    pub(crate) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, rows) = EncryptedRecordLog::<Event>::open(path, password, MAGIC)?;
        let mut events = HashMap::new();
        for event in rows {
            Self::validate(&event)?;
            if events
                .insert(event.id, event.clone())
                .is_some_and(|old| old != event)
            {
                return Err(LogError::CorruptFile("conflicting file proof".into()));
            }
        }
        Ok(Self { file, events })
    }

    fn validate(event: &Event) -> Result<(), LogError> {
        if event.kind != EventKind::FileManifest
            || !event.is_canonical()
            || !event.verify_integrity()
            || !event.verify_signature()
        {
            return Err(LogError::CorruptFile("invalid file proof".into()));
        }
        Ok(())
    }

    pub(crate) fn get(&self, id: &EventId) -> Option<&Event> {
        self.events.get(id)
    }

    pub(crate) fn insert(&mut self, event: Event) -> Result<(), LogError> {
        Self::validate(&event)?;
        if let Some(old) = self.events.get(&event.id) {
            if old != &event {
                return Err(LogError::CorruptFile("conflicting file proof".into()));
            }
            return Ok(());
        }
        self.file.append_durable(&event)?;
        self.events.insert(event.id, event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::ConversationId;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn signed_proof_survives_restart_and_rejects_other_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proofs.log");
        let signer = DeviceIdentity::generate();
        let proof = Event::new(
            &signer,
            ConversationId::new([1; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::FileManifest,
            vec![1],
        );
        let mut store = AccountFileProofs::open(&path, "pw").unwrap();
        store.insert(proof.clone()).unwrap();
        store.insert(proof.clone()).unwrap();
        let wrong = Event::new(
            &signer,
            ConversationId::new([1; 32]),
            2,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![1],
        );
        assert!(store.insert(wrong).is_err());
        drop(store);
        assert_eq!(
            AccountFileProofs::open(&path, "pw").unwrap().get(&proof.id),
            Some(&proof)
        );
    }
}
