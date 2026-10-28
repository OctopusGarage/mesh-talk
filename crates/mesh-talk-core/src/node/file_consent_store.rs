//! Durable signed file-consent revisions and authenticated direct observations.
//! Kept separate from receive preferences so protocol additions do not change
//! the existing preference file's format or downgrade compatibility.

use super::ReceiveDecision;
use super::delivery_store::DeliveryLimits;
use super::file_consent::Decision;
use crate::eventlog::{Event, EventId, LogError};
use crate::identity::device::PublicIdentity;
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
    received: HashMap<(EventId, [u8; 32]), Event>,
    // The old DM accessors have no recipient argument. Keep them available
    // only when the manifest names at most one recipient; group grants always
    // use the exact signed recipient key instead.
    received_recipients: HashMap<EventId, Option<[u8; 32]>>,
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
            received_recipients: HashMap::new(),
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
                if let Some((manifest, decision)) = self.apply_received_control(event)?
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
                self.apply_received_control(event)?;
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

    fn apply_received_control(
        &mut self,
        event: Event,
    ) -> Result<Option<(EventId, ReceiveDecision)>, LogError> {
        let payload = Decision::open(&event)
            .ok_or_else(|| LogError::Serialization("invalid file consent control".into()))?;
        let recipient = payload.recipient.ed25519_pub;
        let key = (payload.manifest, recipient);
        if self
            .received
            .get(&key)
            .is_none_or(|old| old.seq < event.seq)
        {
            self.received_recipients
                .entry(payload.manifest)
                .and_modify(|unique| {
                    if *unique != Some(recipient) {
                        *unique = None;
                    }
                })
                .or_insert(Some(recipient));
            self.received.insert(key, event);
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
        if self
            .received_recipients
            .get(&manifest)
            .is_some_and(|recipient| recipient.is_none())
        {
            return None;
        }
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
        let key = (payload.manifest, payload.recipient.ed25519_pub);
        if self
            .received
            .get(&key)
            .is_some_and(|old| old.seq > event.seq || (!direct && old.seq == event.seq))
        {
            return Ok(false);
        }
        if direct
            && self
                .received
                .get(&key)
                .is_some_and(|old| old.id == event.id)
            && !self.direct_overrides.contains_key(&payload.manifest)
        {
            return Ok(false);
        }
        if (!self.received.contains_key(&key) && self.received.len() >= self.limit)
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
        let recipient = self.received_recipients.get(&manifest)?.as_ref()?;
        self.received.get(&(manifest, *recipient))
    }

    pub fn received_control_for(
        &self,
        manifest: EventId,
        recipient: &PublicIdentity,
    ) -> Option<&Event> {
        self.received.get(&(manifest, recipient.ed25519_pub))
    }

    #[cfg(test)]
    pub fn received_controls(&self) -> impl Iterator<Item = &Event> {
        self.received.values()
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
    fn recipients_of_one_manifest_keep_independent_revisions_and_completion() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consent");
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let mut bob_payload = Decision {
            file: ConversationId::new([1; 32]),
            manifest: EventId::new([2; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: bob.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Accepted,
            completion: None,
        };
        let mut carol_payload = bob_payload.clone();
        carol_payload.recipient = carol.public();
        carol_payload.recipient_account = "c".repeat(32);
        carol_payload.decision = ReceiveDecision::Declined;
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(
            store
                .receive_control(bob_payload.event(&bob, 2).unwrap())
                .unwrap()
        );
        assert!(
            store
                .receive_control(carol_payload.event(&carol, 1).unwrap())
                .unwrap(),
            "another recipient's higher revision must not discard this recipient's refusal"
        );
        bob_payload.completion = Some(EventId::new([3; 32]));
        assert!(
            store
                .receive_control(bob_payload.event(&bob, 3).unwrap())
                .unwrap()
        );
        assert!(store.remote_decision(bob_payload.manifest).is_none());
        assert!(store.received_control(bob_payload.manifest).is_none());
        assert_eq!(store.received_controls().count(), 2);
        assert!(
            Decision::open(
                store
                    .received_control_for(bob_payload.manifest, &bob.public())
                    .unwrap()
            )
            .as_ref()
                == Some(&bob_payload)
        );
        assert!(
            Decision::open(
                store
                    .received_control_for(carol_payload.manifest, &carol.public())
                    .unwrap()
            )
            .as_ref()
                == Some(&carol_payload)
        );
        assert!(
            store
                .received_control_for(bob_payload.manifest, &DeviceIdentity::generate().public())
                .is_none()
        );

        store.compact().unwrap();
        drop(store);
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(store.received.len(), 2);
        assert_eq!(store.received_controls().count(), 2);
        assert!(store.received_control(bob_payload.manifest).is_none());
        assert!(
            store
                .received
                .values()
                .any(|event| Decision::open(event).as_ref() == Some(&bob_payload))
        );
        assert!(
            store
                .received
                .values()
                .any(|event| Decision::open(event).as_ref() == Some(&carol_payload))
        );
        assert!(
            !store
                .receive_control(carol_payload.event(&carol, 1).unwrap())
                .unwrap()
        );
        carol_payload.decision = ReceiveDecision::Accepted;
        assert!(
            store
                .receive_control(carol_payload.event(&carol, 2).unwrap())
                .unwrap()
        );
        assert_eq!(store.received.len(), 2);
        assert!(
            store
                .received
                .values()
                .any(|event| Decision::open(event).as_ref() == Some(&bob_payload))
        );
        assert!(store.remote_decision(bob_payload.manifest).is_none());
        store.compact().unwrap();
        drop(store);
        let store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(
            Decision::open(
                store
                    .received_control_for(bob_payload.manifest, &bob.public())
                    .unwrap()
            )
            .as_ref()
                == Some(&bob_payload)
        );
        assert!(
            Decision::open(
                store
                    .received_control_for(carol_payload.manifest, &carol.public())
                    .unwrap()
            )
            .as_ref()
                == Some(&carol_payload)
        );
    }

    #[test]
    fn direct_recipient_revision_does_not_override_another_recipient() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consent");
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let bob_payload = Decision {
            file: ConversationId::new([11; 32]),
            manifest: EventId::new([12; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: bob.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Declined,
            completion: None,
        };
        let mut carol_payload = bob_payload.clone();
        carol_payload.recipient = carol.public();
        carol_payload.recipient_account = "c".repeat(32);
        carol_payload.decision = ReceiveDecision::Accepted;
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(
            store
                .receive_direct_control(bob_payload.event(&bob, 9).unwrap())
                .unwrap()
        );
        assert!(
            store
                .receive_control(carol_payload.event(&carol, 1).unwrap())
                .unwrap()
        );
        assert_eq!(store.received.len(), 2);
        assert!(
            store
                .received
                .values()
                .any(|event| Decision::open(event).as_ref() == Some(&bob_payload))
        );
        assert!(
            store
                .received
                .values()
                .any(|event| Decision::open(event).as_ref() == Some(&carol_payload))
        );
    }

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
