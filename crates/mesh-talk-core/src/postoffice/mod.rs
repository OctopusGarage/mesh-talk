//! The post office: an elected always-on peer that stores-and-forwards encrypted
//! events so a message reaches an offline recipient. Its production sync path
//! stores recipient-encrypted, pairwise opaque outer events in a
//! [`crate::eventlog::PersistentEventLog`]. It validates those outer signatures
//! and reconciles them as a [`crate::eventlog::sync::SyncStore`], but cannot
//! decrypt the signed inner events or hold a conversation key.
//!
//! [`election`] decides — deterministically, so every peer agrees — which eligible
//! (always-on) peer is the post office: the one with the lowest identity
//! fingerprint. Full election with standbys and uptime detection is Phase 1.

use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::store::AppendOutcome;
use crate::eventlog::sync::SyncStore;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod election;
pub mod federation;
pub mod gateway;
pub mod network;
pub mod opaque;

pub use election::{elect, is_post_office};

/// Soft cap on total stored events before the relay starts evicting. The relay is a
/// store-and-forward BUFFER, not the source of truth — evicted events still live with
/// their authors + online peers and re-serve via sync.
const MAX_RELAY_EVENTS: usize = 100_000;
/// Only run a (file-rewriting) compaction once the total exceeds the cap by this much,
/// so compaction runs at most once per `RETENTION_SLACK` appends rather than every one.
const RETENTION_SLACK: usize = 5_000;
/// Bound ciphertext storage even when every event is a large file chunk.
const MAX_RELAY_BYTES: usize = 512 * 1024 * 1024;
const RETENTION_BYTE_SLACK: usize = 16 * 1024 * 1024;
pub(crate) const MAX_RELAY_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const RETENTION_MAGIC: &[u8; 6] = b"MTPOR1";

#[derive(Serialize, Deserialize)]
struct MailboxArrival {
    conversation: ConversationId,
    first_seen_secs: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct FederatedMailbox {
    pub id: ConversationId,
    pub digest: [u8; 32],
    pub first_seen_secs: u64,
}

fn unix_seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn next_expiry(first_seen: &HashMap<ConversationId, u64>) -> Option<u64> {
    first_seen
        .values()
        .map(|seen| seen.saturating_add(MAX_RELAY_AGE.as_secs()))
        .min()
}

fn stored_bytes(event: &Event) -> usize {
    event
        .ciphertext
        .len()
        .saturating_add(event.parents.len().saturating_mul(32))
        .saturating_add(256)
}

/// A post office: a durable store-and-forward relay. The production network
/// path accepts only opaque outer events; local `accept` remains available for
/// pre-migration callers. Storage is bounded by evicting least-recently-touched
/// WHOLE mailboxes once it grows past the event or byte cap.
pub struct PostOffice {
    log: PersistentEventLog,
    arrivals: EncryptedRecordLog<MailboxArrival>,
    first_seen: HashMap<ConversationId, u64>,
    next_expiry_secs: Option<u64>,
    identity: DeviceIdentity,
    /// Per-conversation last-touch tick, for least-recently-used eviction.
    touch: HashMap<ConversationId, u64>,
    tick: u64,
    /// Cached total event count (kept in step with the log).
    total: usize,
    bytes: usize,
    max_events: usize,
    slack: usize,
    max_bytes: usize,
    byte_slack: usize,
    opaque_only: bool,
    changes: tokio::sync::watch::Sender<u64>,
    inventory_cache: std::cell::RefCell<Option<Vec<FederatedMailbox>>>,
    mailbox_digests: std::cell::RefCell<HashMap<ConversationId, [u8; 32]>>,
    federation_cursors: HashMap<([u8; 32], [u8; 32]), ConversationId>,
}

impl PostOffice {
    /// Durable mailbox changes, broadcast independently to each scheduler.
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn publish_change(&mut self) {
        self.inventory_cache.get_mut().take();
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    pub(crate) fn federation_batch(
        &self,
        peer: &PublicIdentity,
        mut ids: Vec<ConversationId>,
        limit: usize,
    ) -> Vec<ConversationId> {
        ids.sort_by_key(|id| *id.as_bytes());
        if let Some(cursor) = self
            .federation_cursors
            .get(&(peer.ed25519_pub, peer.x25519_pub))
        {
            let next = ids.partition_point(|id| id.as_bytes() <= cursor.as_bytes());
            if next < ids.len() {
                ids.rotate_left(next);
            }
        }
        ids.truncate(limit);
        ids
    }

    pub(crate) fn advance_federation_cursor(&mut self, peer: &PublicIdentity, id: ConversationId) {
        let key = (peer.ed25519_pub, peer.x25519_pub);
        if self.federation_cursors.len() >= 1024 && !self.federation_cursors.contains_key(&key) {
            // Scheduling hints are disposable; persistent custody is unchanged.
            self.federation_cursors.clear();
        }
        self.federation_cursors.insert(key, id);
    }

    /// Open (or create) the post office's replica at `path`, under `identity`.
    pub fn open(path: &Path, password: &str, identity: DeviceIdentity) -> Result<Self, LogError> {
        Self::open_at(path, password, identity, SystemTime::now())
    }

    fn open_at(
        path: &Path,
        password: &str,
        identity: DeviceIdentity,
        now: SystemTime,
    ) -> Result<Self, LogError> {
        let log = PersistentEventLog::open(path, password)?;
        let (mut arrivals, stored_arrivals) = EncryptedRecordLog::<MailboxArrival>::open(
            &path.with_extension("retention"),
            password,
            RETENTION_MAGIC,
        )?;
        let mut first_seen = HashMap::new();
        for record in stored_arrivals {
            first_seen.insert(record.conversation, record.first_seen_secs);
        }
        let conversations = log.conversations();
        first_seen.retain(|conversation, _| conversations.contains(conversation));
        for conversation in &conversations {
            first_seen
                .entry(*conversation)
                .or_insert_with(|| unix_seconds(now));
        }
        let records = first_seen
            .iter()
            .map(|(&conversation, &first_seen_secs)| MailboxArrival {
                conversation,
                first_seen_secs,
            })
            .collect::<Vec<_>>();
        arrivals.rewrite(&records)?;
        // Seed LRU order from the existing conversations (file order ≈ recency); live
        // accepts refine it. Distinct ticks so eviction has a deterministic order.
        let mut touch = HashMap::new();
        let mut tick = 0u64;
        for conv in log.conversations() {
            tick += 1;
            touch.insert(conv, tick);
        }
        let total = log.all_event_ids().len();
        let bytes = log
            .conversations()
            .iter()
            .flat_map(|conv| log.events(conv))
            .fold(0usize, |total, event| {
                total.saturating_add(stored_bytes(event))
            });
        let mut office = Self {
            log,
            arrivals,
            next_expiry_secs: next_expiry(&first_seen),
            first_seen,
            identity,
            touch,
            tick,
            total,
            bytes,
            max_events: MAX_RELAY_EVENTS,
            slack: RETENTION_SLACK,
            max_bytes: MAX_RELAY_BYTES,
            byte_slack: RETENTION_BYTE_SLACK,
            opaque_only: false,
            changes: tokio::sync::watch::channel(0).0,
            inventory_cache: std::cell::RefCell::new(None),
            mailbox_digests: std::cell::RefCell::new(HashMap::new()),
            federation_cursors: HashMap::new(),
        };
        office.prune_expired_at(now)?;
        Ok(office)
    }

    /// Append + track recency + enforce the storage bound. Shared by `accept` and the
    /// `SyncStore::ingest` relay path so retention applies however an event arrives.
    fn record(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.record_at(event, SystemTime::now())
    }

    fn record_at(&mut self, event: Event, now: SystemTime) -> Result<AppendOutcome, LogError> {
        self.record_at_with_arrival(event, now, None)
    }

    fn record_at_with_arrival(
        &mut self,
        event: Event,
        now: SystemTime,
        origin: Option<u64>,
    ) -> Result<AppendOutcome, LogError> {
        if origin.is_some_and(|seen| {
            seen <= unix_seconds(now).saturating_sub(MAX_RELAY_AGE.as_secs())
                || seen > unix_seconds(now).saturating_add(300)
        }) {
            return Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "federated mailbox arrival is expired or invalid",
            )));
        }
        if self
            .next_expiry_secs
            .is_some_and(|expiry| unix_seconds(now) >= expiry)
        {
            self.prune_expired_at(now)?;
        }
        let conv = event.conversation_id;
        let event_bytes = stored_bytes(&event);
        let new_mailbox = !self.first_seen.contains_key(&conv) && !self.log.has(&event.id);
        let first_seen_secs = origin
            .unwrap_or_else(|| unix_seconds(now))
            .min(unix_seconds(now));
        // Keep arrival metadata durable before the event, but never write it for
        // rejected input. The log validates once while holding exclusive access.
        let outcome = self.log.append_with_precommit(event, || {
            if new_mailbox {
                self.arrivals.append_durable(&MailboxArrival {
                    conversation: conv,
                    first_seen_secs,
                })?;
            }
            Ok(())
        })?;
        if matches!(outcome, AppendOutcome::Appended) {
            // The append is durable even if later retention maintenance fails.
            // Hash only this changed mailbox on the next inventory, preserving
            // cached digests for unrelated conversations under active load.
            self.mailbox_digests.get_mut().remove(&conv);
            self.publish_change();
            if new_mailbox {
                self.first_seen.insert(conv, first_seen_secs);
                self.next_expiry_secs = next_expiry(&self.first_seen);
            }
            self.total += 1;
            self.bytes = self.bytes.saturating_add(event_bytes);
            self.tick += 1;
            self.touch.insert(conv, self.tick);
            self.enforce_retention()?;
        }
        Ok(outcome)
    }

    /// Expire whole mailboxes so any retained event remains causally complete.
    /// Existing logs without arrival metadata begin their clock on upgrade.
    pub fn prune_expired(&mut self) -> Result<(), LogError> {
        self.prune_expired_at(SystemTime::now())
    }

    fn prune_expired_at(&mut self, now: SystemTime) -> Result<(), LogError> {
        if self
            .next_expiry_secs
            .is_none_or(|expiry| unix_seconds(now) < expiry)
        {
            return Ok(());
        }
        let cutoff = unix_seconds(now).saturating_sub(MAX_RELAY_AGE.as_secs());
        let keep = self
            .log
            .conversations()
            .into_iter()
            .filter(|conversation| {
                self.first_seen
                    .get(conversation)
                    .is_some_and(|&first_seen| first_seen > cutoff)
            })
            .collect::<HashSet<_>>();
        if keep.len() == self.first_seen.len() {
            return Ok(());
        }
        self.log.retain_conversations(&keep)?;
        self.mailbox_digests
            .get_mut()
            .retain(|id, _| keep.contains(id));
        self.publish_change();
        self.first_seen
            .retain(|conversation, _| keep.contains(conversation));
        self.next_expiry_secs = next_expiry(&self.first_seen);
        self.touch
            .retain(|conversation, _| keep.contains(conversation));
        self.recount();
        self.rewrite_arrivals()
    }

    fn recount(&mut self) {
        self.total = self.log.all_event_ids().len();
        self.bytes = self
            .log
            .conversations()
            .iter()
            .flat_map(|conversation| self.log.events(conversation))
            .fold(0usize, |total, event| {
                total.saturating_add(stored_bytes(event))
            });
    }

    fn rewrite_arrivals(&mut self) -> Result<(), LogError> {
        let records = self
            .first_seen
            .iter()
            .map(|(&conversation, &first_seen_secs)| MailboxArrival {
                conversation,
                first_seen_secs,
            })
            .collect::<Vec<_>>();
        self.arrivals.rewrite(&records)
    }

    /// Evict least-recently-touched WHOLE conversations until total ≤ `max_events`.
    /// Whole-conversation eviction keeps the kept set causally closed (dropping part of
    /// a conversation would orphan kept children → un-ingestable).
    fn enforce_retention(&mut self) -> Result<(), LogError> {
        if self.total <= self.max_events + self.slack
            && self.bytes <= self.max_bytes + self.byte_slack
        {
            return Ok(());
        }
        let mut convs: Vec<(ConversationId, u64)> = self
            .log
            .conversations()
            .into_iter()
            .map(|c| {
                let t = *self.touch.get(&c).unwrap_or(&0);
                (c, t)
            })
            .collect();
        convs.sort_by_key(|&(_, t)| t); // least-recently-touched first
        let mut keep: HashSet<ConversationId> = convs.iter().map(|(c, _)| *c).collect();
        let mut running = self.total;
        let mut running_bytes = self.bytes;
        for (conv, _) in &convs {
            if running <= self.max_events && running_bytes <= self.max_bytes {
                break;
            }
            running -= self.log.conversation_len(conv);
            running_bytes = running_bytes.saturating_sub(
                self.log.events(conv).iter().fold(0usize, |total, event| {
                    total.saturating_add(stored_bytes(event))
                }),
            );
            keep.remove(conv);
            self.touch.remove(conv);
        }
        let dropped = self.log.retain_conversations(&keep)?;
        self.mailbox_digests
            .get_mut()
            .retain(|id, _| keep.contains(id));
        self.publish_change();
        self.total -= dropped;
        self.bytes = running_bytes;
        self.first_seen
            .retain(|conversation, _| keep.contains(conversation));
        self.next_expiry_secs = next_expiry(&self.first_seen);
        self.rewrite_arrivals()?;
        Ok(())
    }

    /// This post office's public identity (used by the election).
    pub fn public(&self) -> PublicIdentity {
        self.identity.public()
    }

    /// Production relay sessions accept only the opaque outer event shape.
    pub(crate) fn require_opaque_envelopes(&mut self) {
        self.opaque_only = true;
    }

    /// Accept an event for relay: validate (signature, integrity, parents, …),
    /// persist it, and enforce the storage bound. A forged or malformed event is rejected.
    pub fn accept(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.record(event)
    }

    /// Test-only: shrink the retention thresholds so eviction is exercisable without
    /// authoring 100k events.
    #[cfg(test)]
    pub fn set_retention_cap(&mut self, max_events: usize, slack: usize) {
        self.max_events = max_events;
        self.slack = slack;
    }

    #[cfg(test)]
    pub fn set_byte_retention_cap(&mut self, max_bytes: usize, slack: usize) {
        self.max_bytes = max_bytes;
        self.byte_slack = slack;
    }

    /// Whether this post office holds the event with the given id.
    pub fn has(&self, id: &EventId) -> bool {
        self.log.has(id)
    }

    /// The ids this post office holds for a conversation, sorted (for comparison).
    pub fn held_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut ids: Vec<EventId> = self.log.events(conversation).iter().map(|e| e.id).collect();
        ids.sort();
        ids
    }

    /// Only version-3 pairwise mailboxes may be copied between relay identities.
    /// Legacy mailboxes bind their keys to one relay and stay local.
    pub fn federated_mailboxes(&self) -> Vec<ConversationId> {
        self.log
            .conversations()
            .into_iter()
            .filter(opaque::RelayMailbox::is_federated_id)
            .collect()
    }

    pub fn federated_inventory(&self) -> Vec<FederatedMailbox> {
        if let Some(cached) = self.inventory_cache.borrow().as_ref() {
            return cached.clone();
        }
        let inventory: Vec<_> = self
            .federated_mailboxes()
            .into_iter()
            .map(|id| {
                let digest = *self
                    .mailbox_digests
                    .borrow_mut()
                    .entry(id)
                    .or_insert_with(|| {
                        let mut hash = Sha256::new();
                        for event_id in self.held_ids(&id) {
                            hash.update(event_id.as_bytes());
                        }
                        hash.finalize().into()
                    });
                FederatedMailbox {
                    id,
                    digest,
                    first_seen_secs: self
                        .first_seen
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| unix_seconds(SystemTime::now())),
                }
            })
            .collect();
        *self.inventory_cache.borrow_mut() = Some(inventory.clone());
        inventory
    }

    pub(crate) fn adopt_federated_arrivals(
        &mut self,
        inventory: &[FederatedMailbox],
        now: SystemTime,
    ) -> Result<(), LogError> {
        let current = unix_seconds(now);
        let cutoff = current.saturating_sub(MAX_RELAY_AGE.as_secs());
        let mut changes = HashMap::new();
        for entry in inventory {
            if !opaque::RelayMailbox::is_federated_id(&entry.id)
                || entry.first_seen_secs <= cutoff
                || entry.first_seen_secs > current.saturating_add(300)
            {
                continue;
            }
            // Unknown inventories are session hints, not durable custody. Only
            // an accepted event may create a mailbox's arrival record.
            let Some(&prior) = changes
                .get(&entry.id)
                .or_else(|| self.first_seen.get(&entry.id))
            else {
                continue;
            };
            if entry.first_seen_secs < prior {
                changes.insert(entry.id, entry.first_seen_secs);
            }
        }
        if !changes.is_empty() {
            // Rewrite bounded metadata rather than appending an unbounded stream
            // of progressively earlier timestamps. Publish only after success so
            // a failed write remains retryable in this running process.
            let records = self
                .first_seen
                .iter()
                .map(|(&conversation, &prior)| MailboxArrival {
                    conversation,
                    first_seen_secs: changes.get(&conversation).copied().unwrap_or(prior),
                })
                .collect::<Vec<_>>();
            self.arrivals.rewrite(&records)?;
            self.first_seen.extend(changes);
            self.publish_change();
            self.next_expiry_secs = next_expiry(&self.first_seen);
            self.prune_expired_at(now)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn held_events(&self, conversation: &ConversationId) -> Vec<Event> {
        self.log.events(conversation).into_iter().cloned().collect()
    }
}

impl SyncStore for PostOffice {
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> bool {
        self.log
            .get(id)
            .is_some_and(|e| e.conversation_id == *conversation)
            && self.log.sync().is_ok()
    }
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        SyncStore::event_ids(&self.log, conversation)
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        SyncStore::events_excluding(&self.log, conversation, have)
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if self.opaque_only && !opaque::envelope_shape(&event) {
            return Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "post office requires an opaque envelope",
            )));
        }
        // Route through `record` so the relay-receive path is also retention-bounded.
        self.record(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::EventKind;
    use crate::eventlog::store::EventLog;
    use crate::eventlog::sync::reconcile;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    fn message(
        author: &DeviceIdentity,
        seq: u64,
        parents: Vec<EventId>,
        lamport: u64,
        body: &[u8],
    ) -> Event {
        Event::new(
            author,
            conv(),
            seq,
            parents,
            lamport,
            0,
            EventKind::Message,
            body.to_vec(),
        )
    }

    #[test]
    fn durable_changes_wake_each_subscriber_and_duplicates_are_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut office =
            PostOffice::open(&dir.path().join("relay"), "pw", DeviceIdentity::generate()).unwrap();
        let mut first = office.subscribe_changes();
        let second = office.subscribe_changes();
        let sender = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox = opaque::RelayMailbox::federated(&sender, &recipient.public()).unwrap();
        let event = mailbox
            .wrap(&message(&sender, 1, vec![], 1, b"one"))
            .unwrap();
        assert!(office.federated_inventory().is_empty());
        assert!(office.inventory_cache.borrow().is_some());
        office.accept(event.clone()).unwrap();
        assert!(first.has_changed().unwrap());
        assert!(second.has_changed().unwrap());
        assert!(office.inventory_cache.borrow().is_none());
        let before = office.federated_inventory();
        first.borrow_and_update();
        office.accept(event).unwrap();
        assert!(!first.has_changed().unwrap());
        assert!(office.inventory_cache.borrow().is_some());
        assert_eq!(office.federated_inventory()[0].digest, before[0].digest);
        let next = mailbox
            .wrap(&message(&sender, 2, vec![], 2, b"two"))
            .unwrap();
        office.accept(next).unwrap();
        assert_ne!(office.federated_inventory()[0].digest, before[0].digest);
    }

    #[test]
    fn failed_mailbox_attempt_rotates_before_retry_and_bounds_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut office =
            PostOffice::open(&dir.path().join("relay"), "pw", DeviceIdentity::generate()).unwrap();
        let peer = DeviceIdentity::generate().public();
        let ids = (1..=20)
            .map(|byte| ConversationId::new([byte; 32]))
            .collect::<Vec<_>>();
        let batch = office.federation_batch(&peer, ids.clone(), 16);
        assert_eq!(batch.len(), 16);
        assert_eq!(batch[0], ids[0]);
        office.advance_federation_cursor(&peer, batch[0]); // cancelled/failed I/O
        let next = office.federation_batch(&peer, ids.clone(), 16);
        assert_eq!(
            next[0], ids[1],
            "one failed mailbox cannot starve unrelated conversations"
        );
        office.advance_federation_cursor(&peer, *ids.last().unwrap());
        assert_eq!(office.federation_batch(&peer, ids.clone(), 16)[0], ids[0]);
        assert_eq!(
            office.federation_batch(&DeviceIdentity::generate().public(), ids.clone(), 16)[0],
            ids[0]
        );
    }

    #[test]
    fn inventory_cache_invalidates_when_custody_expires_or_arrival_is_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut office = PostOffice::open_at(
            &dir.path().join("relay"),
            "pw",
            DeviceIdentity::generate(),
            start,
        )
        .unwrap();
        let sender = DeviceIdentity::generate();
        let mailbox =
            opaque::RelayMailbox::federated(&sender, &DeviceIdentity::generate().public()).unwrap();
        let event = mailbox
            .wrap(&message(&sender, 1, vec![], 1, b"one"))
            .unwrap();
        office.record_at(event, start).unwrap();
        let mut inventory = office.federated_inventory();
        inventory[0].first_seen_secs -= 10;
        office.adopt_federated_arrivals(&inventory, start).unwrap();
        assert_eq!(
            office.federated_inventory()[0].first_seen_secs,
            inventory[0].first_seen_secs
        );
        office.prune_expired_at(start + MAX_RELAY_AGE).unwrap();
        assert!(office.federated_inventory().is_empty());
        assert!(office.mailbox_digests.borrow().is_empty());
        assert!(office.mailbox_digests.borrow().is_empty());
    }

    #[test]
    fn accepts_and_holds_a_valid_event() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let po_id = DeviceIdentity::generate();
        let po_pub = po_id.public();
        let mut po = PostOffice::open(&dir.path().join("po.log"), "pw", po_id).unwrap();
        assert_eq!(po.public(), po_pub); // the post office knows its own identity (for election)

        let ev = message(&alice, 1, vec![], 1, b"x");
        assert_eq!(po.accept(ev.clone()).unwrap(), AppendOutcome::Appended);
        assert!(po.has(&ev.id));
        assert_eq!(po.held_ids(&conv()), vec![ev.id]);
    }

    #[test]
    fn rejected_opaque_uploads_do_not_write_retention_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.log");
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let mailbox = opaque::RelayMailbox::new(&alice, &bob.public(), &relay.public()).unwrap();
        let outer = mailbox
            .wrap(&message(&alice, 1, vec![], 1, b"private"))
            .unwrap();
        let mut office = PostOffice::open(&path, "pw", relay).unwrap();
        office.require_opaque_envelopes();
        let retention = path.with_extension("retention");
        let before = std::fs::read(&retention).unwrap();
        let mut bad_id = outer.clone();
        bad_id.ciphertext[12] ^= 1;
        let mut bad_signature = outer.clone();
        bad_signature.sig[0] ^= 1;
        for invalid in [bad_id, bad_signature] {
            assert!(opaque::envelope_shape(&invalid));
            for _ in 0..8 {
                assert!(SyncStore::ingest(&mut office, invalid.clone()).is_err());
            }
            assert_eq!(std::fs::read(&retention).unwrap(), before);
            assert_eq!(office.total, 0);
            assert_eq!(office.bytes, 0);
            assert!(office.first_seen.is_empty());
        }
        assert_eq!(
            SyncStore::ingest(&mut office, outer.clone()).unwrap(),
            AppendOutcome::Appended
        );
        let accepted = std::fs::read(&retention).unwrap();
        assert!(accepted.len() > before.len());
        assert_eq!(
            SyncStore::ingest(&mut office, outer).unwrap(),
            AppendOutcome::Duplicate
        );
        assert_eq!(std::fs::read(&retention).unwrap(), accepted);
    }

    #[test]
    fn rejected_local_events_do_not_create_mailbox_arrivals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.log");
        let sender = DeviceIdentity::generate();
        let mut office = PostOffice::open(&path, "pw", DeviceIdentity::generate()).unwrap();
        let before = std::fs::read(path.with_extension("retention")).unwrap();
        let missing_parent = message(
            &sender,
            2,
            vec![message(&sender, 1, vec![], 1, b"missing").id],
            2,
            b"child",
        );
        for _ in 0..8 {
            assert!(matches!(
                office.accept(missing_parent.clone()),
                Err(LogError::MissingParents(_))
            ));
        }
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            before
        );
        assert!(office.first_seen.is_empty());
        assert!(office.held_ids(&conv()).is_empty());
    }

    #[test]
    fn relay_mailbox_expires_at_thirty_days_even_without_new_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("po.log");
        let identity = DeviceIdentity::generate();
        let sender = DeviceIdentity::generate();
        let start = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let lifetime = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let event = message(&sender, 1, vec![], 1, b"queued");

        let mut po = PostOffice::open_at(&path, "pw", identity, start).unwrap();
        po.record_at(event.clone(), start).unwrap();
        po.prune_expired_at(start + lifetime - std::time::Duration::from_secs(1))
            .unwrap();
        assert!(po.has(&event.id));
        po.prune_expired_at(start + lifetime).unwrap();
        assert!(!po.has(&event.id));
    }

    #[test]
    fn relay_mailbox_arrival_time_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("po.log");
        let identity = DeviceIdentity::generate();
        let sender = DeviceIdentity::generate();
        let start = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let lifetime = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let event = message(&sender, 1, vec![], 1, b"queued");

        let (ed, x) = identity.secret_bytes();
        let reopened_identity = DeviceIdentity::from_secret_bytes(ed, x);
        let mut po = PostOffice::open_at(&path, "pw", identity, start).unwrap();
        po.record_at(event.clone(), start).unwrap();
        drop(po);

        let po = PostOffice::open_at(&path, "pw", reopened_identity, start + lifetime).unwrap();
        assert!(!po.has(&event.id));
    }

    #[test]
    fn nonexistent_federated_inventory_does_not_persist_arrivals() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("relay.log");
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let identity = DeviceIdentity::generate();
        let (ed, x) = identity.secret_bytes();
        let mut office = PostOffice::open_at(&path, "pw", identity, now).unwrap();
        let before = std::fs::read(path.with_extension("retention")).unwrap();
        let sender = DeviceIdentity::generate();
        for _ in 0..16 {
            let recipient = DeviceIdentity::generate();
            let mailbox = opaque::RelayMailbox::federated(&sender, &recipient.public()).unwrap();
            let inventory = [FederatedMailbox {
                id: mailbox.id(),
                digest: [0; 32],
                first_seen_secs: unix_seconds(now) - 60,
            }];
            office.adopt_federated_arrivals(&inventory, now).unwrap();
        }
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            before
        );
        assert!(office.first_seen.is_empty());
        drop(office);
        let reopened =
            PostOffice::open_at(&path, "pw", DeviceIdentity::from_secret_bytes(ed, x), now)
                .unwrap();
        assert!(reopened.first_seen.is_empty());
    }

    #[test]
    fn federated_copy_keeps_the_origin_thirty_day_expiry() {
        let root = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mailbox = opaque::RelayMailbox::federated(&alice, &bob.public()).unwrap();
        let event = mailbox
            .wrap(&message(&alice, 1, vec![], 1, b"queued"))
            .unwrap();
        let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let later = start + Duration::from_secs(10 * 24 * 60 * 60);
        let mut first = PostOffice::open_at(
            &root.path().join("first"),
            "pw",
            DeviceIdentity::generate(),
            start,
        )
        .unwrap();
        first.record_at(event.clone(), start).unwrap();
        let inventory = first.federated_inventory();
        let second_identity = DeviceIdentity::generate();
        let (ed, x) = second_identity.secret_bytes();
        let second_path = root.path().join("second");
        let mut second = PostOffice::open_at(&second_path, "pw", second_identity, later).unwrap();
        second.adopt_federated_arrivals(&inventory, later).unwrap();
        second
            .record_at_with_arrival(event.clone(), later, Some(inventory[0].first_seen_secs))
            .unwrap();
        drop(second);
        let mut second = PostOffice::open_at(
            &second_path,
            "pw",
            DeviceIdentity::from_secret_bytes(ed, x),
            later,
        )
        .unwrap();
        assert!(second.has(&event.id));
        assert_eq!(
            second.first_seen.get(&event.conversation_id),
            Some(&unix_seconds(start))
        );
        second
            .prune_expired_at(start + Duration::from_secs(30 * 24 * 60 * 60))
            .unwrap();
        assert!(!second.has(&event.id));
    }

    #[tokio::test]
    async fn trusted_three_relay_chain_carries_presence_and_opaque_mail() {
        use crate::discovery::Announce;
        use crate::identity::account::Account;
        use crate::node::postbox::run_federated_relay_accept_loop_with_directory;
        use std::net::Ipv4Addr;
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        let root = tempfile::tempdir().unwrap();
        let p = DeviceIdentity::generate();
        let q = DeviceIdentity::generate();
        let s = DeviceIdentity::generate();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let duplicate = |identity: &DeviceIdentity| {
            let (ed, x) = identity.secret_bytes();
            DeviceIdentity::from_secret_bytes(ed, x)
        };
        let directory_p = federation::FederationDirectory::new();
        let directory_q = federation::FederationDirectory::new();
        let directory_s = federation::FederationDirectory::new();
        let store_p = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("p"), "pw", duplicate(&p)).unwrap(),
        ));
        let store_q = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("q"), "pw", duplicate(&q)).unwrap(),
        ));
        let store_s = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("s"), "pw", duplicate(&s)).unwrap(),
        ));
        let outer = opaque::RelayMailbox::federated(&alice, &bob.public())
            .unwrap()
            .wrap(&message(&alice, 1, vec![], 1, b"only-bob-can-open"))
            .unwrap();
        store_p.lock().unwrap().accept(outer.clone()).unwrap();

        let bob_presence = Announce::new_with_account(&bob, &Account::generate(), "Bob", 47475);
        directory_s
            .shared()
            .lock()
            .unwrap()
            .register(
                &bob.public(),
                Some(&bob_presence),
                true,
                std::time::Instant::now(),
            )
            .unwrap();

        let q_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let q_addr = q_listener.local_addr().unwrap();
        let s_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let s_addr = s_listener.local_addr().unwrap();
        let q_task = tokio::spawn(run_federated_relay_accept_loop_with_directory(
            duplicate(&q),
            q_listener,
            Arc::clone(&store_q),
            Arc::new(HashSet::from([p.user_id(), s.user_id()])),
            directory_q.clone(),
        ));
        let s_task = tokio::spawn(run_federated_relay_accept_loop_with_directory(
            duplicate(&s),
            s_listener,
            Arc::clone(&store_s),
            Arc::new(HashSet::from([q.user_id()])),
            directory_s.clone(),
        ));

        directory_p
            .sync_with_relay(q_addr, &p, &q.public(), &store_p)
            .await
            .unwrap();
        assert!(store_q.lock().unwrap().has(&outer.id));
        directory_q
            .sync_with_relay(s_addr, &q, &s.public(), &store_q)
            .await
            .unwrap();
        assert!(store_s.lock().unwrap().has(&outer.id));
        directory_p
            .sync_with_relay(q_addr, &p, &q.public(), &store_p)
            .await
            .unwrap();
        let introduced = directory_p.shared().lock().unwrap().export_federated(
            std::time::Instant::now(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        assert!(
            introduced
                .iter()
                .any(|proof| proof.announce.user_id == bob.user_id())
        );
        let routed = directory_p
            .shared()
            .lock()
            .unwrap()
            .export_presence_sources(
                std::time::Instant::now(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );
        assert_eq!(
            routed
                .iter()
                .find(|entry| entry.presence.announce.user_id == bob.user_id())
                .unwrap()
                .origin_hops,
            Some(2)
        );

        q_task.abort();
        s_task.abort();
    }

    #[test]
    fn existing_relay_log_starts_its_age_clock_on_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.log");
        let sender = DeviceIdentity::generate();
        let event = message(&sender, 1, vec![], 1, b"legacy queued event");
        let mut old_log = PersistentEventLog::open(&path, "pw").unwrap();
        old_log.append(event.clone()).unwrap();
        drop(old_log);

        let upgrade = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let mut po = PostOffice::open_at(&path, "pw", DeviceIdentity::generate(), upgrade).unwrap();
        assert!(po.has(&event.id));
        po.prune_expired_at(upgrade + std::time::Duration::from_secs(30 * 24 * 60 * 60))
            .unwrap();
        assert!(!po.has(&event.id));
    }

    #[test]
    fn byte_budget_evicts_oldest_whole_mailbox() {
        let dir = tempfile::tempdir().unwrap();
        let sender = DeviceIdentity::generate();
        let mut po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();
        let first = message(&sender, 1, vec![], 1, &[1; 1024]);
        let second = Event::new(
            &sender,
            ConversationId::new([2; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![2; 1024],
        );
        po.set_byte_retention_cap(stored_bytes(&first), 0);
        po.accept(first.clone()).unwrap();
        po.accept(second.clone()).unwrap();
        assert!(!po.has(&first.id));
        assert!(po.has(&second.id));
    }

    #[test]
    fn rejects_a_forged_event() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();

        let mut forged = message(&alice, 1, vec![], 1, b"x");
        forged.sig[0] ^= 0xFF;
        assert!(po.accept(forged).is_err());
    }

    #[test]
    fn a_peer_reconciles_its_events_into_the_post_office() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut alice_log = EventLog::default();
        let ev = message(&alice, 1, vec![], 1, b"x");
        alice_log.append(ev.clone()).unwrap();

        let mut po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();
        // The post office is just another SyncStore.
        reconcile(&mut alice_log, &mut po, conv());
        assert!(po.has(&ev.id));
    }

    #[test]
    fn duplicate_accept_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();

        let ev = message(&alice, 1, vec![], 1, b"x");
        assert_eq!(po.accept(ev.clone()).unwrap(), AppendOutcome::Appended);
        // Re-accepting the same event is a no-op duplicate, not a relay failure.
        assert_eq!(po.accept(ev.clone()).unwrap(), AppendOutcome::Duplicate);
        assert!(po.has(&ev.id));
    }

    #[test]
    fn unknown_conversation_and_id_hold_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();

        assert!(po.held_ids(&conv()).is_empty());
        assert!(!po.has(&EventId::new([9u8; 32])));
    }

    #[test]
    fn offline_dm_delivered_via_post_office() {
        // The headline scenario: Alice sends Bob a DM while Bob is offline; the
        // post office holds it; Bob comes online (Alice now offline) and receives
        // and decrypts it. The post office never could.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Keep a second handle to the post office's identity to prove it can't decrypt.
        let po_identity = DeviceIdentity::generate();
        let (po_ed, po_x) = po_identity.secret_bytes();
        let po_identity_copy = DeviceIdentity::from_secret_bytes(po_ed, po_x);

        // Alice seals a DM to Bob and wraps it in a Message event.
        let sealed = crate::dm::seal(&alice, &bob.public().x25519_pub, b"meet at 5").unwrap();
        let dm_event = Event::new(&alice, conv(), 1, vec![], 1, 0, EventKind::Message, sealed);

        // Alice pushes it to the post office while Bob is offline.
        let mut alice_log = EventLog::default();
        alice_log.append(dm_event.clone()).unwrap();
        let mut po = PostOffice::open(&dir.path().join("po.log"), "pw", po_identity).unwrap();
        reconcile(&mut alice_log, &mut po, conv());
        assert!(po.has(&dm_event.id));

        // Bob comes online and syncs from the post office (Alice may now be offline).
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut po, conv());
        let received = bob_log.get(&dm_event.id).expect("bob received the event");

        // Bob opens it; the post office's own identity cannot. (That `open`
        // succeeds only for the true recipient is established in `dm.rs`'s
        // round-trip / wrong-recipient tests; here we assert the relay is blind.)
        let plaintext =
            crate::dm::open(&bob, &alice.public().x25519_pub, &received.ciphertext).unwrap();
        assert_eq!(plaintext, b"meet at 5");
        assert!(
            crate::dm::open(
                &po_identity_copy,
                &alice.public().x25519_pub,
                &received.ciphertext
            )
            .is_err()
        );
    }

    #[test]
    fn two_post_offices_converge_after_split_brain() {
        // Two post offices each saw a different branch of a conversation; after they
        // reconcile, content-addressed events merge idempotently to identical state.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();

        let root = message(&alice, 1, vec![], 1, b"root");
        let x = message(&alice, 2, vec![root.id], 2, b"x");
        let y = message(&alice, 3, vec![root.id], 2, b"y");

        let mut po1 = PostOffice::open(
            &dir.path().join("po1.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let mut po2 = PostOffice::open(
            &dir.path().join("po2.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        po1.accept(root.clone()).unwrap();
        po1.accept(x.clone()).unwrap();
        po2.accept(root.clone()).unwrap();
        po2.accept(y.clone()).unwrap();

        // Premise: the two post offices genuinely start divergent.
        assert!(po1.has(&x.id) && !po1.has(&y.id));
        assert!(po2.has(&y.id) && !po2.has(&x.id));

        reconcile(&mut po1, &mut po2, conv());
        assert_eq!(po1.held_ids(&conv()), po2.held_ids(&conv()));
        assert!(po1.has(&x.id) && po1.has(&y.id));
        assert!(po2.has(&x.id) && po2.has(&y.id));
    }

    #[test]
    fn retention_evicts_least_recently_touched_whole_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();
        po.set_retention_cap(2, 0); // evict once total > 2

        let conv_n = |n: u8| ConversationId::new([n; 32]);
        let ev = |c: ConversationId| {
            Event::new(
                &alice,
                c,
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                b"m".to_vec(),
            )
        };
        let (a, b, c) = (ev(conv_n(1)), ev(conv_n(2)), ev(conv_n(3)));
        let (a_id, b_id, c_id) = (a.id, b.id, c.id);

        po.accept(a).unwrap(); // touch conv 1
        po.accept(b).unwrap(); // touch conv 2
        po.accept(c).unwrap(); // touch conv 3 → total 3 > 2 → evict LRU (conv 1)
        assert!(
            !po.has(&a_id),
            "least-recently-touched conversation is evicted whole"
        );
        assert!(po.has(&b_id) && po.has(&c_id));

        // A new event in conv 2 refreshes its recency, making conv 3 the LRU.
        let b2 = Event::new(
            &alice,
            conv_n(2),
            2,
            vec![b_id],
            2,
            0,
            EventKind::Message,
            b"m2".to_vec(),
        );
        let b2_id = b2.id;
        po.accept(b2).unwrap(); // total 3 → evict LRU (conv 3)
        assert!(
            !po.has(&c_id),
            "conv 3 now least-recently-touched → evicted"
        );
        // conv 2 kept whole — and its child still has its parent (causally closed).
        assert!(po.has(&b_id) && po.has(&b2_id));
    }
}
