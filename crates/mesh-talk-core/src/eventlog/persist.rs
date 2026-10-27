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
//! Authenticated replay repairs parent/child record reordering, including files
//! written by earlier timestamp-sorted compactions. Unresolved ancestry returns
//! an error instead of installing an inconsistent frontier. Events remain signed
//! and content-addressed; peer sync can recover a causally complete missing tail.

use crate::eventlog::LogError;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::store::{AppendOutcome, EventLog};
use crate::eventlog::sync::SyncStore;
use crate::storage::record_log::EncryptedRecordLog;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAGIC: &[u8; 6] = b"MTLOG1";
const PROFILE_COMPACTION_COOLDOWN: Duration = Duration::from_secs(10);
const PROFILE_REWRITE_COOLDOWN: Duration = Duration::from_secs(60);
const PROFILE_RECLAIM_RATIO: usize = 8;

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
    pending_profile_compactions: HashSet<ConversationId>,
    last_profile_compaction_scan: Option<Instant>,
    last_profile_rewrite_attempt: Option<Instant>,
    #[cfg(test)]
    rewrite_count: usize,
    fingerprint_cache: Mutex<HashMap<ConversationId, ([u8; 32], usize)>>,
    #[cfg(test)]
    sync_event_exports: std::cell::Cell<usize>,
}

impl PersistentEventLog {
    #[cfg(test)]
    pub(crate) fn sync_metadata_observations(
        &self,
        conversation: &ConversationId,
    ) -> (Option<([u8; 32], usize)>, usize) {
        (
            self.fingerprint_cache
                .lock()
                .unwrap()
                .get(conversation)
                .copied(),
            self.sync_event_exports.get(),
        )
    }
    #[cfg(test)]
    pub(crate) fn before_rewrite_rename(&mut self, hook: Box<dyn FnOnce() + Send + Sync>) {
        self.file.before_rewrite_rename = Some(hook);
    }
    /// Open (or create) the log at `path`, replaying any stored events.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, events) = EncryptedRecordLog::<Event>::open(path, password, MAGIC)?;
        let mut log = EventLog::default();
        // Normal appends are already causal. Earlier versions could compact by
        // timestamp, so defer authenticated records whose parents appear later.
        // Only that exceptional subset needs topological sorting on startup.
        let mut deferred = Vec::new();
        for event in events {
            if event.parents.iter().all(|parent| {
                log.get(parent)
                    .is_some_and(|prior| prior.conversation_id == event.conversation_id)
            }) {
                log.index_trusted(event);
            } else {
                deferred.push(event);
            }
        }
        if !deferred.is_empty() {
            log = Self::index_retained(deferred, log)?.0;
        }
        Ok(Self {
            log,
            file,
            pending_profile_compactions: HashSet::new(),
            last_profile_compaction_scan: None,
            last_profile_rewrite_attempt: None,
            #[cfg(test)]
            rewrite_count: 0,
            fingerprint_cache: Mutex::new(HashMap::new()),
            #[cfg(test)]
            sync_event_exports: std::cell::Cell::new(0),
        })
    }

    /// Validate, persist, then index an event.
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append_with_precommit(event, || Ok(()))
    }

    /// Commit local prerequisites after validation, before publishing a new event.
    /// Invalid events and duplicates never run `precommit`. Exclusive access keeps
    /// validation valid through the write without a second signature check.
    /// A hook failure leaves this log unchanged; a later event-write failure may
    /// leave committed prerequisites, which the caller must tolerate on recovery.
    pub(crate) fn append_with_precommit(
        &mut self,
        event: Event,
        precommit: impl FnOnce() -> Result<(), LogError>,
    ) -> Result<AppendOutcome, LogError> {
        if !self.log.validate(&event)? {
            return Ok(AppendOutcome::Duplicate);
        }
        precommit()?;
        self.file.append(&event)?;
        self.fingerprint_cache
            .get_mut()
            .expect("fingerprint cache lock not poisoned")
            .remove(&event.conversation_id);
        self.log.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    /// Transaction boundary for delivery: flush all causal parents before writing
    /// an intent, and synchronize even when recovery finds an existing event.
    pub fn sync(&self) -> Result<(), LogError> {
        self.file.sync()
    }

    pub fn append_durable(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.log.validate(&event)? {
            self.file.sync()?;
            return Ok(AppendOutcome::Duplicate);
        }
        self.file.append_durable(&event)?;
        self.fingerprint_cache
            .get_mut()
            .expect("fingerprint cache lock not poisoned")
            .remove(&event.conversation_id);
        self.log.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    /// Commit a new channel's verified ancestry and invitation together. The
    /// caller has already enforced channel admission; this method independently
    /// verifies event integrity, causal order, and an empty destination before
    /// the atomic file replacement.
    pub(crate) fn append_channel_batch_atomic(&mut self, events: &[Event]) -> Result<(), LogError> {
        let Some(first) = events.first() else {
            return Ok(());
        };
        if !self.log.events(&first.conversation_id).is_empty()
            || events
                .iter()
                .any(|event| event.conversation_id != first.conversation_id)
        {
            return Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "channel batch must be one new conversation",
            )));
        }
        let mut validation = EventLog::default();
        for event in events {
            validation.append(event.clone())?;
        }
        self.file.append_batch_atomic(events)?;
        self.fingerprint_cache
            .get_mut()
            .expect("fingerprint cache lock not poisoned")
            .remove(&first.conversation_id);
        for event in events {
            self.log.index_trusted(event.clone());
        }
        Ok(())
    }

    pub fn has(&self, id: &EventId) -> bool {
        self.log.has(id)
    }
    pub fn requires_channel_v3(&self, conversation: &ConversationId) -> bool {
        self.log.requires_channel_v3(conversation)
    }
    pub fn requires_channel_v4(&self, conversation: &ConversationId) -> bool {
        self.log.requires_channel_v4(conversation)
    }
    pub fn requires_channel_v2(&self, conversation: &ConversationId) -> bool {
        self.log.requires_channel_v2(conversation)
    }
    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.log.get(id)
    }
    pub fn authored_event(
        &self,
        conversation: ConversationId,
        author: crate::eventlog::event::Author,
        seq: u64,
    ) -> Option<&Event> {
        self.log.authored_event(conversation, author, seq)
    }
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event> {
        self.log.events(conversation)
    }
    pub fn message_keys_after(
        &self,
        conversation: ConversationId,
        author: crate::eventlog::event::Author,
        cursor: Option<(u64, EventId)>,
        limit: usize,
    ) -> Vec<(u64, EventId)> {
        self.log
            .message_keys_after(conversation, author, cursor, limit)
    }
    pub fn heads(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.log.heads(conversation)
    }
    pub fn version_vector(&self, conversation: &ConversationId) -> HashMap<Author, u64> {
        self.log.version_vector(conversation)
    }
    pub fn prepare(&self, conversation: &ConversationId) -> Result<(Vec<EventId>, u64), LogError> {
        self.log.prepare(conversation)
    }

    pub fn next_sequence(
        &self,
        conversation: &ConversationId,
        author: &Author,
    ) -> Result<u64, LogError> {
        self.log.next_sequence(conversation, author)
    }

    /// Rebuild causal indexes before publishing replacement records. Timestamp
    /// order is only a tie-break; accepted events may have nonmonotonic clocks.
    fn rewrite_kept(&mut self, kept: Vec<Event>) -> Result<(), LogError> {
        let (rebuilt, ordered) = Self::index_retained(kept, EventLog::default())?;
        self.file.rewrite(&ordered)?;
        self.log = rebuilt;
        #[cfg(test)]
        {
            self.rewrite_count += 1;
        }
        Ok(())
    }

    fn index_retained(
        kept: Vec<Event>,
        mut rebuilt: EventLog,
    ) -> Result<(EventLog, Vec<Event>), LogError> {
        let refs: Vec<_> = kept.iter().collect();
        let order = super::order::ordered_ids(&refs, &[], true);
        if order.len() != kept.len() {
            return Err(LogError::CorruptFile(
                "retained event graph is not acyclic".into(),
            ));
        }
        let mut by_id: HashMap<_, _> = kept.into_iter().map(|event| (event.id, event)).collect();
        let mut ordered = Vec::with_capacity(order.len());
        for id in order {
            let event = by_id.remove(&id).expect("ordered retained event exists");
            let missing: Vec<_> = event
                .parents
                .iter()
                .filter(|parent| {
                    rebuilt
                        .get(parent)
                        .is_none_or(|prior| prior.conversation_id != event.conversation_id)
                })
                .copied()
                .collect();
            if !missing.is_empty() {
                return Err(LogError::MissingParents(missing));
            }
            // These immutable events were already validated on admission. Only
            // the replacement's causal completeness needs checking again.
            rebuilt.index_trusted(event.clone());
            ordered.push(event);
        }
        Ok((rebuilt, ordered))
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

    pub fn fingerprint_and_len(&self, conversation: &ConversationId) -> ([u8; 32], usize) {
        let mut cache = self
            .fingerprint_cache
            .lock()
            .expect("fingerprint cache lock not poisoned");
        if let Some(cached) = cache.get(conversation) {
            return *cached;
        }
        let ids: Vec<_> = self
            .log
            .events(conversation)
            .iter()
            .map(|event| event.id)
            .collect();
        let len = ids.len();
        let fingerprint = (crate::eventlog::sync::fingerprint(ids), len);
        // Unknown remote mailbox probes must not grow a cache entry per supplied
        // ID. Retained nonempty conversations are bounded by the owning store.
        if len > 0 {
            cache.insert(*conversation, fingerprint);
        }
        fingerprint
    }

    /// Mark a conversation for profile compaction. Requests are coalesced until the
    /// maintenance path drains them, so a burst of profile syncs cannot enqueue repeated
    /// rewrites for the same conversation.
    pub fn request_profile_compaction(&mut self, conversation: ConversationId) {
        self.pending_profile_compactions.insert(conversation);
    }

    /// Drain eligible profile requests in at most one whole-file rewrite. The
    /// byte-amortization threshold and global rewrite cooldown bound maintenance
    /// independently of how many conversations old peers keep reintroducing.
    pub fn drain_one_profile_compaction(&mut self) -> Result<Option<usize>, LogError> {
        self.drain_one_profile_compaction_at(Instant::now())
    }

    fn drain_one_profile_compaction_at(&mut self, now: Instant) -> Result<Option<usize>, LogError> {
        if self
            .last_profile_rewrite_attempt
            .is_some_and(|last| now.saturating_duration_since(last) < PROFILE_REWRITE_COOLDOWN)
        {
            return Ok(None);
        }
        if self
            .last_profile_compaction_scan
            .is_some_and(|last| now.saturating_duration_since(last) < PROFILE_COMPACTION_COOLDOWN)
        {
            return Ok(None);
        }
        let conversations = self
            .pending_profile_compactions
            .iter()
            .copied()
            .collect::<Vec<_>>();
        if conversations.is_empty() {
            return Ok(None);
        }
        self.last_profile_compaction_scan = Some(now);
        let dropped = self.compact_profile_conversations(&conversations, Some(now))?;
        for conversation in conversations {
            // Keep amortization candidates across scans so independent small
            // profile changes can eventually justify one shared rewrite. A
            // successful rewrite can also expose previously referenced leaves.
            if self.superseded_profile_ids(&conversation).is_empty() {
                self.pending_profile_compactions.remove(&conversation);
            }
        }
        Ok(Some(dropped))
    }

    #[cfg(test)]
    pub(crate) fn pending_profile_compactions(&self) -> usize {
        self.pending_profile_compactions.len()
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
                // Rebuild establishes causal order before replacing the file.
                kept.extend(self.log.events(&conv).into_iter().cloned());
            }
        }
        let dropped = before - kept.len();
        if dropped == 0 {
            return Ok(0);
        }
        self.rewrite_kept(kept)?;
        self.fingerprint_cache
            .get_mut()
            .expect("fingerprint cache lock not poisoned")
            .clear();
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
            // Rebuild establishes causal order before replacing the file.
            .flat_map(|c| self.log.events(&c).into_iter().cloned().collect::<Vec<_>>())
            .collect();
        self.rewrite_kept(kept)?;
        self.fingerprint_cache
            .get_mut()
            .expect("fingerprint cache lock not poisoned")
            .remove(conversation);
        Ok(dropped)
    }

    /// Compact superseded profile events to bound on-disk growth. A conversation accrues one
    /// Profile event per avatar change per author, but only the NEWEST per account is ever
    /// needed. A profile event is safe to drop iff it is BOTH (a) superseded by a
    /// higher-`(lamport, id)` profile from the same author and (b) UNREFERENCED as a parent
    /// by any event — dropping an unreferenced event can never dangle a child, so this is
    /// safe regardless of history or peer version. New profile events are kept out of the
    /// head frontier (see [`EventLog::index_trusted`]) so they're unreferenced; any profile
    /// a message DID reference (legacy / an older peer's) stays. Deterministic (highest
    /// `(lamport, id)` order per author) so peers converge. Preserve the latest two
    /// unreferenced leaves per author. Compatibility avatars name their paired signed
    /// cards as parents, which stay until superseded children have been removed.
    /// The event log cannot classify ciphertext.
    /// Rewrites only when at least `COMPACT_PROFILE_MIN` events and one eighth
    /// of the whole log's estimated bytes are reclaimable. Small profile changes
    /// must not repeatedly rewrite unrelated attachment/history ciphertext.
    /// Returns the count dropped; the latest pair and referenced events always remain.
    pub fn compact_superseded_profiles(
        &mut self,
        conversation: &ConversationId,
    ) -> Result<usize, LogError> {
        self.compact_profile_conversations(&[*conversation], None)
    }

    fn superseded_profile_ids(&self, conversation: &ConversationId) -> HashSet<EventId> {
        let events = self.log.events(conversation);
        let referenced: HashSet<EventId> = events
            .iter()
            .flat_map(|e| e.parents.iter().copied())
            .collect();
        let mut newest: HashMap<Author, Vec<EventId>> = HashMap::new();
        for e in &events {
            if e.kind == EventKind::Profile && !referenced.contains(&e.id) {
                let pair = newest.entry(e.author).or_default();
                if pair.len() == 2 {
                    pair.remove(0);
                }
                pair.push(e.id); // (lamport, id) order ⇒ last pair is newest
            }
        }
        events
            .iter()
            .filter(|e| {
                e.kind == EventKind::Profile
                    && !referenced.contains(&e.id)
                    && !newest
                        .get(&e.author)
                        .is_some_and(|pair| pair.contains(&e.id))
            })
            .map(|e| e.id)
            .collect()
    }

    fn compact_profile_conversations(
        &mut self,
        conversations: &[ConversationId],
        maintenance_at: Option<Instant>,
    ) -> Result<usize, LogError> {
        let drop: HashSet<_> = conversations
            .iter()
            .flat_map(|conversation| self.superseded_profile_ids(conversation))
            .collect();
        if drop.len() < COMPACT_PROFILE_MIN {
            return Ok(0);
        }
        // Estimate encoded record cost from lengths, without copying or
        // serializing any ciphertext until a rewrite is worth its global cost.
        let (total_bytes, reclaimed_bytes) = self
            .log
            .conversations()
            .into_iter()
            .flat_map(|conversation| self.log.events(&conversation))
            .fold((0usize, 0usize), |(total, reclaimed), event| {
                let bytes = event
                    .ciphertext
                    .len()
                    .saturating_add(event.parents.len().saturating_mul(32))
                    .saturating_add(256);
                (
                    total.saturating_add(bytes),
                    reclaimed.saturating_add(if drop.contains(&event.id) { bytes } else { 0 }),
                )
            });
        if reclaimed_bytes < total_bytes.div_ceil(PROFILE_RECLAIM_RATIO) {
            return Ok(0);
        }
        // Failed writes also consume the maintenance budget while all requests
        // remain pending, preventing an expensive I/O failure retry loop.
        if let Some(now) = maintenance_at {
            self.last_profile_rewrite_attempt = Some(now);
        }
        let kept: Vec<Event> = self
            .log
            .conversations()
            .into_iter()
            .flat_map(|c| {
                self.log
                    .events(&c)
                    .into_iter()
                    .filter(|e| !drop.contains(&e.id))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect();
        self.rewrite_kept(kept)?;
        let cache = self
            .fingerprint_cache
            .get_mut()
            .expect("fingerprint cache lock not poisoned");
        for conversation in conversations {
            cache.remove(conversation);
        }
        Ok(drop.len())
    }
}

/// Minimum droppable superseded profiles before [`PersistentEventLog::compact_superseded_profiles`]
/// may rewrite the log. The byte-amortization gate separately bounds retained garbage
/// relative to the whole encrypted log rather than imposing a fixed stale-event ceiling.
const COMPACT_PROFILE_MIN: usize = 8;

// NOTE: this impl is structurally identical to `impl SyncStore for EventLog`
// in sync.rs — keep the two in sync.
impl SyncStore for PersistentEventLog {
    fn requires_channel_v3(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(PersistentEventLog::requires_channel_v3(self, conversation))
    }
    fn requires_channel_v4(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(PersistentEventLog::requires_channel_v4(self, conversation))
    }
    fn requires_channel_v2(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(PersistentEventLog::requires_channel_v2(self, conversation))
    }
    fn fingerprint_and_len(
        &self,
        conversation: &ConversationId,
    ) -> Result<([u8; 32], usize), LogError> {
        Ok(PersistentEventLog::fingerprint_and_len(self, conversation))
    }
    fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
        Ok(self.events(conversation).iter().map(|e| e.id).collect())
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Result<Vec<Event>, LogError> {
        #[cfg(test)]
        self.sync_event_exports
            .set(self.sync_event_exports.get() + 1);
        Ok(super::sync::causal_events(self.events(conversation))
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .cloned()
            .collect())
    }
    fn events_excluding_bounded(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
        event_budget: usize,
        max_frames: usize,
    ) -> Result<Vec<Event>, LogError> {
        Ok(super::sync::bounded_events(
            super::sync::causal_events(self.events(conversation))
                .into_iter()
                .filter(|event| !have.contains(&event.id)),
            event_budget,
            max_frames,
        ))
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, EventKind};
    use crate::eventlog::sync::{SyncStore, reconcile};
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
    fn account_channel_version_index_rejects_invalid_events_and_survives_restart() {
        use crate::eventlog::sync::SyncStore;
        for kind in [
            EventKind::ChannelAccountAuthority,
            EventKind::ChannelDeviceInheritance,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("events.log");
            let signer = DeviceIdentity::generate();
            let conversation = ConversationId::new([7; 32]);
            let unrelated = ConversationId::new([8; 32]);
            let event = Event::new(&signer, conversation, 1, vec![], 1, 0, kind, vec![1]);
            {
                let mut log = PersistentEventLog::open(&path, "pw").unwrap();
                assert!(!log.requires_channel_v4(&conversation));
                let mut invalid = event.clone();
                invalid.sig[0] ^= 1;
                assert!(log.append_durable(invalid).is_err());
                assert!(!log.requires_channel_v4(&conversation));
                log.append_durable(event).unwrap();
                assert!(SyncStore::requires_channel_v4(&log, &conversation).unwrap());
                assert!(!log.requires_channel_v4(&unrelated));
            }
            let reopened = PersistentEventLog::open(&path, "pw").unwrap();
            assert!(SyncStore::requires_channel_v4(&reopened, &conversation).unwrap());
            assert!(!reopened.requires_channel_v4(&unrelated));
        }
    }

    #[test]
    fn bounded_durable_sync_preserves_ancestry_after_reopen() {
        use crate::eventlog::sync::{build_request, handle_request_bounded, handle_response};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let author = DeviceIdentity::generate();
        let parent = mk(&author, 1, 100, b"parent");
        let child = Event::new(
            &author,
            parent.conversation_id,
            2,
            vec![parent.id],
            1,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        {
            let mut source = PersistentEventLog::open(&path, "pw").unwrap();
            source.append_durable(parent.clone()).unwrap();
            source.append_durable(child.clone()).unwrap();
        }
        let source = PersistentEventLog::open(&path, "pw").unwrap();
        let mut target = EventLog::default();
        for expected in [parent.id, child.id] {
            let response = handle_request_bounded(
                &source,
                &build_request(&target, parent.conversation_id).unwrap(),
                1,
            )
            .unwrap();
            assert_eq!(response.events.len(), 1);
            assert_eq!(response.events[0].id, expected);
            let (report, _) = handle_response(&mut target, &response).unwrap();
            assert_eq!(report.applied, 1);
            assert!(report.rejected.is_empty());
        }
    }

    #[test]
    fn invalid_retained_graph_does_not_publish_file_or_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let author = DeviceIdentity::generate();
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let parent = mk(&author, 1, 100, b"parent");
        let child = Event::new(
            &author,
            parent.conversation_id,
            2,
            vec![parent.id],
            1,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        log.append_durable(parent.clone()).unwrap();
        log.append_durable(child.clone()).unwrap();
        let before_file = std::fs::read(&path).unwrap();
        let before_heads = log.heads(&parent.conversation_id);
        let before_version = log.version_vector(&parent.conversation_id);

        // Exercise the shared replacement boundary with a retained child whose
        // parent was accidentally omitted. No candidate may publish first.
        assert!(matches!(
            log.rewrite_kept(vec![child.clone()]),
            Err(LogError::MissingParents(ids)) if ids == vec![parent.id]
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before_file);
        assert!(log.has(&parent.id));
        assert!(log.has(&child.id));
        assert_eq!(log.heads(&parent.conversation_id), before_heads);
        assert_eq!(log.version_vector(&parent.conversation_id), before_version);
        drop(log);
        let reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert!(reopened.has(&parent.id));
        assert!(reopened.has(&child.id));
        assert_eq!(reopened.heads(&parent.conversation_id), before_heads);
    }

    #[test]
    fn authenticated_replay_repairs_prior_noncausal_compaction_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let author = DeviceIdentity::generate();
        let parent = mk(&author, 1, 100, b"parent");
        let child = Event::new(
            &author,
            parent.conversation_id,
            2,
            vec![parent.id],
            1,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        // Earlier compactions wrote accepted events by timestamp rather than ancestry.
        let (mut file, _) = EncryptedRecordLog::<Event>::open(&path, "pw", MAGIC).unwrap();
        file.append_durable(&child).unwrap();
        file.append_durable(&parent).unwrap();
        drop(file);
        let reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(reopened.heads(&parent.conversation_id), vec![child.id]);
        assert_eq!(
            reopened.version_vector(&parent.conversation_id)[&parent.author],
            2
        );
    }

    #[test]
    fn compaction_preserves_causal_frontier_with_nonmonotonic_clocks() {
        for mode in ["retain", "drop", "profiles"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("events.log");
            let author = DeviceIdentity::generate();
            let mut log = PersistentEventLog::open(&path, "pw").unwrap();
            let parent = mk(&author, 1, 100, b"parent");
            let conv = parent.conversation_id;
            let child = Event::new(
                &author,
                conv,
                2,
                vec![parent.id],
                1,
                0,
                EventKind::Message,
                b"child".to_vec(),
            );
            log.append_durable(parent.clone()).unwrap();
            log.append_durable(child.clone()).unwrap();
            let other = Event::new(
                &author,
                ConversationId::new([2; 32]),
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                vec![],
            );
            log.append_durable(other.clone()).unwrap();
            match mode {
                "retain" => {
                    log.retain_conversations(&HashSet::from([conv])).unwrap();
                }
                "drop" => {
                    log.drop_conversation(&other.conversation_id).unwrap();
                }
                "profiles" => {
                    for i in 0..10 {
                        let profile = Event::new(
                            &author,
                            other.conversation_id,
                            i + 2,
                            vec![],
                            i + 2,
                            0,
                            EventKind::Profile,
                            vec![i as u8],
                        );
                        log.append_durable(profile).unwrap();
                    }
                    assert!(
                        log.compact_superseded_profiles(&other.conversation_id)
                            .unwrap()
                            > 0
                    );
                }
                _ => unreachable!(),
            }
            assert_eq!(log.heads(&conv), vec![child.id], "{mode}");
            drop(log);
            let reopened = PersistentEventLog::open(&path, "pw").unwrap();
            assert!(reopened.has(&parent.id));
            assert_eq!(reopened.heads(&conv), vec![child.id], "reopen {mode}");
        }
    }

    #[test]
    fn precommit_runs_only_for_new_valid_events_and_before_the_event_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let author = DeviceIdentity::generate();
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let event = mk(&author, 1, 1, b"valid");
        let before = std::fs::read(&path).unwrap();
        let mut corrupt = event.clone();
        corrupt.sig[0] ^= 1;
        assert!(matches!(
            log.append_with_precommit(corrupt, || panic!("invalid event ran precommit")),
            Err(LogError::BadSignature)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut calls = 0;
        assert_eq!(
            log.append_with_precommit(event.clone(), || {
                assert_eq!(std::fs::read(&path).unwrap(), before);
                calls += 1;
                Ok(())
            })
            .unwrap(),
            AppendOutcome::Appended
        );
        assert_eq!(calls, 1);
        assert_eq!(
            log.append_with_precommit(event.clone(), || panic!("duplicate ran precommit"))
                .unwrap(),
            AppendOutcome::Duplicate
        );
        // A distinct, correctly signed event at the same author sequence is
        // rejected before a prerequisite can be written, too.
        assert!(matches!(
            log.append_with_precommit(mk(&author, 1, 1, b"fork"), || {
                panic!("equivocation ran precommit")
            }),
            Err(LogError::AuthorEquivocation { .. })
        ));
        drop(log);
        let reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(reopened.get(&event.id), Some(&event));
        assert_eq!(reopened.all_event_ids(), vec![event.id]);
    }

    #[test]
    fn failed_precommit_does_not_publish_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let author = DeviceIdentity::generate();
        let event = mk(&author, 1, 1, b"retry");
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let before = std::fs::read(&path).unwrap();
        let fingerprint = log.fingerprint_and_len(&event.conversation_id);
        assert!(matches!(
            log.append_with_precommit(event.clone(), || {
                Err(LogError::Io(std::io::Error::other("precommit failed")))
            }),
            Err(LogError::Io(_))
        ));
        assert!(!log.has(&event.id));
        assert_eq!(log.fingerprint_and_len(&event.conversation_id), fingerprint);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(log);
        let mut reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert!(!reopened.has(&event.id));
        assert_eq!(
            reopened.append_with_precommit(event, || Ok(())).unwrap(),
            AppendOutcome::Appended
        );
    }

    #[test]
    fn cached_fingerprint_tracks_append_and_whole_file_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("events.log"), "pw").unwrap();
        let conv = ConversationId::new([1u8; 32]);
        let empty = log.fingerprint_and_len(&conv);
        assert_eq!(empty.1, 0);
        assert_eq!(log.fingerprint_and_len(&conv), empty);
        let author = DeviceIdentity::generate();
        log.append(mk(&author, 1, 1, b"one")).unwrap();
        let first = log.fingerprint_and_len(&conv);
        assert_eq!(first.1, 1);
        assert_ne!(first.0, empty.0);
        log.append(mk(&author, 2, 2, b"two")).unwrap();
        assert_eq!(log.fingerprint_and_len(&conv).1, 2);
        log.drop_conversation(&conv).unwrap();
        assert_eq!(log.fingerprint_and_len(&conv), empty);
    }

    #[test]
    fn authored_message_cursor_rebuilds_on_reopen_and_drops_with_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let author = DeviceIdentity::generate();
        let conversation = ConversationId::new([1; 32]);
        let event = mk(&author, 1, 1, b"retained");
        let key = (1, event.id);
        let by = crate::eventlog::event::Author::from_ed25519(author.public().ed25519_pub);
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        log.append(event).unwrap();
        drop(log);
        let mut reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.message_keys_after(conversation, by, None, 1),
            vec![key]
        );
        reopened.drop_conversation(&conversation).unwrap();
        assert!(
            reopened
                .message_keys_after(conversation, by, None, 1)
                .is_empty()
        );
    }

    #[test]
    fn durable_transaction_append_is_idempotent_and_preserves_parent_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let id = DeviceIdentity::generate();
        let parent = mk(&id, 1, 1, b"parent");
        let child = Event::new(
            &id,
            parent.conversation_id,
            2,
            vec![parent.id],
            2,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        log.append(parent.clone()).unwrap();
        log.sync().unwrap();
        assert_eq!(
            log.append_durable(child.clone()).unwrap(),
            AppendOutcome::Appended
        );
        assert_eq!(
            log.append_durable(child.clone()).unwrap(),
            AppendOutcome::Duplicate
        );
        drop(log);
        let log = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(log.get(&parent.id), Some(&parent));
        assert_eq!(log.get(&child.id), Some(&child));
        assert_eq!(log.conversation_len(&parent.conversation_id), 2);
    }

    #[test]
    fn channel_batch_appears_on_disk_only_after_atomic_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let id = DeviceIdentity::generate();
        let parent = mk(&id, 1, 1, b"parent");
        let child = Event::new(
            &id,
            parent.conversation_id,
            2,
            vec![parent.id],
            2,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let before_rename = path.clone();
        let conversation = parent.conversation_id;
        log.before_rewrite_rename(Box::new(move || {
            let old = PersistentEventLog::open(&before_rename, "pw").unwrap();
            assert!(old.events(&conversation).is_empty());
        }));
        log.append_channel_batch_atomic(&[parent.clone(), child.clone()])
            .unwrap();
        let reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(reopened.events(&parent.conversation_id).len(), 2);
    }

    #[test]
    fn compacts_superseded_unreferenced_profiles_keeping_latest_pair_and_referenced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let id = DeviceIdentity::generate();
        let conv = ConversationId::new([1u8; 32]);

        // A real message stays in the head frontier and is referenced by the profiles.
        let msg = mk(&id, 1, 1, b"hi");
        let msg_id = msg.id;
        log.append(msg).unwrap();

        // 10 profile events from the same author, each anchored to the message (so the
        // message is referenced/kept) but never referenced themselves (non-head leaves).
        let mut newest_id = msg_id;
        for i in 0..10u64 {
            let p = Event::new(
                &id,
                conv,
                2 + i,
                vec![msg_id],
                2 + i,
                0,
                EventKind::Profile,
                format!("avatar{i}").into_bytes(),
            );
            newest_id = p.id;
            log.append(p).unwrap();
        }
        assert_eq!(log.events(&conv).len(), 11); // 1 message + 10 profiles

        let dropped = log.compact_superseded_profiles(&conv).unwrap();
        assert_eq!(dropped, 8); // Preserve the latest legacy-avatar / signed-card pair.

        let remaining = log.events(&conv);
        assert_eq!(
            remaining.len(),
            3,
            "the message and latest profile pair remain"
        );
        assert!(
            remaining.iter().any(|e| e.id == msg_id),
            "the referenced message is never dropped"
        );
        assert!(
            remaining.iter().any(|e| e.id == newest_id),
            "the newest profile is kept (deterministic: highest lamport)"
        );
        assert!(
            remaining.iter().any(|e| e.ciphertext == b"avatar8"),
            "the other leaf of the latest publication pair is kept"
        );

        // The on-disk log was rewritten too — survives a reopen.
        drop(log);
        let log2 = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(log2.events(&conv).len(), 3);
    }

    #[test]
    fn compaction_never_drops_a_referenced_profile() {
        // THE safety invariant: a superseded profile that a later event references as a parent
        // (the legacy / cross-version case, where profiles used to be heads) must be KEPT —
        // dropping it would dangle that child → MissingParents → message loss. Only
        // UNREFERENCED superseded profiles are ever dropped.
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let id = DeviceIdentity::generate();
        let conv = ConversationId::new([1u8; 32]);

        let msg = mk(&id, 1, 1, b"hi");
        let msg_id = msg.id;
        log.append(msg).unwrap();

        // An old profile, then a MESSAGE that references it as a parent (the legacy shape).
        let p0 = Event::new(
            &id,
            conv,
            2,
            vec![msg_id],
            2,
            0,
            EventKind::Profile,
            b"old".to_vec(),
        );
        let p0_id = p0.id;
        log.append(p0).unwrap();
        let m2 = Event::new(
            &id,
            conv,
            3,
            vec![p0_id],
            3,
            0,
            EventKind::Message,
            b"m2".to_vec(),
        );
        log.append(m2).unwrap();

        // 10 newer, unreferenced profiles → 8 droppable, latest pair kept.
        let mut newest_id = p0_id;
        for i in 0..10u64 {
            let p = Event::new(
                &id,
                conv,
                4 + i,
                vec![msg_id],
                4 + i,
                0,
                EventKind::Profile,
                format!("new{i}").into_bytes(),
            );
            newest_id = p.id;
            log.append(p).unwrap();
        }

        let dropped = log.compact_superseded_profiles(&conv).unwrap();
        assert_eq!(
            dropped, 8,
            "only the 8 superseded UNREFERENCED profiles drop"
        );
        let ids: Vec<_> = log.events(&conv).iter().map(|e| e.id).collect();
        assert!(
            ids.contains(&p0_id),
            "the referenced (superseded) profile is KEPT — never dangle its child"
        );
        assert!(
            ids.contains(&newest_id),
            "the newest unreferenced profile is kept"
        );
        // Every remaining event's parents are still present (no dangling parents).
        let present: std::collections::HashSet<_> = ids.iter().copied().collect();
        for e in log.events(&conv) {
            for parent in &e.parents {
                assert!(
                    present.contains(parent),
                    "compaction left a dangling parent"
                );
            }
        }
    }

    #[test]
    fn below_threshold_profiles_are_not_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let id = DeviceIdentity::generate();
        let conv = ConversationId::new([1u8; 32]);
        let msg = mk(&id, 1, 1, b"hi");
        let msg_id = msg.id;
        log.append(msg).unwrap();
        // Only 3 profiles → 1 droppable < COMPACT_PROFILE_MIN → left as-is (amortization).
        for i in 0..3u64 {
            let p = Event::new(
                &id,
                conv,
                2 + i,
                vec![msg_id],
                2 + i,
                0,
                EventKind::Profile,
                format!("a{i}").into_bytes(),
            );
            log.append(p).unwrap();
        }
        assert_eq!(log.compact_superseded_profiles(&conv).unwrap(), 0);
        assert_eq!(log.events(&conv).len(), 4);
    }

    fn append_profile_batch(
        log: &mut PersistentEventLog,
        author: &DeviceIdentity,
        conversation: ConversationId,
        start: u64,
    ) {
        for seq in start..start + 10 {
            log.append(Event::new(
                author,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Profile,
                format!("avatar{seq}").into_bytes(),
            ))
            .unwrap();
        }
    }

    #[test]
    fn profile_compaction_does_not_rewrite_large_history_for_tiny_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let id = DeviceIdentity::generate();
        log.append(mk(&id, 1, 1, &vec![42; 1024 * 1024])).unwrap();
        let conversation = ConversationId::new([2; 32]);
        append_profile_batch(&mut log, &id, conversation, 1);
        log.sync().unwrap();
        let before = std::fs::read(&path).unwrap();
        log.request_profile_compaction(conversation);
        assert_eq!(
            log.drain_one_profile_compaction().unwrap(),
            Some(0),
            "eight tiny stale profiles must not rewrite unrelated megabytes"
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(log.events(&conversation).len(), 10);
        assert_eq!(log.pending_profile_compactions(), 1);
    }

    #[test]
    fn profile_compaction_accumulates_deferred_garbage_across_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let id = DeviceIdentity::generate();
        let history = vec![42; 1024 * 1024];
        log.append(mk(&id, 1, 1, &history)).unwrap();
        let now = Instant::now();
        let first = ConversationId::new([2; 32]);
        let second = ConversationId::new([3; 32]);
        for (index, conversation) in [first, second].into_iter().enumerate() {
            for seq in 1..=10 {
                log.append(Event::new(
                    &id,
                    conversation,
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Profile,
                    vec![7; 16 * 1024],
                ))
                .unwrap();
            }
            log.request_profile_compaction(conversation);
            let result = log
                .drain_one_profile_compaction_at(now + Duration::from_secs(index as u64 * 11))
                .unwrap();
            assert_eq!(result, Some(if index == 0 { 0 } else { 16 }));
        }
        assert_eq!(log.rewrite_count, 1);
        assert_eq!(log.pending_profile_compactions(), 0);
        assert_eq!(log.events(&first).len(), 2);
        assert_eq!(log.events(&second).len(), 2);
    }

    #[test]
    fn failed_profile_compaction_batch_preserves_all_requests_and_retries_after_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let id = DeviceIdentity::generate();
        let first = ConversationId::new([1; 32]);
        let second = ConversationId::new([2; 32]);
        for conversation in [first, second] {
            append_profile_batch(&mut log, &id, conversation, 1);
            log.request_profile_compaction(conversation);
        }
        log.sync().unwrap();
        let before = std::fs::read(&path).unwrap();
        let blocked = path.with_extension("compact-tmp");
        std::fs::create_dir(&blocked).unwrap();
        let now = Instant::now();
        assert!(log.drain_one_profile_compaction_at(now).is_err());
        assert_eq!(log.pending_profile_compactions(), 2);
        assert_eq!(log.rewrite_count, 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(log.events(&first).len(), 10);
        assert_eq!(log.events(&second).len(), 10);
        std::fs::remove_dir(blocked).unwrap();
        assert_eq!(
            log.drain_one_profile_compaction_at(now + Duration::from_secs(1))
                .unwrap(),
            None
        );
        assert_eq!(
            log.drain_one_profile_compaction_at(now + Duration::from_secs(61))
                .unwrap(),
            Some(16)
        );
        assert_eq!(log.pending_profile_compactions(), 0);
        assert_eq!(log.rewrite_count, 1);
        drop(log);
        let reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(reopened.events(&first).len(), 2);
        assert_eq!(reopened.events(&second).len(), 2);
    }

    #[test]
    fn profile_compaction_batches_pending_conversations_into_one_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let id = DeviceIdentity::generate();
        let first = ConversationId::new([1; 32]);
        let second = ConversationId::new([2; 32]);
        for conversation in [first, second] {
            append_profile_batch(&mut log, &id, conversation, 1);
            log.request_profile_compaction(conversation);
        }
        assert_eq!(log.drain_one_profile_compaction().unwrap(), Some(16));
        assert_eq!(log.rewrite_count, 1);
        assert_eq!(log.pending_profile_compactions(), 0);
        assert_eq!(log.events(&first).len(), 2);
        assert_eq!(log.events(&second).len(), 2);
    }

    #[test]
    fn profile_compaction_rewrite_cooldown_is_shared_across_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let id = DeviceIdentity::generate();
        let first = ConversationId::new([1; 32]);
        let second = ConversationId::new([2; 32]);
        let now = Instant::now();
        append_profile_batch(&mut log, &id, first, 1);
        log.request_profile_compaction(first);
        assert_eq!(log.drain_one_profile_compaction_at(now).unwrap(), Some(8));
        append_profile_batch(&mut log, &id, second, 1);
        log.request_profile_compaction(second);
        assert_eq!(
            log.drain_one_profile_compaction_at(now + Duration::from_secs(1))
                .unwrap(),
            None,
            "a new conversation cannot bypass the whole-file rewrite cooldown"
        );
        assert_eq!(log.pending_profile_compactions(), 1);
        assert_eq!(
            log.drain_one_profile_compaction_at(now + Duration::from_secs(61))
                .unwrap(),
            Some(8)
        );
    }

    #[test]
    fn profile_compaction_requests_are_coalesced() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let first = ConversationId::new([1u8; 32]);
        let second = ConversationId::new([2u8; 32]);

        log.request_profile_compaction(first);
        log.request_profile_compaction(first);
        log.request_profile_compaction(second);

        assert_eq!(log.pending_profile_compactions(), 2);
        assert_eq!(log.drain_one_profile_compaction().unwrap(), Some(0));
        assert_eq!(log.pending_profile_compactions(), 0);
        assert_eq!(log.drain_one_profile_compaction().unwrap(), None);
    }

    #[test]
    fn profile_compaction_noop_scans_are_rate_limited_across_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let now = Instant::now();
        log.request_profile_compaction(ConversationId::new([1; 32]));
        assert_eq!(log.drain_one_profile_compaction_at(now).unwrap(), Some(0));
        log.request_profile_compaction(ConversationId::new([2; 32]));
        assert_eq!(
            log.drain_one_profile_compaction_at(now + Duration::from_secs(1))
                .unwrap(),
            None
        );
        assert_eq!(
            log.drain_one_profile_compaction_at(now + Duration::from_secs(11))
                .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn profile_compaction_respects_per_conversation_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("log"), "pw").unwrap();
        let conversation = ConversationId::new([3u8; 32]);

        log.request_profile_compaction(conversation);
        assert_eq!(log.drain_one_profile_compaction().unwrap(), Some(0));

        log.request_profile_compaction(conversation);
        assert_eq!(log.drain_one_profile_compaction().unwrap(), None);
        assert_eq!(log.pending_profile_compactions(), 1);

        let after_cooldown = Instant::now() + PROFILE_COMPACTION_COOLDOWN + Duration::from_secs(1);
        assert_eq!(
            log.drain_one_profile_compaction_at(after_cooldown).unwrap(),
            Some(0)
        );
        assert_eq!(log.pending_profile_compactions(), 0);
    }

    #[test]
    fn failed_profile_compaction_keeps_original_log_and_pending_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let id = DeviceIdentity::generate();
        let conversation = ConversationId::new([1u8; 32]);

        let message = mk(&id, 1, 1, b"hi");
        let message_id = message.id;
        log.append(message).unwrap();
        for i in 0..10u64 {
            log.append(Event::new(
                &id,
                conversation,
                2 + i,
                vec![message_id],
                2 + i,
                0,
                EventKind::Profile,
                format!("avatar{i}").into_bytes(),
            ))
            .unwrap();
        }

        // Make the temporary rewrite target unusable. The original file must remain readable,
        // and the maintenance request must remain queued for a later retry.
        std::fs::create_dir(path.with_extension("compact-tmp")).unwrap();
        log.request_profile_compaction(conversation);
        assert!(log.drain_one_profile_compaction().is_err());
        assert_eq!(log.pending_profile_compactions(), 1);

        drop(log);
        let reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(reopened.events(&conversation).len(), 11);
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
            reconcile(&mut dest, &mut source, conv).unwrap();
            assert!(dest.has(&root.id) && dest.has(&child.id));
            // Sanity: the SyncStore view agrees.
            assert_eq!(SyncStore::event_ids(&dest, &conv).unwrap().len(), 2);
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
        let (parents, lamport) = plog.prepare(&conv).unwrap();
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
