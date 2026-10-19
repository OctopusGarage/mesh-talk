//! Durable signed file-consent revisions and authenticated direct observations.
//! Kept separate from receive preferences so protocol additions do not change
//! the existing preference file's format or downgrade compatibility.

use super::ReceiveDecision;
use super::delivery_store::DeliveryLimits;
use super::file_consent::Decision;
use crate::eventlog::{Event, EventId, LogError};
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

const MAGIC: &[u8; 6] = b"MTFCT1";
const COMPACT_AFTER: usize = 8192;

#[derive(Serialize, Deserialize)]
enum Record {
    Published(Event),
    Received(Event),
    Observed {
        manifest: EventId,
        decision: ReceiveDecision,
    },
    ReceivedDirect(Event),
}

pub(super) struct FileConsentStore {
    log: EncryptedRecordLog<Record>,
    published: HashMap<EventId, Event>,
    received: HashMap<EventId, Event>,
    decisions: HashMap<EventId, ReceiveDecision>,
    published_generations: HashMap<EventId, u64>,
    direct_overrides: HashMap<EventId, ReceiveDecision>,
    writes: usize,
    limit: usize,
}

impl FileConsentStore {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (log, records) = EncryptedRecordLog::open(path, password, MAGIC)?;
        let count = records.len();
        let limits = DeliveryLimits::default();
        let mut store = Self {
            log,
            published: HashMap::new(),
            received: HashMap::new(),
            decisions: HashMap::new(),
            published_generations: HashMap::new(),
            direct_overrides: HashMap::new(),
            writes: 0,
            limit: limits
                .completed_receipts
                .saturating_mul(limits.destinations),
        };
        for record in records {
            store.apply(record)?;
        }
        if store.published.len() > store.limit
            || store.received.len() > store.limit
            || store.decisions.len() > store.limit
        {
            return Err(LogError::Serialization(
                "too many file consent controls".into(),
            ));
        }
        let live = store.published.len() + store.received.len() + store.decisions.len();
        if count >= COMPACT_AFTER && count >= live.saturating_mul(2) {
            store.compact()?;
        }
        Ok(store)
    }

    fn apply(&mut self, record: Record) -> Result<(), LogError> {
        match record {
            Record::Published(event) => {
                Self::apply_control(&mut self.published, event)?;
            }
            Record::Received(event) => {
                if let Some((manifest, decision)) = Self::apply_control(&mut self.received, event)?
                    && !self
                        .direct_overrides
                        .get(&manifest)
                        .is_some_and(|d| *d != ReceiveDecision::Accepted)
                {
                    self.decisions.insert(manifest, decision);
                    self.direct_overrides.remove(&manifest);
                }
            }
            Record::ReceivedDirect(event) => {
                let payload = Decision::open(&event)
                    .ok_or_else(|| LogError::Serialization("invalid direct consent".into()))?;
                Self::apply_control(&mut self.received, event)?;
                self.direct_overrides.remove(&payload.manifest);
                self.decisions.insert(payload.manifest, payload.decision);
            }
            Record::Observed { manifest, decision } => {
                self.direct_overrides.insert(manifest, decision);
                self.decisions.insert(manifest, decision);
            }
        }
        Ok(())
    }

    fn apply_control(
        controls: &mut HashMap<EventId, Event>,
        event: Event,
    ) -> Result<Option<(EventId, ReceiveDecision)>, LogError> {
        let payload = Decision::open(&event)
            .ok_or_else(|| LogError::Serialization("invalid file consent control".into()))?;
        if controls
            .get(&payload.manifest)
            .is_none_or(|old| old.seq < event.seq)
        {
            controls.insert(payload.manifest, event);
            return Ok(Some((payload.manifest, payload.decision)));
        }
        Ok(None)
    }

    fn write(&mut self, record: Record) -> Result<(), LogError> {
        self.log.append_durable(&record)?;
        self.apply(record)?;
        self.writes += 1;
        if self.writes >= COMPACT_AFTER {
            let _ = self.compact();
        }
        Ok(())
    }

    fn compact(&mut self) -> Result<(), LogError> {
        let mut records = self
            .published
            .values()
            .cloned()
            .map(Record::Published)
            .collect::<Vec<_>>();
        records.extend(self.received.values().cloned().map(Record::Received));
        // A later authenticated direct answer may supersede a retained signed
        // revision. Preserve that order while keeping the replay watermark.
        records.extend(
            self.direct_overrides
                .iter()
                .map(|(manifest, decision)| Record::Observed {
                    manifest: *manifest,
                    decision: *decision,
                }),
        );
        self.log.rewrite(&records)?;
        self.writes = 0;
        Ok(())
    }

    pub fn published_control(&self, manifest: EventId) -> Option<&Event> {
        self.published.get(&manifest)
    }
    pub fn remote_decision(&self, manifest: EventId) -> Option<ReceiveDecision> {
        self.decisions.get(&manifest).copied()
    }

    pub fn published_generation(&self, manifest: EventId) -> Option<u64> {
        self.published_generations.get(&manifest).copied()
    }

    pub fn publish_control(&mut self, event: Event, generation: u64) -> Result<(), LogError> {
        let payload = Decision::open(&event)
            .ok_or_else(|| LogError::Serialization("invalid file consent control".into()))?;
        if !self.published.contains_key(&payload.manifest) && self.published.len() >= self.limit {
            return Err(LogError::Serialization("file consent control limit".into()));
        }
        self.write(Record::Published(event))?;
        self.published_generations
            .insert(payload.manifest, generation);
        Ok(())
    }

    pub fn receive_control(&mut self, event: Event) -> Result<bool, LogError> {
        self.receive(event, false)
    }

    pub fn receive_direct_control(&mut self, event: Event) -> Result<bool, LogError> {
        self.receive(event, true)
    }

    fn receive(&mut self, event: Event, direct: bool) -> Result<bool, LogError> {
        let payload = Decision::open(&event)
            .ok_or_else(|| LogError::Serialization("invalid file consent control".into()))?;
        if self
            .received
            .get(&payload.manifest)
            .is_some_and(|old| old.seq > event.seq || (!direct && old.seq == event.seq))
        {
            return Ok(false);
        }
        if direct
            && self
                .received
                .get(&payload.manifest)
                .is_some_and(|old| old.id == event.id)
            && !self.direct_overrides.contains_key(&payload.manifest)
        {
            return Ok(false);
        }
        if (!self.received.contains_key(&payload.manifest) && self.received.len() >= self.limit)
            || (!self.decisions.contains_key(&payload.manifest)
                && self.decisions.len() >= self.limit)
        {
            return Err(LogError::Serialization("file consent control limit".into()));
        }
        self.write(if direct {
            Record::ReceivedDirect(event)
        } else {
            Record::Received(event)
        })?;
        Ok(true)
    }

    pub fn observe_remote_decision(
        &mut self,
        manifest: EventId,
        decision: ReceiveDecision,
    ) -> Result<(), LogError> {
        if self.decisions.get(&manifest) == Some(&decision) {
            return Ok(());
        }
        if !self.decisions.contains_key(&manifest) && self.decisions.len() >= self.limit {
            return Err(LogError::Serialization("file consent control limit".into()));
        }
        self.write(Record::Observed { manifest, decision })
    }

    pub fn received_control(&self, manifest: EventId) -> Option<&Event> {
        self.received.get(&manifest)
    }

    #[cfg(test)]
    pub fn fail_appends_for_test(&mut self, enabled: bool) -> std::io::Result<()> {
        self.log.fail_appends_for_test(enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{eventlog::ConversationId, identity::device::DeviceIdentity};

    #[test]
    fn direct_decline_survives_compaction_and_replayed_mailbox_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consent");
        let recipient = DeviceIdentity::generate();
        let payload = Decision {
            file: ConversationId::new([1; 32]),
            manifest: EventId::new([2; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: recipient.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Accepted,
            completion: None,
        };
        let accepted = payload.event(&recipient, 1).unwrap();
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        store
            .observe_remote_decision(payload.manifest, ReceiveDecision::Declined)
            .unwrap();
        store.compact().unwrap();
        drop(store);
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        store.receive_control(accepted).unwrap();
        assert_eq!(
            store.remote_decision(payload.manifest),
            Some(ReceiveDecision::Declined)
        );
        store
            .receive_control(payload.event(&recipient, 2).unwrap())
            .unwrap();
        assert_eq!(
            store.remote_decision(payload.manifest),
            Some(ReceiveDecision::Declined),
            "unversioned direct refusal must not be undone by delayed mailbox approvals"
        );
        store
            .observe_remote_decision(payload.manifest, ReceiveDecision::Accepted)
            .unwrap();
        assert_eq!(
            store.remote_decision(payload.manifest),
            Some(ReceiveDecision::Accepted)
        );
    }
}
