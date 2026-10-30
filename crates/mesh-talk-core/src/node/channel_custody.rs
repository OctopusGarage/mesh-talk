//! Locally encrypted evidence that an authenticated channel member reported durable
//! possession of an exact event. These are historical custody confirmations, not
//! delivery or read receipts; a peer can later delete its copy or lose its device.
//! Revocations use a reserved `!` prefix so old records remain readable.

#[cfg(test)]
use crate::eventlog::LogError;
use crate::eventlog::event::EventId;
#[cfg(test)]
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

mod indexed;
pub(in crate::node) use indexed::IndexedChannelCustodyStore;

const MAGIC: &[u8; 6] = b"MTCUST";

#[derive(Clone, Serialize, Deserialize)]
struct CustodyRecord {
    event: EventId,
    peer: String,
}

#[cfg(test)]
pub struct ChannelCustodyStore {
    file: EncryptedRecordLog<CustodyRecord>,
    confirmed: HashMap<EventId, HashSet<String>>,
    observations: BTreeMap<(EventId, String), bool>,
}

#[cfg(test)]
impl ChannelCustodyStore {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<CustodyRecord>::open(path, password, MAGIC)?;
        let mut confirmed: HashMap<EventId, HashSet<String>> = HashMap::new();
        let mut observations = BTreeMap::new();
        for row in records {
            if let Some(peer) = row.peer.strip_prefix('!') {
                observations.insert((row.event, peer.to_owned()), false);
                confirmed.entry(row.event).or_default().remove(peer);
            } else {
                observations.insert((row.event, row.peer.clone()), true);
                confirmed.entry(row.event).or_default().insert(row.peer);
            }
        }
        Ok(Self {
            file,
            confirmed,
            observations,
        })
    }

    pub fn peers(&self, event: EventId) -> HashSet<String> {
        self.confirmed.get(&event).cloned().unwrap_or_default()
    }

    pub(in crate::node) fn observations_after(
        &self,
        after: Option<&(EventId, String)>,
    ) -> Vec<((EventId, String), bool)> {
        use std::ops::Bound::{Excluded, Unbounded};
        self.observations
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .take(32)
            .map(|(key, value)| (key.clone(), *value))
            .collect()
    }

    pub fn confirm(&mut self, event: EventId, peer: &str) -> Result<(), LogError> {
        debug_assert!(!peer.starts_with('!'));
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
        self.observations.insert((event, peer.to_owned()), true);
        self.confirmed
            .entry(event)
            .or_default()
            .insert(peer.to_owned());
        Ok(())
    }

    /// A fresh authenticated probe reported that the member no longer holds
    /// this event. Append the revocation before changing the in-memory view.
    pub fn revoke(&mut self, event: EventId, peer: &str) -> Result<(), LogError> {
        if !self
            .confirmed
            .get(&event)
            .is_some_and(|set| set.contains(peer))
        {
            return Ok(());
        }
        self.file.append(&CustodyRecord {
            event,
            peer: format!("!{peer}"),
        })?;
        self.observations.insert((event, peer.to_owned()), false);
        if let Some(set) = self.confirmed.get_mut(&event) {
            set.remove(peer);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn participant_warm_open_does_not_replay_retained_custody() {
        use crate::{discovery::Roster, identity::device::DeviceIdentity, node::Node};
        use std::sync::{Arc, Mutex};
        for count in [128_u64, 512] {
            let dir = tempfile::tempdir().unwrap();
            let secret = DeviceIdentity::generate().secret_bytes();
            let open = || {
                let (dm, _) = tokio::sync::mpsc::unbounded_channel();
                let (channel, _) = tokio::sync::mpsc::unbounded_channel();
                let (file, _) = tokio::sync::mpsc::unbounded_channel();
                Node::open(
                    DeviceIdentity::from_secret_bytes(secret.0, secret.1),
                    Arc::new(Mutex::new(Roster::default())),
                    dm,
                    channel,
                    file,
                    &dir.path().join("messages.log"),
                    &dir.path().join("sent.log"),
                    "pw",
                )
                .unwrap()
            };
            let id = |n: u64| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&n.to_be_bytes());
                EventId::new(bytes)
            };
            let node = open();
            for n in 0..count {
                node.channel_custody
                    .lock()
                    .unwrap()
                    .confirm(id(n), "bob")
                    .unwrap();
            }
            node.channel_custody
                .lock()
                .unwrap()
                .revoke(id(0), "bob")
                .unwrap();
            node.close_profile().unwrap();
            let node = open();
            let custody = node.channel_custody.lock().unwrap();
            assert_eq!(
                custody.replayed_records, 0,
                "warm participant open replayed {count} custody records"
            );
            assert!(custody.peers(id(0)).unwrap().is_empty());
            assert!(custody.peers(id(count - 1)).unwrap().contains("bob"));
            assert_eq!(custody.observations_after(None).unwrap().len(), 32);
            let page = custody
                .observations_after(Some(&(id(count - 34), "bob".into())))
                .unwrap();
            assert_eq!(page.len(), 32);
            assert_eq!(page.first().unwrap().0.0, id(count - 33));
            assert_eq!(page.last().unwrap().0.0, id(count - 2));
        }
    }

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

    #[test]
    fn negative_recheck_revokes_old_confirmation_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custody.log");
        let event = EventId::new([7; 32]);
        let mut store = ChannelCustodyStore::open(&path, "pw").unwrap();
        store.confirm(event, "bob").unwrap();
        store.revoke(event, "bob").unwrap();
        drop(store);
        let mut store = ChannelCustodyStore::open(&path, "pw").unwrap();
        assert!(store.peers(event).is_empty());
        assert_eq!(
            store.observations_after(None),
            vec![((event, "bob".into()), false)]
        );
        store.confirm(event, "bob").unwrap();
        assert_eq!(store.peers(event).len(), 1);
    }
}
