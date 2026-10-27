//! Event-log reconciliation ("sync"). Two peers converge a conversation's log
//! by exchanging the set of event ids each holds; the responder returns exactly
//! the events the requester is missing, in topological order. Received events
//! are validated through [`SyncStore::ingest`] (the store's normal `append`
//! gate), so sync can never inject forged or out-of-order history.
//!
//! The public helpers in this module are collecting SDK operations, including
//! payload-bounded helpers: they may collect all IDs or traverse complete history.
//! The network-specific mandatory paging seam lives in [`network`].
//!
//! Reconciliation is id-set based, not version-vector based: the store does not
//! guarantee dense per-author sequences, so a version-vector diff would miss a
//! gap. See the module plan for the rationale.
//!
//! [`SyncStore::events_excluding`] traverses signed parent edges before filtering
//! known ids. Lamport values only break ties between causally independent events,
//! so even signed events with inverted clocks make progress across bounded frames.

pub mod network;

use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, Event, EventId, EventKind};
use crate::eventlog::store::{AppendOutcome, EventLog};
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// Bytes reserved per sync frame for the message's non-event framing (enum tag,
/// conversation id, bincode wrappers) on top of the event/id payload.
pub(crate) const SYNC_MARGIN: usize = 256;

/// `cap_events` guarantees progress by always emitting at least the first event — which is
/// only safe if a single event fits one sync frame, else the responder emits an oversized
/// `Response` the transport rejects and that conversation can never sync past it. The
/// largest event is a file-chunk: `CHUNK_SIZE` ciphertext + AEAD tag + event metadata.
///
/// `MAX_EVENT_METADATA_OVERHEAD` covers the fixed header (~220 B) plus a generous parent
/// allowance — it is NOT a hard cap on metadata: `parents` (the conversation frontier) is
/// unbounded at 32 B each, so ~120 concurrent heads exceed 4096 and a chunk event would need
/// ~375 to actually breach a frame — orders of magnitude beyond any realistic LAN frontier.
/// The compile-time assert below pins the common-case relationship so a `CHUNK_SIZE` bump (or
/// a new large event kind) breaks the build here; the test `single_large_event_fits_one_frame`
/// pins it against a real encoded event. (A `MAX_PARENTS` reject is NOT the fix — capping the
/// frontier would break the hash-linked causal merge.)
const MAX_EVENT_METADATA_OVERHEAD: usize = 4096;
const _: () = assert!(
    crate::file::crypto::CHUNK_SIZE + 16 + MAX_EVENT_METADATA_OVERHEAD + SYNC_MARGIN
        < crate::transport::MAX_PLAINTEXT
);

/// Sent by the peer initiating sync: "for this conversation, here are the ids I have."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub conversation: ConversationId,
    pub have: Vec<EventId>,
}

/// The responder's reply: the events the requester is missing (topological
/// order), plus the responder's own id-set so the requester can reciprocate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
    /// The responder's OWN event ids (note: `SyncRequest::have` is the
    /// requester's). The requester uses these to compute the events the
    /// responder is missing, returned via [`SyncFollowup`].
    pub have: Vec<EventId>,
}

/// The requester's final message: the events the responder is missing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncFollowup {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
}

/// A whole-conversation fingerprint probe sent before the id-set exchange: "here is a
/// digest of my ENTIRE event-id set for this conversation." If the responder's digest
/// matches, the two logs are identical and the O(N) have-set streaming is skipped — the
/// common case on an idle drain tick (nothing changed since last sync).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpRequest {
    pub conversation: ConversationId,
    pub fingerprint: [u8; 32],
}

/// The responder's verdict on an [`FpRequest`]: `true` ⇒ our id-sets are identical, skip
/// reconciliation; `false` ⇒ fall through to the full id-set exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpResponse {
    pub matched: bool,
}

/// A collision-resistant digest of a conversation's entire event-id SET. Order-independent
/// (ids are sorted first) so two peers holding the same set always compute the same value,
/// regardless of internal storage order. A match means the sets are equal → reconciliation
/// can be skipped; because every event is signed and re-validated on ingest, a
/// (cryptographically negligible) false match could at worst skip a real difference —
/// recovered on a later sync — and can NEVER inject or corrupt history.
pub fn fingerprint(mut ids: Vec<EventId>) -> [u8; 32] {
    ids.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update((ids.len() as u64).to_le_bytes());
    for id in &ids {
        hasher.update(id.as_bytes());
    }
    hasher.finalize().into()
}

/// Outcome of ingesting a batch of received events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Events newly appended.
    pub applied: usize,
    /// Events already present (content-addressed duplicates).
    pub duplicates: usize,
    /// Events rejected by validation, with a human-readable reason. (id, message)
    /// Typed validation/admission errors are classified before formatting these
    /// bounded diagnostics. Local store failures abort sync instead.
    pub rejected: Vec<(EventId, String)>,
}

/// Collecting compatibility operations for the in-process SDK. Network stores
/// use the independent [`network::NetworkSyncStore`] seam.
/// The data operations sync needs from an event store. Implemented by both the
/// in-memory [`EventLog`] and the durable `PersistentEventLog`.
pub trait SyncStore {
    /// True if the visible log can contain event kinds introduced with channel v2.
    /// Custom stores take the safe scanning path unless they provide an index.
    fn requires_channel_v2(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(self
            .events_excluding(conversation, &HashSet::new())?
            .iter()
            .any(|event| event.kind == EventKind::ChannelReplica))
    }
    /// True when visible history controls require kinds 15/16 support.
    fn requires_channel_v3(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(self
            .events_excluding(conversation, &HashSet::new())?
            .iter()
            .any(|event| {
                matches!(
                    event.kind,
                    EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory
                )
            }))
    }
    /// Account grants and certified-device inheritance require kinds 17/18.
    fn requires_channel_v4(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(self
            .events_excluding(conversation, &HashSet::new())?
            .iter()
            .any(|event| {
                matches!(
                    event.kind,
                    EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
                )
            }))
    }
    /// Digest and size of the currently visible event set. Implementations with
    /// unfiltered durable logs may cache this across idle recovery ticks.
    fn fingerprint_and_len(
        &self,
        conversation: &ConversationId,
    ) -> Result<([u8; 32], usize), LogError> {
        let ids = self.event_ids(conversation)?;
        let len = ids.len();
        Ok((fingerprint(ids), len))
    }
    /// The authenticated peer may transfer chunks of this exact offered file.
    /// Stores without an attachment policy fail closed for this optional query.
    fn file_consent(
        &self,
        _file: &ConversationId,
        _manifest: &EventId,
    ) -> Result<FileConsentStatus, LogError> {
        Ok(FileConsentStatus::Pending)
    }
    /// Optional ordered consent for modern endpoints. The signed answer shares
    /// the mailbox revision space; legacy stores can retain their existing query.
    fn signed_file_consent(
        &self,
        _file: &ConversationId,
        _manifest: &EventId,
    ) -> Result<Option<Event>, LogError> {
        Ok(None)
    }
    /// Optional custody qualification. Memory/legacy stores cannot promise
    /// durability; durable hosts synchronize and check current visibility.
    fn durable_have(
        &self,
        _conversation: &ConversationId,
        _id: &EventId,
    ) -> Result<bool, LogError> {
        Ok(false)
    }
    /// A host admission denial stops the wire session before further disclosure.
    /// Ordinary stores preserve the protocol's existing validation behavior.
    fn admission_denied(&self) -> bool {
        false
    }
    /// All event ids this store holds for the conversation.
    fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError>;
    /// The conversation's events whose id is NOT in `have`, cloned, in
    /// **topological order**: every event's parents must precede it in the
    /// returned slice or already be in `have`, so a requester can ingest them
    /// one-by-one without a spurious `MissingParents`. Parent edges, rather than
    /// signed clocks, determine this order; filter `have` only after traversal.
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Result<Vec<Event>, LogError>;
    /// Export only the causal prefix needed for at most `max_frames` greedy
    /// frames, each with `event_budget` bytes. The first event of each frame
    /// is retained even when oversized, matching ordinary sync's progress rule.
    /// Implementations may select borrowed events before cloning ciphertext;
    /// the default preserves existing custom-store admission and ordering.
    fn events_excluding_bounded(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
        event_budget: usize,
        max_frames: usize,
    ) -> Result<Vec<Event>, LogError> {
        Ok(cap_event_frames(
            self.events_excluding(conversation, have)?,
            event_budget,
            max_frames,
        ))
    }
    /// Validate and store a received event (the store's normal append gate).
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FileConsentStatus {
    Pending,
    Accepted,
    Declined,
}

// NOTE: this impl is structurally identical to `impl SyncStore for
// PersistentEventLog` in persist.rs — keep the two in sync.
impl SyncStore for EventLog {
    fn requires_channel_v3(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(EventLog::requires_channel_v3(self, conversation))
    }
    fn requires_channel_v4(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(EventLog::requires_channel_v4(self, conversation))
    }
    fn requires_channel_v2(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(EventLog::requires_channel_v2(self, conversation))
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
        UNBOUNDED_EXPORTS.with(|count| count.set(count.get() + 1));
        Ok(causal_events(self.events(conversation))
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
        Ok(bounded_events(
            causal_events(self.events(conversation))
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
thread_local! {
    static UNBOUNDED_EXPORTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Traverse the complete retained DAG before policy or requester-have filtering.
/// Preserves compatibility with accepted signed events whose clocks are inverted.
pub(crate) fn causal_events(events: Vec<&Event>) -> Vec<&Event> {
    let order = super::order::ordered_ids(&events, &[], true);
    let by_id: HashMap<_, _> = events.into_iter().map(|event| (event.id, event)).collect();
    order.into_iter().map(|id| by_id[&id]).collect()
}

/// The fixint-bincode encoded size of one event (matches `node::session::encode`).
pub(crate) fn encoded_event_size(event: &Event) -> usize {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialized_size(event)
        .map(|n| n as usize)
        .unwrap_or(usize::MAX)
}

/// The longest greedy prefix fitting at most `max_frames` frame budgets.
/// Retain an oversized first event per frame to preserve the sync progress rule.
fn frame_prefix<T>(
    events: impl IntoIterator<Item = T>,
    budget: usize,
    max_frames: usize,
    size: impl Fn(&T) -> usize,
) -> impl Iterator<Item = T> {
    let mut used = 0usize;
    let mut frames = 0usize;
    let mut frame_has_event = false;
    events.into_iter().take_while(move |event| {
        if max_frames == 0 {
            return false;
        }
        let bytes = size(event);
        if frame_has_event && used.saturating_add(bytes) > budget {
            frames += 1;
            if frames == max_frames {
                return false;
            }
            used = 0;
        }
        used = used.saturating_add(bytes);
        frame_has_event = true;
        true
    })
}

fn cap_event_frames(events: Vec<Event>, budget: usize, max_frames: usize) -> Vec<Event> {
    frame_prefix(events, budget, max_frames, encoded_event_size).collect()
}

#[cfg(test)]
fn cap_events(events: Vec<Event>, budget: usize) -> Vec<Event> {
    cap_event_frames(events, budget, 1)
}

/// Select borrowed causal events before cloning, so frame truncation never
/// allocates the ciphertext tail of a large missing-history backlog.
pub(crate) fn bounded_events<'a>(
    events: impl IntoIterator<Item = &'a Event>,
    event_budget: usize,
    max_frames: usize,
) -> Vec<Event> {
    frame_prefix(events, event_budget, max_frames, |event| {
        encoded_event_size(event)
    })
    .cloned()
    .collect()
}

/// Build the opening request: the ids this store holds for `conversation`.
pub fn build_request(
    store: &impl SyncStore,
    conversation: ConversationId,
) -> Result<SyncRequest, LogError> {
    Ok(SyncRequest {
        conversation,
        have: store.event_ids(&conversation)?,
    })
}

/// Answer a request, bounding the returned events so the encoded `SyncResponse`
/// (events + the responder's `have`) stays within `frame_budget` bytes. The
/// requester pulls the remainder over subsequent rounds (it learns the full set
/// from `have`).
pub fn handle_request_bounded(
    store: &impl SyncStore,
    request: &SyncRequest,
    frame_budget: usize,
) -> Result<SyncResponse, LogError> {
    let requester_have: HashSet<EventId> = request.have.iter().copied().collect();
    let responder_have = store.event_ids(&request.conversation)?;
    // The responder's `have` is streamed in its own frames (node::session), so it no
    // longer competes with events for this frame's budget — events get the whole frame.
    let event_budget = frame_budget.saturating_sub(SYNC_MARGIN);
    let events =
        store.events_excluding_bounded(&request.conversation, &requester_have, event_budget, 1)?;
    Ok(SyncResponse {
        conversation: request.conversation,
        events,
        have: responder_have,
    })
}

/// Unbounded answer (in-process / single-round callers). Equivalent to
/// [`handle_request_bounded`] with no frame limit.
pub fn handle_request(
    store: &impl SyncStore,
    request: &SyncRequest,
) -> Result<SyncResponse, LogError> {
    handle_request_bounded(store, request, usize::MAX)
}

/// Cap on rejected-event DETAILS recorded per batch. `rejected` is diagnostics only and
/// never drives control flow, so a peer flooding invalid events cannot grow it without
/// bound — rejections past the cap still occur, they're just not individually itemised.
pub(crate) const MAX_REJECTED_DETAIL: usize = 64;

/// Ingest a batch of received events through the store's validation gate,
/// tallying outcomes. An invalid event is rejected and recorded, not fatal —
/// the rest of the batch still applies. The recorded detail is capped
/// ([`MAX_REJECTED_DETAIL`]) so a malicious batch can't inflate the report's memory.
/// Local failures abort immediately; already committed events remain stored.
fn ingest_all(store: &mut impl SyncStore, events: &[Event]) -> Result<ApplyReport, LogError> {
    network::apply_received_events(events, |event| store.ingest(event))
}

/// Apply a responder's reply and build a (bounded) follow-up of events the
/// responder is missing.
pub fn handle_response_bounded(
    store: &mut impl SyncStore,
    response: &SyncResponse,
    frame_budget: usize,
) -> Result<(ApplyReport, SyncFollowup), LogError> {
    let (report, mut frames) = handle_response_bounded_burst(store, response, frame_budget, 1)?;
    Ok((
        report,
        frames.pop().unwrap_or(SyncFollowup {
            conversation: response.conversation,
            events: Vec::new(),
        }),
    ))
}

/// Pack several ordinary Followup frames from one reconciliation. A file transfer
/// can push a bounded burst without re-advertising its entire growing id set after
/// every chunk. Each frame remains independently validated by the old responder.
pub fn handle_response_bounded_burst(
    store: &mut impl SyncStore,
    response: &SyncResponse,
    frame_budget: usize,
    max_frames: usize,
) -> Result<(ApplyReport, Vec<SyncFollowup>), LogError> {
    let report = ingest_all(store, &response.events)?;
    let remote_have: HashSet<EventId> = response.have.iter().copied().collect();
    let budget = frame_budget.saturating_sub(SYNC_MARGIN);
    let all_missing =
        store.events_excluding_bounded(&response.conversation, &remote_have, budget, max_frames)?;
    let frames = followup_frames(response.conversation, all_missing, budget, max_frames);
    Ok((report, frames))
}

/// Shared legacy greedy packing policy; payload bounds do not imply a bounded
/// source traversal. Preserve the SDK's oversized-first-event progress behavior.
pub(crate) fn followup_frames(
    conversation: ConversationId,
    all_missing: Vec<Event>,
    budget: usize,
    max_frames: usize,
) -> Vec<SyncFollowup> {
    let mut frames = Vec::new();
    let mut events = Vec::new();
    let mut used = 0usize;
    for event in all_missing {
        let size = encoded_event_size(&event);
        if !events.is_empty() && used.saturating_add(size) > budget {
            frames.push(SyncFollowup {
                conversation,
                events: std::mem::take(&mut events),
            });
            if frames.len() == max_frames {
                break;
            }
            used = 0;
        }
        used = used.saturating_add(size);
        events.push(event);
    }
    if !events.is_empty() && frames.len() < max_frames {
        frames.push(SyncFollowup {
            conversation,
            events,
        });
    }
    frames
}

/// Apply a responder's reply: ingest the events it sent, then compute the
/// events IT is missing (so the caller can send a [`SyncFollowup`]).
pub fn handle_response(
    store: &mut impl SyncStore,
    response: &SyncResponse,
) -> Result<(ApplyReport, SyncFollowup), LogError> {
    handle_response_bounded(store, response, usize::MAX)
}

/// Apply the requester's follow-up: ingest the events it sent.
pub fn handle_followup(
    store: &mut impl SyncStore,
    followup: &SyncFollowup,
) -> Result<ApplyReport, LogError> {
    ingest_all(store, &followup.events)
}

/// Run a full in-process reconciliation between two stores for one conversation.
/// `a` is the requester, `b` the responder. Afterward both hold the union of the
/// **valid** events each can accept: events that fail [`SyncStore::ingest`] (bad
/// signature, missing parent, equivocation) are skipped on both sides and listed
/// in the returned [`ApplyReport`]s. Returns `(report_a, report_b)`.
/// A local read/write failure returns `Err` and preserves any committed prefix.
///
/// This is the reference in-process driver (over a network the three messages
/// travel across a channel). It is PAIRWISE: transitive propagation across three
/// or more peers requires chaining `reconcile` calls (e.g. A↔B then B↔C).
pub fn reconcile<A: SyncStore, B: SyncStore>(
    a: &mut A,
    b: &mut B,
    conversation: ConversationId,
) -> Result<(ApplyReport, ApplyReport), LogError> {
    let request = build_request(a, conversation)?;
    let response = handle_request(b, &request)?;
    let (report_a, followup) = handle_response(a, &response)?;
    let report_b = handle_followup(b, &followup)?;
    Ok((report_a, report_b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::EventKind;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    #[derive(Clone, Copy)]
    enum Fault {
        Ids,
        Export,
        Metadata,
        Durable,
        Ingest(usize),
    }

    struct FailingStore {
        log: EventLog,
        fault: Fault,
        ingested: usize,
    }
    impl FailingStore {
        fn new(fault: Fault) -> Self {
            Self {
                log: EventLog::default(),
                fault,
                ingested: 0,
            }
        }
        fn failure() -> LogError {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected local disk failure",
            )
            .into()
        }
    }
    impl SyncStore for FailingStore {
        fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
            if matches!(self.fault, Fault::Ids) {
                return Err(Self::failure());
            }
            SyncStore::event_ids(&self.log, conversation)
        }
        fn events_excluding(
            &self,
            conversation: &ConversationId,
            have: &HashSet<EventId>,
        ) -> Result<Vec<Event>, LogError> {
            if matches!(self.fault, Fault::Export) {
                return Err(Self::failure());
            }
            SyncStore::events_excluding(&self.log, conversation, have)
        }
        fn fingerprint_and_len(
            &self,
            conversation: &ConversationId,
        ) -> Result<([u8; 32], usize), LogError> {
            if matches!(self.fault, Fault::Metadata) {
                return Err(Self::failure());
            }
            let ids = self.event_ids(conversation)?;
            let len = ids.len();
            Ok((fingerprint(ids), len))
        }
        fn durable_have(
            &self,
            _conversation: &ConversationId,
            _id: &EventId,
        ) -> Result<bool, LogError> {
            if matches!(self.fault, Fault::Durable) {
                return Err(Self::failure());
            }
            Ok(false)
        }
        fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
            if matches!(self.fault, Fault::Ingest(n) if n == self.ingested) {
                return Err(Self::failure());
            }
            self.ingested += 1;
            self.log.append(event)
        }
    }

    #[test]
    fn local_ingest_failure_is_not_peer_rejection() {
        let identity = DeviceIdentity::generate();
        let event = ev(&identity, 0, vec![], 1, b"hello");
        let result = handle_followup(
            &mut FailingStore::new(Fault::Ingest(0)),
            &SyncFollowup {
                conversation: conv(),
                events: vec![event],
            },
        );
        assert!(
            matches!(result, Err(LogError::Io(_))),
            "local disk failure must abort sync, not reject a peer event"
        );
    }

    #[test]
    fn failed_reads_never_produce_successful_empty_sync() {
        let request = SyncRequest {
            conversation: conv(),
            have: vec![],
        };
        assert!(matches!(
            build_request(&FailingStore::new(Fault::Ids), conv()),
            Err(LogError::Io(_))
        ));
        assert!(matches!(
            handle_request(&FailingStore::new(Fault::Ids), &request),
            Err(LogError::Io(_))
        ));
        assert!(matches!(
            handle_request(&FailingStore::new(Fault::Export), &request),
            Err(LogError::Io(_))
        ));
        assert!(matches!(
            FailingStore::new(Fault::Export).requires_channel_v4(&conv()),
            Err(LogError::Io(_))
        ));
        assert!(matches!(
            FailingStore::new(Fault::Metadata).fingerprint_and_len(&conv()),
            Err(LogError::Io(_))
        ));
        assert!(matches!(
            FailingStore::new(Fault::Durable).durable_have(&conv(), &EventId::new([0; 32])),
            Err(LogError::Io(_))
        ));
        let response = SyncResponse {
            conversation: conv(),
            events: vec![],
            have: vec![],
        };
        assert!(matches!(
            handle_response(&mut FailingStore::new(Fault::Export), &response),
            Err(LogError::Io(_))
        ));
    }

    #[test]
    fn midbatch_local_failure_preserves_durable_prefix_after_reopen() {
        use crate::eventlog::PersistentEventLog;
        struct FailSecondAppend {
            log: PersistentEventLog,
            ingested: usize,
            failure_path: std::path::PathBuf,
        }
        impl SyncStore for FailSecondAppend {
            fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
                SyncStore::event_ids(&self.log, conversation)
            }
            fn events_excluding(
                &self,
                conversation: &ConversationId,
                have: &HashSet<EventId>,
            ) -> Result<Vec<Event>, LogError> {
                SyncStore::events_excluding(&self.log, conversation, have)
            }
            fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
                if self.ingested == 1 {
                    return Err(std::fs::File::open(&self.failure_path).unwrap_err().into());
                }
                self.ingested += 1;
                self.log.append_durable(event)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prefix.log");
        let identity = DeviceIdentity::generate();
        let first = ev(&identity, 0, vec![], 1, b"first");
        let second = ev(&identity, 1, vec![first.id], 2, b"second");
        let mut store = FailSecondAppend {
            log: PersistentEventLog::open(&path, "pw").unwrap(),
            ingested: 0,
            failure_path: directory.path().join("missing-local-file"),
        };
        let result = handle_followup(
            &mut store,
            &SyncFollowup {
                conversation: conv(),
                events: vec![first.clone(), second.clone()],
            },
        );
        assert!(matches!(result, Err(LogError::Io(_))));
        drop(store);
        let reopened = PersistentEventLog::open(&path, "pw").unwrap();
        assert!(reopened.has(&first.id));
        assert!(!reopened.has(&second.id));
    }

    #[test]
    fn validation_rejections_continue_while_every_local_error_aborts() {
        let identity = DeviceIdentity::generate();
        let event = ev(&identity, 0, vec![], 1, b"valid");
        let mut invalid = event.clone();
        invalid.sig[0] ^= 1;
        let report = handle_followup(
            &mut EventLog::default(),
            &SyncFollowup {
                conversation: conv(),
                events: vec![invalid, event],
            },
        )
        .unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.rejected.len(), 1);
        for error in [
            LogError::ClockExhausted,
            LogError::SequenceExhausted,
            LogError::Serialization("failed".into()),
            LogError::CorruptFile("failed".into()),
            FailingStore::failure(),
        ] {
            assert!(!error.is_peer_rejection(), "{error}");
        }
        assert!(LogError::AdmissionRejected("denied".into()).is_peer_rejection());
    }

    #[test]
    fn fingerprint_is_order_independent_and_set_sensitive() {
        let a = EventId::new([1u8; 32]);
        let b = EventId::new([2u8; 32]);
        let c = EventId::new([3u8; 32]);
        // Same SET in any order ⇒ identical fingerprint (so two peers always agree).
        assert_eq!(fingerprint(vec![a, b, c]), fingerprint(vec![c, a, b]));
        // A different set ⇒ different fingerprint (membership AND count are covered).
        assert_ne!(fingerprint(vec![a, b]), fingerprint(vec![a, b, c]));
        assert_ne!(fingerprint(vec![a, b]), fingerprint(vec![a, c]));
        // Empty sets agree (an empty conversation also short-circuits).
        assert_eq!(fingerprint(vec![]), fingerprint(vec![]));
    }

    /// Build a signed event in `conv()`.
    fn ev(
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

    /// Assert two stores hold the same conversation log (same events, same order).
    fn assert_converged(a: &EventLog, b: &EventLog) {
        let ea: Vec<EventId> = a.events(&conv()).iter().map(|e| e.id).collect();
        let eb: Vec<EventId> = b.events(&conv()).iter().map(|e| e.id).collect();
        assert_eq!(ea, eb, "stores did not converge");
    }

    #[test]
    fn single_large_event_fits_one_frame() {
        // Pin the invariant the const assert documents against a REAL encoded event: a
        // file-chunk-sized payload PLUS a generous (64-head) parent frontier must still fit
        // one transport frame, or cap_events' always-emit-the-first guarantee would wedge sync.
        let id = DeviceIdentity::generate();
        let parents: Vec<EventId> = (0..64u8).map(|n| EventId::new([n; 32])).collect();
        let payload = vec![0u8; crate::file::crypto::CHUNK_SIZE + 16]; // chunk ciphertext + tag
        let e = ev(&id, 1, parents, 1, &payload);
        let sz = encoded_event_size(&e);
        assert!(
            sz < crate::transport::MAX_PLAINTEXT,
            "encoded event {sz} >= frame budget {}",
            crate::transport::MAX_PLAINTEXT
        );
    }

    #[test]
    fn disjoint_branches_converge() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a_ev = ev(&id, 2, vec![root.id], 2, b"a");
        let b_ev = ev(&id, 3, vec![root.id], 2, b"b");

        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        a.append(a_ev.clone()).unwrap();

        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();
        b.append(b_ev.clone()).unwrap();

        let (report_a, report_b) = reconcile(&mut a, &mut b, conv()).unwrap();
        assert_eq!(report_a.applied, 1); // a receives b_ev
        assert_eq!(report_b.applied, 1); // b receives a_ev
        assert!(report_a.rejected.is_empty() && report_b.rejected.is_empty());
        assert_converged(&a, &b);
        // Both now hold all three events.
        assert!(a.has(&a_ev.id) && a.has(&b_ev.id));
        assert!(b.has(&a_ev.id) && b.has(&b_ev.id));
    }

    #[test]
    fn subset_peer_catches_up() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a_ev = ev(&id, 2, vec![root.id], 2, b"a");
        let b_ev = ev(&id, 3, vec![a_ev.id], 3, b"b");

        // `full` has the whole chain; `partial` has only root.
        let mut full = EventLog::default();
        full.append(root.clone()).unwrap();
        full.append(a_ev.clone()).unwrap();
        full.append(b_ev.clone()).unwrap();

        let mut partial = EventLog::default();
        partial.append(root.clone()).unwrap();

        let (report_partial, report_full) = reconcile(&mut partial, &mut full, conv()).unwrap();
        assert_eq!(report_partial.applied, 2); // partial receives a_ev + b_ev
        assert_eq!(report_full.applied, 0); // full already had everything
        assert_converged(&partial, &full);
        assert_eq!(partial.events(&conv()).len(), 3);
    }

    #[test]
    fn identical_logs_exchange_nothing() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");

        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();

        let (report_a, report_b) = reconcile(&mut a, &mut b, conv()).unwrap();
        assert_eq!(report_a.applied, 0);
        assert_eq!(report_b.applied, 0);
        assert_converged(&a, &b);
    }

    #[test]
    fn reconcile_is_idempotent() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a_ev = ev(&id, 2, vec![root.id], 2, b"a");
        let b_ev = ev(&id, 3, vec![root.id], 2, b"b");

        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        a.append(a_ev.clone()).unwrap();
        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();
        b.append(b_ev.clone()).unwrap();

        reconcile(&mut a, &mut b, conv()).unwrap();
        // A second pass applies nothing new.
        let (report_a, report_b) = reconcile(&mut a, &mut b, conv()).unwrap();
        assert_eq!(report_a.applied, 0);
        assert_eq!(report_b.applied, 0);
        assert_converged(&a, &b);
    }

    #[test]
    fn multi_author_concurrent_dag_converges() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let root = ev(&alice, 1, vec![], 1, b"root");
        // Alice's branch and Bob's branch from the shared root, then Alice merges.
        let a1 = ev(&alice, 2, vec![root.id], 2, b"a1");
        let b1 = Event::new(
            &bob,
            conv(),
            1,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"b1".to_vec(),
        );
        let merge = ev(&alice, 3, vec![a1.id, b1.id], 3, b"merge");

        // Start divergent: A has Alice's branch, B has Bob's branch.
        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        a.append(a1.clone()).unwrap();

        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();
        b.append(b1.clone()).unwrap();

        // First reconcile so both have {root, a1, b1}, then Alice can author the merge.
        reconcile(&mut a, &mut b, conv()).unwrap();
        assert_converged(&a, &b);
        a.append(merge.clone()).unwrap();

        // Sync again; B catches the merge.
        reconcile(&mut b, &mut a, conv()).unwrap();
        assert_converged(&a, &b);
        assert!(b.has(&merge.id));
    }

    #[test]
    fn store_reports_ids_and_excludes_known() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a = ev(&id, 1, vec![], 1, b"a");
        let b = ev(&id, 2, vec![a.id], 2, b"b");
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();

        let ids = log.event_ids(&conv()).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));

        // Excluding `a` leaves only `b`.
        let have: HashSet<EventId> = std::iter::once(a.id).collect();
        let missing = log.events_excluding(&conv(), &have).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, b.id);
    }

    #[test]
    fn messages_round_trip_through_bincode() {
        let id = DeviceIdentity::generate();
        let a = ev(&id, 1, vec![], 1, b"a");
        let req = SyncRequest {
            conversation: conv(),
            have: vec![a.id],
        };
        let resp = SyncResponse {
            conversation: conv(),
            events: vec![a.clone()],
            have: vec![a.id],
        };

        let req2: SyncRequest = bincode::deserialize(&bincode::serialize(&req).unwrap()).unwrap();
        let resp2: SyncResponse =
            bincode::deserialize(&bincode::serialize(&resp).unwrap()).unwrap();
        assert_eq!(req2.have, req.have);
        assert_eq!(resp2.events, resp.events);
    }

    #[test]
    fn events_excluding_preserves_topo_order_skipping_middle() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        let mid = ev(&id, 2, vec![root.id], 2, b"mid");
        let tip = ev(&id, 3, vec![mid.id], 3, b"tip");
        log.append(root.clone()).unwrap();
        log.append(mid.clone()).unwrap();
        log.append(tip.clone()).unwrap();

        // Exclude the MIDDLE event; the remainder must stay in topo order.
        let have: HashSet<EventId> = std::iter::once(mid.id).collect();
        let remaining = log.events_excluding(&conv(), &have).unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].id, root.id); // root before tip
        assert_eq!(remaining[1].id, tip.id);
    }

    #[test]
    fn event_ids_is_empty_for_unknown_conversation() {
        let log = EventLog::default();
        assert!(log.event_ids(&conv()).unwrap().is_empty());
        let all: HashSet<EventId> = HashSet::new();
        assert!(log.events_excluding(&conv(), &all).unwrap().is_empty());
    }

    #[test]
    fn followup_round_trips_through_bincode() {
        let id = DeviceIdentity::generate();
        let a = ev(&id, 1, vec![], 1, b"a");
        let followup = SyncFollowup {
            conversation: conv(),
            events: vec![a.clone()],
        };
        let back: SyncFollowup =
            bincode::deserialize(&bincode::serialize(&followup).unwrap()).unwrap();
        assert_eq!(back.conversation, followup.conversation);
        assert_eq!(back.events, followup.events);
    }

    #[test]
    fn request_lists_all_local_ids() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a = ev(&id, 1, vec![], 1, b"a");
        let b = ev(&id, 2, vec![a.id], 2, b"b");
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();

        let req = build_request(&log, conv()).unwrap();
        assert_eq!(req.conversation, conv());
        assert_eq!(req.have.len(), 2);
        assert!(req.have.contains(&a.id) && req.have.contains(&b.id));
    }

    #[test]
    fn response_sends_exactly_the_requesters_missing_events() {
        let id = DeviceIdentity::generate();
        // Responder has root -> a -> b; requester has only root.
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![a.id], 3, b"b");
        responder.append(root.clone()).unwrap();
        responder.append(a.clone()).unwrap();
        responder.append(b.clone()).unwrap();

        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id],
        };
        let resp = handle_request(&responder, &req).unwrap();

        // Missing = {a, b}, in topological order (a before b).
        assert_eq!(resp.events.len(), 2);
        assert_eq!(resp.events[0].id, a.id);
        assert_eq!(resp.events[1].id, b.id);
        // Responder advertises all three of its ids.
        assert_eq!(resp.have.len(), 3);
    }

    #[test]
    fn response_is_empty_when_requester_has_everything() {
        let id = DeviceIdentity::generate();
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        responder.append(root.clone()).unwrap();

        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id],
        };
        let resp = handle_request(&responder, &req).unwrap();
        assert!(resp.events.is_empty());
    }

    #[test]
    fn response_for_disjoint_branches_sends_the_responders_branch() {
        let id = DeviceIdentity::generate();
        // Shared root, then responder has branch `b`, requester has branch `a`.
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![root.id], 2, b"b");

        let mut responder = EventLog::default();
        responder.append(root.clone()).unwrap();
        responder.append(b.clone()).unwrap();

        // Requester has root + a.
        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id, a.id],
        };
        let resp = handle_request(&responder, &req).unwrap();
        // Responder only has root + b; requester is missing b (and has root already).
        assert_eq!(resp.events.len(), 1);
        assert_eq!(resp.events[0].id, b.id);
        // The responder must not advertise an id it does not hold (e.g. `a`).
        assert!(!resp.have.contains(&a.id));
    }

    #[test]
    fn response_is_empty_when_requester_is_ahead() {
        let id = DeviceIdentity::generate();
        // Responder only has root; requester holds two events beyond it.
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        responder.append(root.clone()).unwrap();
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![a.id], 3, b"b");

        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id, a.id, b.id],
        };
        let resp = handle_request(&responder, &req).unwrap();
        assert!(resp.events.is_empty());
        assert_eq!(resp.have, vec![root.id]); // responder advertises only what it holds
    }

    #[test]
    fn response_from_empty_store_is_empty() {
        let id = DeviceIdentity::generate();
        let empty = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id],
        };
        let resp = handle_request(&empty, &req).unwrap();
        assert!(resp.events.is_empty());
        assert!(resp.have.is_empty());
    }

    #[test]
    fn handle_response_applies_events_and_builds_followup() {
        let id = DeviceIdentity::generate();
        // Requester has only root; responder sent it a + b.
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![a.id], 3, b"b");
        // Requester also has a local-only event c that the responder lacks.
        let c = ev(&id, 4, vec![root.id], 2, b"c");

        let mut requester = EventLog::default();
        requester.append(root.clone()).unwrap();
        requester.append(c.clone()).unwrap();

        // Responder advertised {root, a, b}; the followup should offer c.
        let response = SyncResponse {
            conversation: conv(),
            events: vec![a.clone(), b.clone()],
            have: vec![root.id, a.id, b.id],
        };
        let (report, followup) = handle_response(&mut requester, &response).unwrap();

        assert_eq!(report.applied, 2);
        assert!(report.rejected.is_empty());
        assert!(requester.has(&a.id) && requester.has(&b.id));
        // Responder is missing c.
        assert_eq!(followup.events.len(), 1);
        assert_eq!(followup.events[0].id, c.id);
    }

    #[test]
    fn ingest_rejects_a_forged_event_but_keeps_the_rest() {
        let id = DeviceIdentity::generate();
        let mut requester = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        requester.append(root.clone()).unwrap();

        // A valid event a, and a forged event with a broken signature.
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let mut forged = ev(&id, 3, vec![root.id], 2, b"forged");
        forged.sig[0] ^= 0xFF;

        let response = SyncResponse {
            conversation: conv(),
            events: vec![a.clone(), forged.clone()],
            have: vec![],
        };
        let (report, _followup) = handle_response(&mut requester, &response).unwrap();

        assert_eq!(report.applied, 1); // only `a`
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].0, forged.id);
        assert!(requester.has(&a.id));
        assert!(!requester.has(&forged.id));
    }

    #[test]
    fn ingesting_known_events_counts_as_duplicates() {
        let id = DeviceIdentity::generate();
        let mut requester = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        requester.append(root.clone()).unwrap();

        let response = SyncResponse {
            conversation: conv(),
            events: vec![root.clone()], // requester already has root
            have: vec![root.id],
        };
        let (report, _followup) = handle_response(&mut requester, &response).unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.duplicates, 1);
    }

    #[test]
    fn handle_followup_applies_events() {
        let id = DeviceIdentity::generate();
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        responder.append(root.clone()).unwrap();

        let c = ev(&id, 2, vec![root.id], 2, b"c");
        let followup = SyncFollowup {
            conversation: conv(),
            events: vec![c.clone()],
        };
        let report = handle_followup(&mut responder, &followup).unwrap();
        assert_eq!(report.applied, 1);
        assert!(responder.has(&c.id));
    }

    #[test]
    fn child_of_rejected_parent_is_orphaned_safely() {
        // A malicious batch: a valid event, a forged event, and a child that
        // depends on the forged (rejected) one. The child must fail cleanly with
        // a missing parent — never corrupt the store.
        let id = DeviceIdentity::generate();
        let mut store = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        store.append(root.clone()).unwrap();

        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let mut forged = ev(&id, 3, vec![root.id], 2, b"forged");
        forged.sig[0] ^= 0xFF; // breaks the signature
        let child = ev(&id, 4, vec![forged.id], 3, b"child"); // parent = the forged event

        let response = SyncResponse {
            conversation: conv(),
            events: vec![a.clone(), forged.clone(), child.clone()],
            have: vec![],
        };
        let (report, _followup) = handle_response(&mut store, &response).unwrap();

        assert_eq!(report.applied, 1); // only `a`
        assert_eq!(report.rejected.len(), 2); // forged (bad sig) + child (missing parent)
        assert!(store.has(&a.id));
        assert!(!store.has(&forged.id) && !store.has(&child.id));
    }

    #[test]
    fn returning_peer_catches_up_after_offline_activity() {
        // Models Phase-0 offline delivery: A goes offline at the root; B keeps
        // appending; A returns and syncs everything B accumulated.
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");

        let mut online = EventLog::default();
        online.append(root.clone()).unwrap();
        // B (online) appends a chain of 5 while A is away.
        let mut parent = root.id;
        let mut chain = Vec::new();
        for i in 0..5u64 {
            let e = ev(&id, i + 2, vec![parent], i + 2, &[b'm', i as u8]);
            online.append(e.clone()).unwrap();
            parent = e.id;
            chain.push(e);
        }

        let mut returning = EventLog::default();
        returning.append(root.clone()).unwrap();

        let (report, _) = reconcile(&mut returning, &mut online, conv()).unwrap();
        assert_eq!(report.applied, 5);
        assert_converged(&returning, &online);
        for e in &chain {
            assert!(returning.has(&e.id));
        }
    }

    #[test]
    fn random_partitions_converge() {
        use rand_chacha::ChaCha20Rng;
        use rand_core::{RngCore, SeedableRng};
        use std::collections::HashMap;

        // 70% inclusion, expressed against next_u32() so the test needs no `rand` crate.
        let pct = |rng: &mut ChaCha20Rng, p: u32| rng.next_u32() % 100 < p;

        let authors: Vec<DeviceIdentity> = (0..3).map(|_| DeviceIdentity::generate()).collect();

        for seed in 0..8u64 {
            let mut rng = ChaCha20Rng::seed_from_u64(seed);

            // Build a valid reference DAG of 12 events using a reference log's
            // `prepare` for correct parents/lamport, with dense per-author seqs.
            let mut reference = EventLog::default();
            let mut all: Vec<Event> = Vec::new();
            let mut seqs: HashMap<[u8; 32], u64> = HashMap::new();
            for i in 0..12u64 {
                let author = &authors[(rng.next_u32() as usize) % authors.len()];
                let (parents, lamport) = reference.prepare(&conv()).unwrap();
                let pk = author.public().ed25519_pub;
                let seq = {
                    let s = seqs.entry(pk).or_insert(0);
                    *s += 1;
                    *s
                };
                let e = ev(author, seq, parents, lamport, &[i as u8]);
                reference.append(e.clone()).unwrap();
                all.push(e);
            }

            // Causally-closed random partition: only add an event to a side if all
            // its parents are already there, so each side stays append-valid.
            let mut a = EventLog::default();
            let mut b = EventLog::default();
            let (mut a_ids, mut b_ids): (HashSet<EventId>, HashSet<EventId>) =
                (HashSet::new(), HashSet::new());
            for e in &all {
                if pct(&mut rng, 70) && e.parents.iter().all(|p| a_ids.contains(p)) {
                    a.append(e.clone()).unwrap();
                    a_ids.insert(e.id);
                }
                if pct(&mut rng, 70) && e.parents.iter().all(|p| b_ids.contains(p)) {
                    b.append(e.clone()).unwrap();
                    b_ids.insert(e.id);
                }
            }

            reconcile(&mut a, &mut b, conv()).unwrap();

            // Both converge to exactly the union of the two partitions.
            let mut got: Vec<EventId> = a.events(&conv()).iter().map(|e| e.id).collect();
            let mut got_b: Vec<EventId> = b.events(&conv()).iter().map(|e| e.id).collect();
            got.sort();
            got_b.sort();
            assert_eq!(got, got_b, "seed {seed}: peers did not converge");

            let mut union: Vec<EventId> = a_ids.union(&b_ids).copied().collect();
            union.sort();
            assert_eq!(
                got, union,
                "seed {seed}: converged set != union of partitions"
            );
        }
    }

    #[test]
    fn all_rejected_batch_changes_nothing() {
        let id = DeviceIdentity::generate();
        let mut store = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        store.append(root.clone()).unwrap();

        // Two events that reference a parent the store does not hold.
        let phantom = EventId::new([9u8; 32]);
        let x = ev(&id, 2, vec![phantom], 2, b"x");
        let y = ev(&id, 3, vec![phantom], 2, b"y");

        let response = SyncResponse {
            conversation: conv(),
            events: vec![x.clone(), y.clone()],
            have: vec![],
        };
        let (report, _followup) = handle_response(&mut store, &response).unwrap();

        assert_eq!(report.applied, 0);
        assert_eq!(report.rejected.len(), 2);
        assert_eq!(store.event_ids(&conv()).unwrap().len(), 1); // still just root
    }

    #[test]
    fn peer_holding_merge_event_converges_in_one_round() {
        // A already holds a full DAG including a merge of two branches; B holds
        // only the root. A SINGLE reconcile must bring B fully up to date — the
        // merge and both its parents arrive topologically ordered in one round.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let root = ev(&alice, 1, vec![], 1, b"root");
        let a1 = ev(&alice, 2, vec![root.id], 2, b"a1");
        let b1 = Event::new(
            &bob,
            conv(),
            1,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"b1".to_vec(),
        );
        let merge = ev(&alice, 3, vec![a1.id, b1.id], 3, b"merge");

        let mut a = EventLog::default();
        for e in [&root, &a1, &b1, &merge] {
            a.append(e.clone()).unwrap();
        }
        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();

        // B is the requester (behind); A is the responder (holds the merge).
        let (report_b, report_a) = reconcile(&mut b, &mut a, conv()).unwrap();
        assert_eq!(report_b.applied, 3); // a1, b1, merge
        assert_eq!(report_a.applied, 0);
        assert_converged(&a, &b);
        assert!(b.has(&merge.id));
    }

    #[test]
    fn followup_excludes_events_just_received_from_responder() {
        // No echo-back: an event the requester ingests from the responder must
        // not be offered back to the responder in the followup. (Convergence
        // correctness depends on this — otherwise the round would re-send it.)
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a"); // responder-only event

        let mut requester = EventLog::default();
        requester.append(root.clone()).unwrap();

        // Responder advertises {root, a} and sends `a`.
        let response = SyncResponse {
            conversation: conv(),
            events: vec![a.clone()],
            have: vec![root.id, a.id],
        };
        let (report, followup) = handle_response(&mut requester, &response).unwrap();
        assert_eq!(report.applied, 1); // `a` ingested
        // Even though the requester now holds `a`, the followup must not echo it
        // back (it is in the responder's `have`).
        assert!(followup.events.is_empty());
    }

    /// Build a store with `n` chained events in `conv()` and return it.
    fn build_store_with_events(n: usize) -> (EventLog, ConversationId) {
        let id = DeviceIdentity::generate();
        let mut store = EventLog::default();
        let c = conv();
        let mut parent_ids: Vec<EventId> = vec![];
        for i in 0..n as u64 {
            let e = Event::new(
                &id,
                c,
                i + 1,
                parent_ids.clone(),
                i + 1,
                0,
                EventKind::Message,
                vec![b'x'; 32], // small fixed payload so sizes are predictable
            );
            parent_ids = vec![e.id];
            store.append(e).unwrap();
        }
        (store, c)
    }

    #[test]
    fn bounded_sync_advances_across_inverted_signed_clocks() {
        let author = DeviceIdentity::generate();
        let parent = ev(&author, 1, vec![], 100, b"parent");
        let child = ev(&author, 2, vec![parent.id], 1, b"child");
        let mut source = EventLog::default();
        source.append(parent.clone()).unwrap();
        source.append(child.clone()).unwrap();
        let mut target = EventLog::default();
        for expected in [parent.id, child.id] {
            let response = handle_request_bounded(
                &source,
                &build_request(&target, conv()).unwrap(),
                SYNC_MARGIN + 1,
            )
            .unwrap();
            assert_eq!(response.events.len(), 1);
            assert_eq!(response.events[0].id, expected);
            let (report, _) = handle_response(&mut target, &response).unwrap();
            assert_eq!(report.applied, 1);
            assert!(report.rejected.is_empty());
        }
        assert_converged(&target, &source);
    }

    #[test]
    fn cap_events_returns_prefix_within_budget_but_always_one() {
        let (store, c) = build_store_with_events(5);
        let all = store.events_excluding(&c, &HashSet::new()).unwrap();
        assert_eq!(all.len(), 5);
        let one = encoded_event_size(&all[0]);
        // A budget below one event still yields exactly one (progress guarantee).
        assert_eq!(cap_events(all.clone(), 1).len(), 1);
        // A budget for ~2 events yields 2 or 3 (size-dependent), never all 5.
        let capped = cap_events(all.clone(), one * 2 + 8);
        assert!(!capped.is_empty() && capped.len() < 5);
        // A huge budget yields all.
        assert_eq!(cap_events(all, usize::MAX).len(), 5);
    }

    #[test]
    fn handle_request_bounded_caps_events_but_reports_full_have() {
        let (store, c) = build_store_with_events(5);
        let request = SyncRequest {
            conversation: c,
            have: vec![],
        };
        let one = encoded_event_size(&store.events_excluding(&c, &HashSet::new()).unwrap()[0]);
        // A small frame budget returns fewer events than the full set...
        let resp = handle_request_bounded(&store, &request, one + 8 + 300).unwrap();
        assert!(resp.events.len() < 5);
        // ...but always the FULL have id-set, so the requester knows to come back.
        assert_eq!(resp.have.len(), 5);
        // Unbounded returns everything.
        assert_eq!(handle_request(&store, &request).unwrap().events.len(), 5);
    }

    #[test]
    fn bounded_memory_export_does_not_materialize_the_missing_ciphertext_tail() {
        let (mut store, conversation) = build_store_with_events(40);
        let ordered = causal_events(store.events(&conversation));
        let budget = encoded_event_size(ordered[0]);
        UNBOUNDED_EXPORTS.with(|count| count.set(0));
        let page = store
            .events_excluding_bounded(&conversation, &HashSet::new(), budget, 1)
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, ordered[0].id);
        assert_eq!(
            UNBOUNDED_EXPORTS.with(|count| count.get()),
            0,
            "bounded memory export must select before cloning the missing history"
        );
        let request = SyncRequest {
            conversation,
            have: vec![],
        };
        let response = handle_request_bounded(&store, &request, budget + SYNC_MARGIN).unwrap();
        assert_eq!(response.events.len(), 1);
        let empty = SyncResponse {
            conversation,
            events: vec![],
            have: vec![],
        };
        let (_, frames) =
            handle_response_bounded_burst(&mut store, &empty, budget + SYNC_MARGIN, 3).unwrap();
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|frame| frame.events.len() == 1));
        assert_eq!(
            UNBOUNDED_EXPORTS.with(|count| count.get()),
            0,
            "request and followup paths must use bounded ciphertext export"
        );
    }

    #[test]
    fn bounded_frame_prefix_matches_greedy_burst_packing() {
        let (store, conversation) = build_store_with_events(7);
        let ordered = causal_events(store.events(&conversation));
        let budget = encoded_event_size(ordered[0]) + 1;
        let prefix: Vec<_> = frame_prefix(ordered.iter().copied(), budget, 3, |event| {
            encoded_event_size(event)
        })
        .collect();
        assert_eq!(prefix.len(), 3);
        for (selected, expected) in prefix.iter().zip(&ordered) {
            assert!(std::ptr::eq(*selected, *expected));
        }
        assert!(bounded_events(ordered.iter().copied(), budget, 0).is_empty());
        assert_eq!(
            bounded_events(ordered.iter().copied(), 1, 2).len(),
            2,
            "an oversized first event still occupies one bounded frame"
        );
    }

    #[test]
    fn bounded_followup_backlog_converges_without_unbounded_exports() {
        let (mut source, conversation) = build_store_with_events(40);
        let mut target = EventLog::default();
        let frame_budget = encoded_event_size(source.events(&conversation)[0]) + SYNC_MARGIN;
        UNBOUNDED_EXPORTS.with(|count| count.set(0));
        for _ in 0..40 {
            let request = build_request(&source, conversation).unwrap();
            let response = handle_request_bounded(&target, &request, frame_budget).unwrap();
            let (report, frames) =
                handle_response_bounded_burst(&mut source, &response, frame_budget, 3).unwrap();
            assert!(report.rejected.is_empty());
            assert!(frames.len() <= 3);
            for frame in frames {
                assert_eq!(frame.events.len(), 1);
                let report = handle_followup(&mut target, &frame).unwrap();
                assert_eq!(report.applied, 1);
                assert!(report.rejected.is_empty());
            }
            if target.event_ids(&conversation).unwrap().len() == 40 {
                break;
            }
        }
        assert_converged(&source, &target);
        assert_eq!(
            UNBOUNDED_EXPORTS.with(|count| count.get()),
            0,
            "multi-frame catch-up must never copy the unused ciphertext backlog"
        );
    }
}
