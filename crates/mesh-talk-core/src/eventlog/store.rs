//! In-memory, append-only event index for all conversations. Validates events
//! (integrity, signature, canonical form, causal completeness, equivocation)
//! and answers ordering/heads/version queries. Pure data structure — no I/O.

use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::LogError;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Result of an append: a genuinely new event, or an already-present duplicate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppendOutcome {
    Appended,
    Duplicate,
}

/// Warn each time a single conversation's event count crosses a multiple of this — an
/// observability backstop for unbounded log growth (see `index_trusted`).
const CONVERSATION_WARN_EVERY: usize = 50_000;

/// The in-memory event log across all conversations.
#[derive(Default)]
pub struct EventLog {
    /// Every known event, by id.
    events: HashMap<EventId, Event>,
    /// Event ids grouped by conversation, kept in `(lamport, id)` order — the same total
    /// order `events()` returns — so reads are O(N) with no per-call sort (a `BTreeSet`
    /// orders incrementally as events are indexed). `(lamport, id)` is stable per event
    /// (both are content-addressed), so no duplicate/aliasing.
    by_conversation: HashMap<ConversationId, BTreeSet<(u64, EventId)>>,
    /// Per-conversation frontier: events not (yet) referenced as a parent.
    heads: HashMap<ConversationId, HashSet<EventId>>,
    /// Per-conversation version vector: author → highest seq seen.
    version: HashMap<ConversationId, HashMap<Author, u64>>,
    /// Per-conversation highest Lamport timestamp seen.
    max_lamport: HashMap<ConversationId, u64>,
    /// (conversation, author, seq) → id, for equivocation detection.
    author_seq: HashMap<(ConversationId, Author, u64), EventId>,
}

impl EventLog {
    /// Validate an event for appending.
    ///
    /// Returns `Ok(true)` if it is new and valid (ready to index), `Ok(false)`
    /// if it is already present (a content-addressed duplicate — safe to skip),
    /// or `Err` if it is corrupt, unsigned, non-canonical, causally incomplete,
    /// or an equivocation. Pure — does not mutate the log.
    pub fn validate(&self, event: &Event) -> Result<bool, LogError> {
        if !event.is_canonical() {
            return Err(LogError::NonCanonical);
        }
        if !event.verify_integrity() {
            return Err(LogError::CorruptId);
        }
        if !event.verify_signature() {
            return Err(LogError::BadSignature);
        }
        if self.events.contains_key(&event.id) {
            return Ok(false);
        }
        let conv = event.conversation_id;
        // Every parent must already be present, in the same conversation.
        let missing: Vec<EventId> = event
            .parents
            .iter()
            .filter(|p| self.events.get(*p).map(|e| e.conversation_id) != Some(conv))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(LogError::MissingParents(missing));
        }
        // The author must not already have a different event at this seq.
        if let Some(existing) = self.author_seq.get(&(conv, event.author, event.seq)) {
            if *existing != event.id {
                return Err(LogError::AuthorEquivocation {
                    author: event.author,
                    seq: event.seq,
                });
            }
        }
        Ok(true)
    }

    /// Insert an event that is already known to be valid (validated here, or
    /// authenticated at rest on reload). Must be called in causal order:
    /// every parent must already be indexed. The precondition is checked by a
    /// `debug_assert!`; release builds trust the caller's ordering, and an
    /// out-of-order insert would leave a stale head rather than failing.
    pub(crate) fn index_trusted(&mut self, event: Event) {
        debug_assert!(
            event.parents.iter().all(|p| self.events.contains_key(p)),
            "index_trusted called out of causal order: a parent is not indexed"
        );
        let conv = event.conversation_id;
        let id = event.id;

        // Profile events are kept OUT of the head frontier: they reference the current heads
        // (so they're causally anchored + ingestable) but are never themselves referenced as
        // a parent by future events. That makes a superseded profile an UNREFERENCED leaf,
        // which `compact_superseded_profiles` can safely drop (dropping an unreferenced event
        // can never dangle a parent) — the only safe way to bound profile-event growth in an
        // append-only DAG. Messages/reactions/files still chain normally.
        if event.kind != EventKind::Profile {
            let heads = self.heads.entry(conv).or_default();
            for p in &event.parents {
                heads.remove(p);
            }
            heads.insert(id);
        }

        let vv = self.version.entry(conv).or_default();
        let slot = vv.entry(event.author).or_insert(0);
        if event.seq > *slot {
            *slot = event.seq;
        }

        let ml = self.max_lamport.entry(conv).or_insert(0);
        if event.lamport > *ml {
            *ml = event.lamport;
        }

        self.author_seq.insert((conv, event.author, event.seq), id);
        let set = self.by_conversation.entry(conv).or_default();
        set.insert((event.lamport, id));
        // Observability for unbounded log growth: an append-only DAG can't drop interior
        // events (they're referenced as parents — pruning would dangle them), so a
        // conversation that grows pathologically (e.g. a peer flooding profile/reaction
        // events) is a known concern pending DAG-aware checkpoint compaction. Warn at each
        // threshold crossing so runaway growth is visible rather than silent.
        let count = set.len();
        if count.is_multiple_of(CONVERSATION_WARN_EVERY) {
            log::warn!(
                target: "mesh_talk::eventlog",
                "conversation {} holds {} events",
                hex::encode(&conv.as_bytes()[..4]),
                count,
            );
        }
        self.events.insert(id, event);
    }

    /// Validate and append an event.
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.validate(&event)? {
            return Ok(AppendOutcome::Duplicate);
        }
        self.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    pub fn has(&self, id: &EventId) -> bool {
        self.events.contains_key(id)
    }

    /// All event ids currently held, in arbitrary order.
    pub fn all_event_ids(&self) -> Vec<EventId> {
        self.events.keys().copied().collect()
    }

    /// All conversation ids this log holds events for.
    pub fn conversations(&self) -> Vec<ConversationId> {
        self.by_conversation.keys().copied().collect()
    }

    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.events.get(id)
    }

    /// All events in a conversation, in deterministic total order `(lamport, id)`.
    ///
    /// This order is topologically consistent (parents before children) for
    /// events created via [`EventLog::prepare`], which stamps `max_lamport + 1`.
    /// It is NOT guaranteed topological for events from a peer that violates the
    /// Lamport invariant (`child.lamport > parent.lamport`); the store does not
    /// validate Lamport monotonicity. Callers needing strict causal order for
    /// untrusted input must enforce it themselves.
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event> {
        // `by_conversation` is already in `(lamport, id)` order, so no sort here — this is a
        // hot path (every sync round + every history/reaction read).
        match self.by_conversation.get(conversation) {
            Some(ids) => ids
                .iter()
                .filter_map(|(_, id)| self.events.get(id))
                .collect(),
            None => Vec::new(),
        }
    }

    /// The current frontier of a conversation: events not referenced as a
    /// parent by any other event. Sorted for determinism.
    pub fn heads(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut heads: Vec<EventId> = self
            .heads
            .get(conversation)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        heads.sort();
        heads
    }

    /// Per-author highest sequence number seen in a conversation (for sync).
    ///
    /// This is a **max(seq), not a dense high-water mark**: the store does not
    /// enforce gap-free per-author sequences, so `author → 5` does NOT imply
    /// seqs 1..=5 are all present (a peer could deliver seqs 1 and 5 only). The
    /// sync engine must treat this as an availability hint and reconcile actual
    /// coverage via the causal frontier ([`EventLog::heads`]) and missing-parent
    /// detection, not assume contiguity.
    pub fn version_vector(&self, conversation: &ConversationId) -> HashMap<Author, u64> {
        self.version.get(conversation).cloned().unwrap_or_default()
    }

    /// The `(parents, lamport)` a new local event should use: the current heads
    /// and the next Lamport timestamp (one past the highest seen).
    pub fn prepare(&self, conversation: &ConversationId) -> (Vec<EventId>, u64) {
        let parents = self.heads(conversation);
        let lamport = self
            .max_lamport
            .get(conversation)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .expect("lamport u64 overflow (unreachable in practice)");
        (parents, lamport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::EventKind;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    fn mk(
        id: &DeviceIdentity,
        seq: u64,
        parents: Vec<EventId>,
        lamport: u64,
        payload: &[u8],
    ) -> Event {
        Event::new(
            id,
            conv(),
            seq,
            parents,
            lamport,
            0,
            EventKind::Message,
            payload.to_vec(),
        )
    }

    #[test]
    fn appends_a_root_then_a_child() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();

        let root = mk(&id, 1, vec![], 1, b"root");
        assert_eq!(log.append(root.clone()).unwrap(), AppendOutcome::Appended);
        assert!(log.has(&root.id));

        let child = mk(&id, 2, vec![root.id], 2, b"child");
        assert_eq!(log.append(child.clone()).unwrap(), AppendOutcome::Appended);
        assert!(log.has(&child.id));
    }

    #[test]
    fn rejects_event_with_missing_parent() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let orphan = mk(&id, 1, vec![EventId::new([42u8; 32])], 1, b"orphan");
        assert!(matches!(
            log.append(orphan),
            Err(LogError::MissingParents(_))
        ));
    }

    #[test]
    fn duplicate_append_is_idempotent() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let e = mk(&id, 1, vec![], 1, b"hi");
        assert_eq!(log.append(e.clone()).unwrap(), AppendOutcome::Appended);
        assert_eq!(log.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
        assert!(log.has(&e.id));
    }

    #[test]
    fn rejects_bad_signature() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let mut e = mk(&id, 1, vec![], 1, b"hi");
        e.sig[0] ^= 0xFF;
        assert!(matches!(log.append(e), Err(LogError::BadSignature)));
    }

    #[test]
    fn rejects_corrupt_id() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let mut e = mk(&id, 1, vec![], 1, b"hi");
        e.ciphertext.push(0); // mutate content without recomputing id
        assert!(matches!(log.append(e), Err(LogError::CorruptId)));
    }

    #[test]
    fn rejects_author_equivocation() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        // Two different roots from the same author at the same seq.
        let a = mk(&id, 1, vec![], 1, b"one");
        let b = mk(&id, 1, vec![], 1, b"two");
        assert_eq!(log.append(a).unwrap(), AppendOutcome::Appended);
        assert!(matches!(
            log.append(b),
            Err(LogError::AuthorEquivocation { seq: 1, .. })
        ));
    }

    #[test]
    fn rejects_parent_from_different_conversation() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        // Root lives in conversation A ([1u8;32], the one `mk` uses).
        let root = mk(&id, 1, vec![], 1, b"a");
        log.append(root.clone()).unwrap();
        // A child in conversation B tries to use it as a parent.
        let child_b = Event::new(
            &id,
            ConversationId::new([2u8; 32]),
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"b".to_vec(),
        );
        assert!(matches!(
            log.append(child_b),
            Err(LogError::MissingParents(_))
        ));
    }

    #[test]
    fn rejects_non_canonical_event() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let p1 = EventId::new([3u8; 32]);
        let p2 = EventId::new([5u8; 32]);
        let mut e = mk(&id, 2, vec![p1, p2], 2, b"x");
        // Hand-craft an unsorted, duplicate-bearing parents list.
        e.parents = vec![p2, p1, p1];
        assert!(matches!(log.append(e), Err(LogError::NonCanonical)));
    }

    #[test]
    fn events_are_returned_in_total_order() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = mk(&id, 1, vec![], 1, b"a");
        let mid = mk(&id, 2, vec![root.id], 2, b"b");
        let tip = mk(&id, 3, vec![mid.id], 3, b"c");
        log.append(root.clone()).unwrap();
        log.append(mid.clone()).unwrap();
        log.append(tip.clone()).unwrap();

        let ordered = log.events(&conv());
        let lamports: Vec<u64> = ordered.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![1, 2, 3]);
        assert_eq!(ordered[0].id, root.id);
        assert_eq!(ordered[2].id, tip.id);
    }

    #[test]
    fn heads_track_the_frontier() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = mk(&id, 1, vec![], 1, b"root");
        log.append(root.clone()).unwrap();
        assert_eq!(log.heads(&conv()), vec![root.id]);

        let child = mk(&id, 2, vec![root.id], 2, b"child");
        log.append(child.clone()).unwrap();
        assert_eq!(log.heads(&conv()), vec![child.id]);
    }

    #[test]
    fn merge_event_collapses_two_heads_into_one() {
        // Diamond: root → {a, b} (concurrent) → merge(parents=[a,b]).
        let id = DeviceIdentity::generate();
        let other = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = mk(&id, 1, vec![], 1, b"root");
        log.append(root.clone()).unwrap();
        // Two concurrent children of root (different authors so seqs don't clash).
        let a = mk(&id, 2, vec![root.id], 2, b"a");
        let b = Event::new(
            &other,
            conv(),
            1,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"b".to_vec(),
        );
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();
        // Both a and b are heads now.
        let mut frontier = log.heads(&conv());
        frontier.sort();
        let mut expected = vec![a.id, b.id];
        expected.sort();
        assert_eq!(frontier, expected);
        // A merge referencing both collapses the frontier to itself.
        let merge = mk(&id, 3, vec![a.id, b.id], 3, b"merge");
        log.append(merge.clone()).unwrap();
        assert_eq!(log.heads(&conv()), vec![merge.id]);
    }

    #[test]
    fn version_vector_tracks_max_seq_per_author() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a1 = mk(&alice, 1, vec![], 1, b"a1");
        log.append(a1.clone()).unwrap();
        let b1 = Event::new(
            &bob,
            conv(),
            1,
            vec![a1.id],
            2,
            0,
            EventKind::Message,
            b"b1".to_vec(),
        );
        log.append(b1.clone()).unwrap();
        let a2 = mk(&alice, 2, vec![b1.id], 3, b"a2");
        log.append(a2).unwrap();

        let vv = log.version_vector(&conv());
        assert_eq!(vv.get(&a1.author), Some(&2));
        assert_eq!(vv.get(&b1.author), Some(&1));
    }

    #[test]
    fn prepare_returns_heads_and_next_lamport() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        assert_eq!(log.prepare(&conv()), (vec![], 1));

        let root = mk(&id, 1, vec![], 5, b"root");
        log.append(root.clone()).unwrap();
        let (parents, lamport) = log.prepare(&conv());
        assert_eq!(parents, vec![root.id]);
        assert_eq!(lamport, 6); // one past the highest lamport (5)
    }

    #[test]
    fn events_tiebreak_by_id_when_lamport_equal() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = mk(&alice, 1, vec![], 1, b"root");
        log.append(root.clone()).unwrap();
        // Two concurrent children of root, both at lamport 2 (different authors).
        let a = mk(&alice, 2, vec![root.id], 2, b"a");
        let b = Event::new(
            &bob,
            conv(),
            1,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"b".to_vec(),
        );
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();

        let ordered = log.events(&conv());
        assert_eq!(ordered[0].id, root.id);
        // The two concurrent events appear in id order, regardless of append order.
        let mut expected = [a.id, b.id];
        expected.sort();
        assert_eq!(ordered[1].id, expected[0]);
        assert_eq!(ordered[2].id, expected[1]);
    }

    #[test]
    fn events_sort_independent_of_append_order() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = mk(&alice, 1, vec![], 1, b"root");
        log.append(root.clone()).unwrap();
        // Two concurrent children of root with different lamports (2 and 3).
        let low = mk(&alice, 2, vec![root.id], 2, b"low");
        let high = Event::new(
            &bob,
            conv(),
            1,
            vec![root.id],
            3,
            0,
            EventKind::Message,
            b"high".to_vec(),
        );
        // Append the higher-lamport event FIRST.
        log.append(high.clone()).unwrap();
        log.append(low.clone()).unwrap();

        let lamports: Vec<u64> = log.events(&conv()).iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![1, 2, 3]); // sorted despite reverse append
    }

    #[test]
    fn queries_on_unknown_conversation_are_empty() {
        let log = EventLog::default();
        assert!(log.events(&conv()).is_empty());
        assert!(log.heads(&conv()).is_empty());
        assert!(log.version_vector(&conv()).is_empty());
        assert_eq!(log.prepare(&conv()), (vec![], 1));
    }
}
