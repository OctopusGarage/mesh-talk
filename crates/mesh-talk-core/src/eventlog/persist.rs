//! A durable event log: an in-memory [`EventLog`] backed by the shared encrypted,
//! append-only [`EncryptedRecordLog`]. The on-disk format is a 6-byte magic +
//! 16-byte salt header, then length-prefixed AES-256-GCM records, one per event:
//! `[u32 be record_len][nonce(12)][ciphertext]`, where `ciphertext` is the
//! AES-GCM encryption of `bincode(Event)`.
//!
//! ## Trust boundary
//!
//! Each record is independently AEAD-authenticated, so an event's *content*
//! cannot be forged or altered on disk without the tampered record failing to
//! decrypt (→ [`LogError::CorruptFile`]). What this layer does NOT provide is
//! truncation/reorder resistance: the length prefixes are not authenticated, so
//! a corrupted mid-file frame causes the remainder to be treated as a torn tail
//! and dropped, and an attacker who knows the password could reorder records.
//!
//! That is by design. The file is a local *cache*, not the source of truth.
//! Truncation and reordering are recovered by the higher layers: events are
//! content-addressed and signed, so on re-ingest via [`crate::eventlog::EventLog`]
//! any altered event is rejected, and the sync engine re-fetches missing events
//! from peers. A dropped or reordered local record is therefore self-healing.

use crate::eventlog::event::{Author, ConversationId, Event, EventId};
use crate::eventlog::store::{AppendOutcome, EventLog};
use crate::eventlog::sync::SyncStore;
use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTLOG1";

/// A durable event log: an in-memory [`EventLog`] backed by an encrypted,
/// append-only [`EncryptedRecordLog`]. On append it validates, persists, then
/// indexes — so a write failure never leaves an unpersisted event in memory.
///
/// Invariant: every event in `log` has a record in `file`; after an interrupted
/// write `file` may hold at most one extra (torn) record, which is dropped on
/// the next [`PersistentEventLog::open`].
pub struct PersistentEventLog {
    log: EventLog,
    file: EncryptedRecordLog<Event>,
}

impl PersistentEventLog {
    /// Open (or create) the log at `path`, replaying any stored events.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, events) = EncryptedRecordLog::open(path, password, MAGIC)?;
        let mut log = EventLog::default();
        // File order IS causal order: `append` validates that all parents are
        // already indexed before writing, so a child can only appear in the file
        // after its parents — exactly what `index_trusted` requires. (Records are
        // also validated on first write and AEAD-authenticated at rest, so we
        // replay them as trusted.) Writing records via `LogFile` directly, or an
        // adversary reordering the file, breaks this; per the module trust
        // boundary, the sync layer heals any resulting stale state.
        for event in events {
            log.index_trusted(event);
        }
        Ok(Self { log, file })
    }

    /// Validate, persist, then index an event.
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.log.validate(&event)? {
            return Ok(AppendOutcome::Duplicate);
        }
        self.file.append(&event)?;
        self.log.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    pub fn has(&self, id: &EventId) -> bool {
        self.log.has(id)
    }
    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.log.get(id)
    }
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event> {
        self.log.events(conversation)
    }
    pub fn heads(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.log.heads(conversation)
    }
    pub fn version_vector(&self, conversation: &ConversationId) -> HashMap<Author, u64> {
        self.log.version_vector(conversation)
    }
    pub fn prepare(&self, conversation: &ConversationId) -> (Vec<EventId>, u64) {
        self.log.prepare(conversation)
    }

    /// All event ids currently held (used to seed an already-emitted set).
    pub fn all_event_ids(&self) -> Vec<EventId> {
        self.log.all_event_ids()
    }

    /// All conversation ids this log holds events for (used to rebuild channel state).
    pub fn conversations(&self) -> Vec<ConversationId> {
        self.log.conversations()
    }

    /// Number of events held for `conversation`.
    pub fn conversation_len(&self, conversation: &ConversationId) -> usize {
        self.log.events(conversation).len()
    }

    /// Drop every event whose conversation is NOT in `keep`, compacting the on-disk
    /// log to match. Only WHOLE conversations are dropped, so the kept set stays
    /// causally closed (no kept event references a dropped parent — partial eviction
    /// would orphan children and make them un-ingestable). Returns events dropped.
    pub fn retain_conversations(
        &mut self,
        keep: &HashSet<ConversationId>,
    ) -> Result<usize, LogError> {
        let before = self.log.all_event_ids().len();
        let mut kept: Vec<Event> = Vec::new();
        for conv in self.log.conversations() {
            if keep.contains(&conv) {
                // (lamport, id) order is causal within a conversation.
                kept.extend(self.log.events(&conv).into_iter().cloned());
            }
        }
        let dropped = before - kept.len();
        if dropped == 0 {
            return Ok(0);
        }
        self.file.rewrite(&kept)?;
        let mut log = EventLog::default();
        for event in kept {
            log.index_trusted(event);
        }
        self.log = log;
        Ok(dropped)
    }

    /// Drop a single WHOLE conversation, compacting the on-disk log to match. Used by a
    /// regular device to reclaim a per-file CHUNK conversation once the file is fully
    /// reassembled + verified — those chunk events are dead weight afterwards (one event
    /// per CHUNK_SIZE piece, otherwise kept forever, the worst append-only offender).
    /// Returns the number of events dropped (0 if the conversation is absent).
    ///
    /// Only safe for conversations no other conversation references — which holds for
    /// per-file chunk conversations: they are self-contained (their events are never
    /// parents of DM/channel events). Do NOT call this on a real DM/channel conversation.
    pub fn drop_conversation(&mut self, conversation: &ConversationId) -> Result<usize, LogError> {
        let dropped = self.log.events(conversation).len();
        if dropped == 0 {
            return Ok(0);
        }
        let kept: Vec<Event> = self
            .log
            .conversations()
            .into_iter()
            .filter(|c| c != conversation)
            // (lamport, id) order is causal within a conversation.
            .flat_map(|c| self.log.events(&c).into_iter().cloned().collect::<Vec<_>>())
            .collect();
        self.file.rewrite(&kept)?;
        let mut log = EventLog::default();
        for event in kept {
            log.index_trusted(event);
        }
        self.log = log;
        Ok(dropped)
    }
}

// NOTE: this impl is structurally identical to `impl SyncStore for EventLog`
// in sync.rs — keep the two in sync.
impl SyncStore for PersistentEventLog {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.events(conversation).iter().map(|e| e.id).collect()
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        self.events(conversation)
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .cloned()
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, EventKind};
    use crate::eventlog::sync::{reconcile, SyncStore};
    use crate::identity::device::DeviceIdentity;

    fn mk(id: &DeviceIdentity, seq: u64, lamport: u64, payload: &[u8]) -> Event {
        Event::new(
            id,
            ConversationId::new([1u8; 32]),
            seq,
            vec![],
            lamport,
            0,
            EventKind::Message,
            payload.to_vec(),
        )
    }

    #[test]
    fn event_log_record_plaintext_encodes_to_golden_bytes() {
        // A log record is `[u32 len][nonce(12)][AES-GCM(ciphertext)]` where the
        // ciphertext is `bincode(Event)` (default bincode). The framing's nonce/salt
        // are random, so we pin the DETERMINISTIC inner layout — `bincode(Event)` —
        // which is what survives encryption and must stay stable for re-ingest and
        // cross-version reads. A field reorder/type change breaks this.
        use crate::eventlog::event::{Author, Event, EventId, EventKind};
        let e = Event {
            id: EventId::new([0x11; 32]),
            conversation_id: ConversationId::new([0x22; 32]),
            author: Author::from_ed25519([0x33; 32]),
            seq: 2,
            parents: vec![EventId::new([0x44; 32])],
            lamport: 3,
            wall_clock: 4,
            kind: EventKind::Message,
            ciphertext: vec![0xAB, 0xCD],
            sig: vec![0xEE; 64],
        };
        let golden = "1111111111111111111111111111111111111111111111111111111111111111\
2222222222222222222222222222222222222222222222222222222222222222\
3333333333333333333333333333333333333333333333333333333333333333\
0200000000000000\
0100000000000000\
4444444444444444444444444444444444444444444444444444444444444444\
0300000000000000\
0400000000000000\
00000000\
0200000000000000abcd\
4000000000000000\
eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        assert_eq!(hex::encode(bincode::serialize(&e).unwrap()), golden);
        // Round-trip on the golden value.
        let back: Event = bincode::deserialize(&bincode::serialize(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn durable_log_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        let root = mk(&id, 1, 1, b"root");
        let child = Event::new(
            &id,
            conv,
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            plog.append(root.clone()).unwrap();
            plog.append(child.clone()).unwrap();
            assert_eq!(plog.heads(&conv), vec![child.id]);
        }

        // Reopen: events, heads, and version vector are rebuilt from disk.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        let events = plog.events(&conv);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, root.id);
        assert_eq!(events[1].id, child.id);
        assert_eq!(plog.heads(&conv), vec![child.id]);
        assert_eq!(plog.version_vector(&conv).get(&root.author), Some(&2));
    }

    #[test]
    fn invalid_event_is_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();

        let mut bad = mk(&id, 1, 1, b"bad");
        bad.sig[0] ^= 0xFF; // break the signature
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            assert!(matches!(plog.append(bad), Err(LogError::BadSignature)));
        }
        // Reopen: nothing was written.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&ConversationId::new([1u8; 32])).len(), 0);
    }

    #[test]
    fn duplicate_append_writes_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();
        let e = mk(&id, 1, 1, b"hi");
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Appended);
            assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
        }
        // Reopen: the duplicate was not written a second time.
        let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&conv).len(), 1);
        // Re-appending the same event after reopen is still a Duplicate.
        assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
    }

    #[test]
    fn concurrent_events_survive_reopen_with_both_heads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        let root = mk(&alice, 1, 1, b"root");
        // Two concurrent children of root, from different authors → two heads.
        let a = Event::new(
            &alice,
            conv,
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"a".to_vec(),
        );
        let b = Event::new(
            &bob,
            conv,
            1,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"b".to_vec(),
        );
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            plog.append(root.clone()).unwrap();
            plog.append(a.clone()).unwrap();
            plog.append(b.clone()).unwrap();
        }

        // Reopen: the two-head frontier and per-author version vector are restored.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        let mut frontier = plog.heads(&conv);
        frontier.sort();
        let mut expected = vec![a.id, b.id];
        expected.sort();
        assert_eq!(frontier, expected);
        assert_eq!(plog.events(&conv).len(), 3);
        assert_eq!(plog.version_vector(&conv).get(&a.author), Some(&2)); // alice: root(1), a(2)
        assert_eq!(plog.version_vector(&conv).get(&b.author), Some(&1)); // bob: b(1)
    }

    #[test]
    fn persistent_store_syncs_and_persists_received_events() {
        let dir = tempfile::tempdir().unwrap();
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        // Source log has a root + child.
        let root = mk(&id, 1, 1, b"root");
        let child = Event::new(
            &id,
            conv,
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );

        let src_path = dir.path().join("src.log");
        let mut source = PersistentEventLog::open(&src_path, "pw").unwrap();
        source.append(root.clone()).unwrap();
        source.append(child.clone()).unwrap();

        // Empty destination syncs from the source.
        let dst_path = dir.path().join("dst.log");
        {
            let mut dest = PersistentEventLog::open(&dst_path, "pw").unwrap();
            reconcile(&mut dest, &mut source, conv);
            assert!(dest.has(&root.id) && dest.has(&child.id));
            // Sanity: the SyncStore view agrees.
            assert_eq!(SyncStore::event_ids(&dest, &conv).len(), 2);
        }

        // Received events were persisted — they survive reopen.
        let dest = PersistentEventLog::open(&dst_path, "pw").unwrap();
        assert_eq!(dest.events(&conv).len(), 2);
    }

    #[test]
    fn drop_conversation_reclaims_file_chunks_without_accumulating() {
        // Regression (disk leak): a completed file transfer's per-file CHUNK
        // conversation must be reclaimable so repeated transfers don't accumulate
        // chunk events forever in messages.log. Dropping one conversation keeps the
        // rest, compacts on disk, and survives reopen — and doing it across repeated
        // "transfers" keeps the file bounded by the live (real) conversation, not by
        // total chunks ever transferred.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        let real_conv = ConversationId::new([1u8; 32]);

        let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
        // A real DM event we must always keep.
        plog.append(mk(&id, 1, 1, b"keep me")).unwrap();

        // Simulate 5 successive file transfers, each into its own file_conv with
        // several chunk events, dropped right after "save".
        for t in 0..5u8 {
            let file_conv = ConversationId::new([100 + t; 32]);
            for c in 0..6u64 {
                let chunk = Event::new(
                    &id,
                    file_conv,
                    1000 + t as u64 * 10 + c, // unique seq per author chain
                    vec![],
                    1000 + t as u64 * 10 + c,
                    0,
                    EventKind::Message,
                    vec![0xCD; 32],
                );
                plog.append(chunk).unwrap();
            }
            // The receiver saved + verified the file → reclaim its chunk events.
            let dropped = plog.drop_conversation(&file_conv).unwrap();
            assert_eq!(dropped, 6);
            assert!(plog.events(&file_conv).is_empty());
        }

        // After 5 transfers (30 chunk events total), only the real conversation's
        // single event remains — chunks did NOT accumulate.
        assert_eq!(plog.all_event_ids().len(), 1);
        assert_eq!(plog.events(&real_conv).len(), 1);

        // Dropping an absent conversation is a no-op.
        assert_eq!(
            plog.drop_conversation(&ConversationId::new([9u8; 32]))
                .unwrap(),
            0
        );

        // The compaction is durable: reopen sees only the kept event.
        drop(plog);
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.all_event_ids().len(), 1);
        assert_eq!(plog.events(&real_conv).len(), 1);
    }

    #[test]
    fn append_after_reopen_uses_replayed_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        let root = mk(&id, 1, 1, b"root");
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            plog.append(root.clone()).unwrap();
        }

        // Reopen, derive the next event from the replayed state via `prepare`.
        let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
        let (parents, lamport) = plog.prepare(&conv);
        assert_eq!(parents, vec![root.id]);
        assert_eq!(lamport, 2);
        let child = Event::new(
            &id,
            conv,
            2,
            parents,
            lamport,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        plog.append(child.clone()).unwrap();
        assert_eq!(plog.heads(&conv), vec![child.id]);

        // The appended-after-reopen event persists across another reopen.
        drop(plog);
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&conv).len(), 2);
        assert_eq!(plog.heads(&conv), vec![child.id]);
    }
}
