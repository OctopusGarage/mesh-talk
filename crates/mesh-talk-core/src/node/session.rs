//! Driving event-log reconciliation over a [`SecureChannel`] — the networked
//! counterpart of Plan-4's in-process `reconcile`. The same handlers
//! (`build_request`/`handle_request`/`handle_response`/`handle_followup`) run on
//! each side; the three messages are framed (bincode) and sent over the channel.
//! The requester runs one round with [`request_round`]; the responder serves
//! rounds one message at a time with [`serve_one`].
//!
//! The store is shared as a `&Mutex<S>` and locked only for the synchronous
//! handler calls — never across an `.await` — so a `std::sync::Mutex` is correct.

use crate::eventlog::event::{ConversationId, EventId};
use crate::eventlog::sync::{
    build_request, fingerprint, handle_followup, handle_request_bounded, handle_response_bounded,
    ApplyReport, FpRequest, FpResponse, SyncFollowup, SyncRequest, SyncResponse, SyncStore,
    MAX_REJECTED_DETAIL,
};
use crate::transport::{SecureChannel, TransportError, MAX_PLAINTEXT};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

/// Backstop on sync rounds for one conversation (a peer that resends
/// non-applying events forever can't spin us indefinitely). Generous: a round
/// transfers up to ~one frame, so this bounds a single conversation's transfer.
const MAX_SYNC_ROUNDS: usize = 10_000;

/// Bytes per event id (a 32-byte hash) — used to estimate the `have`-set bandwidth in the
/// sync-cost telemetry below.
const EVENT_ID_BYTES: usize = 32;

/// Event-ids per `have` chunk frame. 32 B/id; this keeps a chunk well under
/// `MAX_PLAINTEXT` (65519) with room for the wrappers.
const HAVE_IDS_PER_CHUNK: usize = 1500;
/// Backstop on the number of `have` chunk frames accepted for one conversation
/// (a peer can't stream unbounded chunks). 1500 × this ≫ any real conversation.
const MAX_HAVE_CHUNKS: usize = 1000;
/// Hard cap on the TOTAL ids accumulated from a streamed `have`, independent of the
/// per-chunk + chunk-count bounds. Without it a peer could pin `HAVE_IDS_PER_CHUNK *
/// MAX_HAVE_CHUNKS` ids (~48 MB) per concurrent sync against the always-on relay; this
/// caps it at ~6 MB while staying far above any realistic conversation's id-set.
const MAX_HAVE_IDS_TOTAL: usize = 200_000;

/// One framed sync message on the wire.
///
/// The requester's and responder's `have` id-sets are streamed as a sequence of
/// `ReqHave`/`RespHave` chunk frames (terminated by `more == false`) rather than
/// inlined into `Request`/`Response`, so a conversation with more event-ids than
/// fit one frame (~2040) still reconciles. `Request`/`Response` therefore carry an
/// empty `have` on this networked path (the in-process `reconcile` keeps inlining it).
#[derive(Debug, Serialize, Deserialize)]
enum SyncWire {
    Request(SyncRequest),
    Response(SyncResponse),
    Followup(SyncFollowup),
    ReqHave(HaveChunk),
    RespHave(HaveChunk),
    // Appended (stable discriminants) for the whole-conversation fingerprint short-circuit.
    FpRequest(FpRequest),
    FpResponse(FpResponse),
    // Optional final custody probe. Legacy endpoints close this disposable
    // connection; ordinary reconciliation remains byte-for-byte compatible.
    DurableHaveRequest {
        conversation: ConversationId,
        event_id: EventId,
    },
    DurableHaveResponse {
        conversation: ConversationId,
        event_id: EventId,
        stored: bool,
    },
}

/// Acceptance preflight for an immutable event which must fit both directions
/// of ordinary reconciliation. Have sets are streamed separately.
pub(in crate::node) fn event_fits_frame(event: &crate::eventlog::Event) -> bool {
    let response = SyncWire::Response(SyncResponse {
        conversation: event.conversation_id,
        events: vec![event.clone()],
        have: vec![],
    });
    let followup = SyncWire::Followup(SyncFollowup {
        conversation: event.conversation_id,
        events: vec![event.clone()],
    });
    [&response, &followup].iter().all(|wire| {
        bincode::serialized_size(wire)
            .is_ok_and(|size| size <= crate::transport::MAX_PLAINTEXT as u64)
    })
}

/// One chunk of a streamed `have` id-set.
#[derive(Debug, Serialize, Deserialize)]
struct HaveChunk {
    conversation: ConversationId,
    ids: Vec<EventId>,
    /// `true` if more chunks of this id-set follow.
    more: bool,
}

/// Errors from a sync session.
#[derive(Debug)]
pub enum SessionError {
    /// Channel send/recv failure.
    Transport(TransportError),
    /// (De)serialization of a wire message failed.
    Serialization(String),
    /// Received a wire message that doesn't fit the protocol state.
    UnexpectedMessage,
    /// Neither local application nor acceptance of the previous push advanced.
    NoProgress,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Transport(e) => write!(f, "sync transport error: {e}"),
            SessionError::Serialization(m) => write!(f, "sync serialization error: {m}"),
            SessionError::UnexpectedMessage => write!(f, "unexpected sync message"),
            SessionError::NoProgress => write!(f, "sync made no progress"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<TransportError> for SessionError {
    fn from(e: TransportError) -> Self {
        SessionError::Transport(e)
    }
}

fn encode(wire: &SyncWire) -> Result<Vec<u8>, SessionError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .serialize(wire)
        .map_err(|e| SessionError::Serialization(e.to_string()))
}

fn decode(bytes: &[u8]) -> Result<SyncWire, SessionError> {
    if bytes.len() > MAX_PLAINTEXT {
        return Err(SessionError::Serialization("oversized sync frame".into()));
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| SessionError::Serialization(e.to_string()))
}

/// A qualified exact storage claim over the authenticated channel, issued only
/// after the remote synchronizes its currently authorized durable store.
/// This is custody, never a recipient delivery receipt.
pub(crate) async fn request_durable_have<IO>(
    channel: &mut SecureChannel<IO>,
    conversation: ConversationId,
    event_id: EventId,
) -> Result<bool, SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    channel
        .send(&encode(&SyncWire::DurableHaveRequest {
            conversation,
            event_id,
        })?)
        .await?;
    match decode(&channel.recv().await?)? {
        SyncWire::DurableHaveResponse {
            conversation: c,
            event_id: id,
            stored,
        } if c == conversation && id == event_id => Ok(stored),
        _ => Err(SessionError::UnexpectedMessage),
    }
}

/// Stream a `have` id-set as chunk frames (always ≥1 frame, so the receiver gets an
/// explicit terminator even for an empty set). `wrap` selects `ReqHave` vs `RespHave`.
async fn send_have<IO>(
    channel: &mut SecureChannel<IO>,
    conversation: ConversationId,
    have: &[EventId],
    wrap: fn(HaveChunk) -> SyncWire,
) -> Result<(), SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let chunks: Vec<&[EventId]> = if have.is_empty() {
        vec![&[][..]]
    } else {
        have.chunks(HAVE_IDS_PER_CHUNK).collect()
    };
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        let wire = wrap(HaveChunk {
            conversation,
            ids: chunk.to_vec(),
            more: i < last,
        });
        channel.send(&encode(&wire)?).await?;
    }
    Ok(())
}

/// Receive a streamed `have` id-set (chunk frames until `more == false`), accepting
/// only the expected variant (`want_req` → `ReqHave`, else `RespHave`). Bounded by
/// `MAX_HAVE_CHUNKS` so a peer can't stream forever.
async fn recv_have<IO>(
    channel: &mut SecureChannel<IO>,
    want_req: bool,
    conversation: ConversationId,
) -> Result<Vec<EventId>, SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let mut have = Vec::new();
    for _ in 0..MAX_HAVE_CHUNKS {
        let chunk = match decode(&channel.recv().await?)? {
            SyncWire::ReqHave(c) if want_req => c,
            SyncWire::RespHave(c) if !want_req => c,
            _ => return Err(SessionError::UnexpectedMessage),
        };
        if chunk.conversation != conversation {
            return Err(SessionError::UnexpectedMessage);
        }
        // A well-behaved sender chunks at HAVE_IDS_PER_CHUNK; reject an over-stuffed chunk so
        // a peer can't pack a frame full of ids to amplify the accumulated `have` (bounds the
        // total at HAVE_IDS_PER_CHUNK * MAX_HAVE_CHUNKS).
        if chunk.ids.len() > HAVE_IDS_PER_CHUNK {
            return Err(SessionError::UnexpectedMessage);
        }
        // Direct cap on accumulated ids (not just per-chunk × chunk-count), bounding relay memory.
        if have.len() + chunk.ids.len() > MAX_HAVE_IDS_TOTAL {
            return Err(SessionError::UnexpectedMessage);
        }
        have.extend(chunk.ids);
        if !chunk.more {
            return Ok(have);
        }
    }
    Err(SessionError::UnexpectedMessage)
}

/// Run one reconciliation round as the REQUESTER over `channel`, for
/// `conversation`: send a Request (+ our streamed have), receive the Response
/// (ingesting its events) + the responder's streamed have, then send the Followup.
/// Converges the conversation in both directions. Returns what this side applied.
pub async fn request_round<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<ApplyReport, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    // Phase 0 — fingerprint short-circuit. Exchange a single 32-byte digest of our whole
    // id-set; if the responder's matches, the logs are identical and we skip the O(N)
    // have-set streaming entirely (the common idle-drain case). A mismatch falls through to
    // the full reconciliation below. (Correctness: a digest match means equal sets; a
    // false match is cryptographically negligible and, since ingest re-validates, could
    // only skip a diff — never corrupt — so this is safe.)
    {
        let fp = {
            let store = store.lock().expect("store mutex not poisoned");
            fingerprint(store.event_ids(&conversation))
        };
        channel
            .send(&encode(&SyncWire::FpRequest(FpRequest {
                conversation,
                fingerprint: fp,
            }))?)
            .await?;
        match decode(&channel.recv().await?)? {
            SyncWire::FpResponse(FpResponse { matched: true }) => {
                log::debug!(
                    target: "mesh_talk::sync",
                    "sync conv={} fp_match skipped",
                    hex::encode(&conversation.as_bytes()[..4]),
                );
                return Ok(ApplyReport::default());
            }
            SyncWire::FpResponse(FpResponse { matched: false }) => {
                // Diverged — run the full id-set reconciliation below.
            }
            _ => return Err(SessionError::UnexpectedMessage),
        }
    }

    let mut total = ApplyReport::default();
    // Sync-cost telemetry (see node/session.rs sync docs): we re-stream the FULL id-set
    // (`have`) every round in both directions regardless of how small the diff is, so the
    // metadata cost is O(N) per round. Capture N, the diff, and the round count so we can
    // evaluate reconciliation scaling (e.g. range-based / Negentropy) on real workloads.
    let mut rounds = 0u32;
    let mut first_have = 0usize;
    let mut pushed_total = 0usize;
    let mut previous_push = std::collections::HashSet::new();
    for round in 0..MAX_SYNC_ROUNDS {
        let have = {
            let store = store.lock().expect("store mutex not poisoned");
            build_request(&*store, conversation).have
        };
        if round == 0 {
            first_have = have.len();
        }
        rounds += 1;
        // Opening Request carries an empty inline have; the real set is streamed.
        channel
            .send(&encode(&SyncWire::Request(SyncRequest {
                conversation,
                have: Vec::new(),
            }))?)
            .await?;
        send_have(channel, conversation, &have, SyncWire::ReqHave).await?;

        // Response carries the (bounded) events; the responder's have is streamed after.
        let mut response = match decode(&channel.recv().await?)? {
            SyncWire::Response(r) => r,
            _ => return Err(SessionError::UnexpectedMessage),
        };
        if response.conversation != conversation
            || response
                .events
                .iter()
                .any(|event| event.conversation_id != conversation)
        {
            return Err(SessionError::UnexpectedMessage);
        }
        response.have = recv_have(channel, false, conversation).await?;

        let (report, followup) = {
            let mut store = store.lock().expect("store mutex not poisoned");
            let result = handle_response_bounded(&mut *store, &response, MAX_PLAINTEXT);
            if store.admission_denied() {
                return Err(SessionError::UnexpectedMessage);
            }
            result
        };
        let made_progress = report.applied > 0;
        // Sending is not progress: projected stores can ingest a duplicate but
        // omit it from their Have set. Require evidence of acceptance before
        // repeating a push, unless this round advanced our own store instead.
        if !previous_push.is_empty()
            && !made_progress
            && !response.have.iter().any(|id| previous_push.contains(id))
        {
            return Err(SessionError::NoProgress);
        }
        let more_to_push = !followup.events.is_empty();
        pushed_total += followup.events.len();
        previous_push = followup.events.iter().map(|event| event.id).collect();

        channel
            .send(&encode(&SyncWire::Followup(followup))?)
            .await?;

        total.applied += report.applied;
        total.duplicates += report.duplicates;
        // Bound accumulated reject detail across rounds (diagnostics only) so a peer that
        // keeps a multi-round sync alive while interleaving bad events can't grow it.
        if total.rejected.len() < MAX_REJECTED_DETAIL {
            total.rejected.extend(report.rejected);
        }

        if !made_progress && !more_to_push {
            break;
        }
        if round + 1 == MAX_SYNC_ROUNDS {
            return Err(SessionError::NoProgress);
        }
    }
    // One line per conversation sync. `have` is the id-set size N we streamed (≈32·N bytes
    // each way, per round); applied/pushed are the actual diff. When `have` ≫ applied+pushed
    // across many syncs, the O(N) id-set is the dominant waste — the signal that range-based
    // reconciliation is worth it. Greppable target so the diagnostics log can filter it.
    let diff = total.applied + pushed_total;
    log::debug!(
        target: "mesh_talk::sync",
        "sync conv={} have={} have_kib={} applied={} dup={} pushed={} diff={} rounds={}",
        hex::encode(&conversation.as_bytes()[..4]),
        first_have,
        first_have * EVENT_ID_BYTES / 1024,
        total.applied,
        total.duplicates,
        pushed_total,
        diff,
        rounds,
    );
    Ok(total)
}

/// The outcome of serving one inbound wire message.
#[derive(Debug)]
pub enum Served {
    /// Handled a message for this conversation (the caller may surface new events).
    Handled(ConversationId),
    /// The channel closed / errored — stop serving it.
    Closed,
}

/// Serve ONE inbound wire message as the RESPONDER over `channel`: a Request is
/// answered with a Response; a Followup is ingested. Returns the conversation
/// handled, or `Closed` when the peer hangs up (a recv error is treated as a
/// clean close so the serve loop can stop).
pub async fn serve_one<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
) -> Result<Served, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let bytes = match channel.recv().await {
        Ok(b) => b,
        Err(_) => return Ok(Served::Closed),
    };
    serve_wire_bytes(channel, store, &bytes).await
}

/// Serve one ALREADY-READ sync wire frame (the body of [`serve_one`] after the recv).
/// Exposed so a caller that peeked the first frame of a connection — e.g. to detect a
/// non-sync protocol like device pairing — can still serve a sync frame it consumed.
pub async fn serve_wire_bytes<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    bytes: &[u8],
) -> Result<Served, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    match decode(bytes)? {
        SyncWire::DurableHaveRequest {
            conversation,
            event_id,
        } => {
            let stored = store
                .lock()
                .expect("store mutex not poisoned")
                .durable_have(&conversation, &event_id);
            channel
                .send(&encode(&SyncWire::DurableHaveResponse {
                    conversation,
                    event_id,
                    stored,
                })?)
                .await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::Request(request) => {
            let conversation = request.conversation;
            // The Request's inline have is empty on the networked path; read the
            // requester's streamed have, then answer.
            let have = recv_have(channel, true, conversation).await?;
            let full = SyncRequest { conversation, have };
            let response = {
                let store = store.lock().expect("store mutex not poisoned");
                handle_request_bounded(&*store, &full, MAX_PLAINTEXT)
            };
            let responder_have = response.have;
            channel
                .send(&encode(&SyncWire::Response(SyncResponse {
                    conversation,
                    events: response.events,
                    have: Vec::new(),
                }))?)
                .await?;
            send_have(channel, conversation, &responder_have, SyncWire::RespHave).await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::Followup(followup) => {
            let conversation = followup.conversation;
            if followup
                .events
                .iter()
                .any(|event| event.conversation_id != conversation)
            {
                return Err(SessionError::UnexpectedMessage);
            }
            {
                let mut store = store.lock().expect("store mutex not poisoned");
                handle_followup(&mut *store, &followup);
                if store.admission_denied() {
                    return Err(SessionError::UnexpectedMessage);
                }
            }
            Ok(Served::Handled(conversation))
        }
        SyncWire::FpRequest(req) => {
            // Compare the requester's whole-set digest to ours. On match the requester
            // skips the full exchange; on mismatch it follows up with a normal Request.
            let conversation = req.conversation;
            let matched = {
                let store = store.lock().expect("store mutex not poisoned");
                fingerprint(store.event_ids(&conversation)) == req.fingerprint
            };
            channel
                .send(&encode(&SyncWire::FpResponse(FpResponse { matched }))?)
                .await?;
            Ok(Served::Handled(conversation))
        }
        // A Response/FpResponse, or a stray have-chunk outside its stream, is out of order.
        SyncWire::Response(_)
        | SyncWire::ReqHave(_)
        | SyncWire::RespHave(_)
        | SyncWire::FpResponse(_) => Err(SessionError::UnexpectedMessage),
        SyncWire::DurableHaveResponse { .. } => Err(SessionError::UnexpectedMessage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn exact_single_event_transport_boundary_includes_long_parent_frontier() {
        let signer = DeviceIdentity::generate();
        for kind in [EventKind::Message, EventKind::FileManifest] {
            for count in [0, 64] {
                let parents = (0..count)
                    .map(|i| EventId::new([i as u8; 32]))
                    .collect::<Vec<_>>();
                let empty = Event::new(&signer, conv(), 1, parents.clone(), 1, 1, kind, vec![]);
                let response = SyncWire::Response(SyncResponse {
                    conversation: conv(),
                    events: vec![empty.clone()],
                    have: vec![],
                });
                let followup = SyncWire::Followup(SyncFollowup {
                    conversation: conv(),
                    events: vec![empty],
                });
                let overhead = bincode::serialized_size(&response)
                    .unwrap()
                    .max(bincode::serialized_size(&followup).unwrap())
                    as usize;
                let fits = Event::new(
                    &signer,
                    conv(),
                    1,
                    parents.clone(),
                    1,
                    1,
                    kind,
                    vec![0; MAX_PLAINTEXT - overhead],
                );
                assert!(event_fits_frame(&fits));
                let exceeds = Event::new(
                    &signer,
                    conv(),
                    1,
                    parents,
                    1,
                    1,
                    kind,
                    vec![0; MAX_PLAINTEXT - overhead + 1],
                );
                assert!(!event_fits_frame(&exceeds));
            }
        }
    }

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    #[derive(Serialize, Deserialize)]
    enum LegacyWire {
        Request(SyncRequest),
        Response(SyncResponse),
        Followup(SyncFollowup),
        ReqHave(HaveChunk),
        RespHave(HaveChunk),
        FpRequest(FpRequest),
        FpResponse(FpResponse),
    }

    #[test]
    fn optional_storage_probe_preserves_legacy_tags_and_has_fixed_strict_size() {
        let request = SyncRequest {
            conversation: conv(),
            have: vec![],
        };
        assert_eq!(
            encode(&SyncWire::Request(request.clone())).unwrap(),
            bincode::serialize(&LegacyWire::Request(request)).unwrap()
        );
        let fp = FpResponse { matched: true };
        assert_eq!(
            encode(&SyncWire::FpResponse(fp)).unwrap(),
            bincode::serialize(&LegacyWire::FpResponse(FpResponse { matched: true })).unwrap()
        );
        let request = encode(&SyncWire::DurableHaveRequest {
            conversation: conv(),
            event_id: EventId::new([3; 32]),
        })
        .unwrap();
        assert_eq!(request.len(), 68);
        assert_eq!(&request[..4], &7u32.to_le_bytes());
        assert!(bincode::deserialize::<LegacyWire>(&request).is_err());
        let mut trailing = request.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        for length in [0, 4, 67] {
            assert!(decode(&request[..length]).is_err());
        }
        let mut response = encode(&SyncWire::DurableHaveResponse {
            conversation: conv(),
            event_id: EventId::new([3; 32]),
            stored: true,
        })
        .unwrap();
        assert_eq!(response.len(), 69);
        response[68] = 2;
        assert!(decode(&response).is_err());
        let oversized = encode(&SyncWire::Response(SyncResponse {
            conversation: conv(),
            events: vec![],
            have: vec![EventId::new([0; 32]); 3000],
        }))
        .unwrap();
        assert!(decode(&oversized).is_err());
    }

    #[tokio::test]
    async fn memory_have_never_qualifies_as_durable_custody() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let event = Event::new(&alice, conv(), 1, vec![], 1, 0, EventKind::Message, vec![1]);
        let id = event.id;
        let (a, b) = tokio::io::duplex(65536);
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
            let mut log = EventLog::default();
            log.append(event).unwrap();
            serve_one(&mut channel, &Mutex::new(log)).await.unwrap();
        });
        let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
        assert!(!request_durable_have(&mut channel, conv(), id)
            .await
            .unwrap());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn exact_storage_probe_rejects_wrong_conversation_or_event() {
        for wrong_conversation in [true, false] {
            let alice = DeviceIdentity::generate();
            let bob = DeviceIdentity::generate();
            let id = EventId::new([3; 32]);
            let (a, b) = tokio::io::duplex(65536);
            let server = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                let _ = channel.recv().await.unwrap();
                channel
                    .send(
                        &encode(&SyncWire::DurableHaveResponse {
                            conversation: if wrong_conversation {
                                ConversationId::new([2; 32])
                            } else {
                                conv()
                            },
                            event_id: if wrong_conversation {
                                id
                            } else {
                                EventId::new([4; 32])
                            },
                            stored: true,
                        })
                        .unwrap(),
                    )
                    .await
                    .unwrap();
            });
            let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
            assert!(matches!(
                request_durable_have(&mut channel, conv(), id).await,
                Err(SessionError::UnexpectedMessage)
            ));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn legacy_endpoint_syncs_normally_then_rejects_only_optional_probe() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let event = Event::new(&alice, conv(), 1, vec![], 1, 0, EventKind::Message, vec![1]);
        let id = event.id;
        let (a, b) = tokio::io::duplex(65536);
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
            let store = Mutex::new(EventLog::default());
            loop {
                let bytes = channel.recv().await.unwrap();
                if bincode::deserialize::<LegacyWire>(&bytes).is_err() {
                    break;
                }
                serve_wire_bytes(&mut channel, &store, &bytes)
                    .await
                    .unwrap();
            }
            assert!(store.lock().unwrap().has(&id));
        });
        let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
        let mut log = EventLog::default();
        log.append(event).unwrap();
        request_round(&mut channel, &Mutex::new(log), conv())
            .await
            .unwrap();
        assert!(request_durable_have(&mut channel, conv(), id)
            .await
            .is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn responder_rejects_events_outside_the_followup_conversation() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let alice = DeviceIdentity::generate();
            let bob = DeviceIdentity::generate();
            let injected = Event::new(
                &alice,
                ConversationId::new([2; 32]),
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                b"unrelated".to_vec(),
            );
            let id = injected.id;
            let (a, b) = tokio::io::duplex(65536);
            let server = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                let store = Mutex::new(EventLog::default());
                let result = serve_one(&mut channel, &store).await;
                assert!(matches!(result, Err(SessionError::UnexpectedMessage)));
                assert!(!store.lock().unwrap().has(&id));
            });
            let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
            channel
                .send(
                    &encode(&SyncWire::Followup(SyncFollowup {
                        conversation: conv(),
                        events: vec![injected],
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
            server.await.unwrap();
        })
        .await
        .expect("malformed followup exchange timed out");
    }

    #[tokio::test]
    async fn requester_rejects_response_conversation_switch() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let alice = DeviceIdentity::generate();
            let bob = DeviceIdentity::generate();
            let (a, b) = tokio::io::duplex(65536);
            let server = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                assert!(matches!(
                    decode(&channel.recv().await.unwrap()).unwrap(),
                    SyncWire::FpRequest(_)
                ));
                channel
                    .send(&encode(&SyncWire::FpResponse(FpResponse { matched: false })).unwrap())
                    .await
                    .unwrap();
                assert!(matches!(
                    decode(&channel.recv().await.unwrap()).unwrap(),
                    SyncWire::Request(_)
                ));
                assert!(matches!(
                    decode(&channel.recv().await.unwrap()).unwrap(),
                    SyncWire::ReqHave(_)
                ));
                let other = ConversationId::new([2; 32]);
                channel
                    .send(
                        &encode(&SyncWire::Response(SyncResponse {
                            conversation: other,
                            events: vec![],
                            have: vec![],
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                let _ = send_have(&mut channel, other, &[], SyncWire::RespHave).await;
            });
            let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
            assert!(matches!(
                request_round(&mut channel, &Mutex::new(EventLog::default()), conv()).await,
                Err(SessionError::UnexpectedMessage)
            ));
            server.await.unwrap();
        })
        .await
        .expect("malformed response exchange timed out");
    }

    #[tokio::test]
    async fn streamed_have_rejects_a_different_conversation() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            for want_req in [true, false] {
                let alice = DeviceIdentity::generate();
                let bob = DeviceIdentity::generate();
                let (a, b) = tokio::io::duplex(65536);
                let server = tokio::spawn(async move {
                    let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                    assert!(matches!(
                        recv_have(&mut channel, want_req, conv()).await,
                        Err(SessionError::UnexpectedMessage)
                    ));
                });
                let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
                send_have(
                    &mut channel,
                    ConversationId::new([2; 32]),
                    &[],
                    if want_req {
                        SyncWire::ReqHave
                    } else {
                        SyncWire::RespHave
                    },
                )
                .await
                .unwrap();
                server.await.unwrap();
            }
        })
        .await
        .expect("malformed have exchange timed out");
    }

    #[tokio::test]
    async fn requester_rejects_foreign_event_in_matching_response() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let alice = DeviceIdentity::generate();
            let bob = DeviceIdentity::generate();
            let injected = Event::new(
                &bob,
                ConversationId::new([2; 32]),
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                b"unrelated".to_vec(),
            );
            let id = injected.id;
            let (a, b) = tokio::io::duplex(65536);
            let server = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                let _ = channel.recv().await.unwrap();
                channel
                    .send(&encode(&SyncWire::FpResponse(FpResponse { matched: false })).unwrap())
                    .await
                    .unwrap();
                let _ = channel.recv().await.unwrap();
                let _ = channel.recv().await.unwrap();
                channel
                    .send(
                        &encode(&SyncWire::Response(SyncResponse {
                            conversation: conv(),
                            events: vec![injected],
                            have: vec![],
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                let _ = send_have(&mut channel, conv(), &[], SyncWire::RespHave).await;
            });
            let store = Mutex::new(EventLog::default());
            let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
            assert!(matches!(
                request_round(&mut channel, &store, conv()).await,
                Err(SessionError::UnexpectedMessage)
            ));
            assert!(!store.lock().unwrap().has(&id));
            server.await.unwrap();
        })
        .await
        .expect("foreign event exchange timed out");
    }

    #[tokio::test]
    async fn one_round_converges_two_stores_over_a_channel() {
        // A SecureChannel pair over an in-memory duplex (no real sockets).
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        // A has one event; B is empty.
        let event = Event::new(
            &a_id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        let a_store = Mutex::new({
            let mut log = EventLog::default();
            log.append(event.clone()).unwrap();
            log
        });
        let event_id = event.id;

        // Responder: accept the channel and serve until the requester closes.
        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(EventLog::default());
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
            b_store.into_inner().unwrap()
        });

        // Requester: connect and run request_round (loops until converged), then drop.
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv()).await.unwrap();
        drop(a_ch);

        let b_log = server.await.unwrap();
        assert!(
            b_log.has(&event_id),
            "B received A's event via the sync round"
        );
        assert!(a_store.lock().unwrap().has(&event_id));
    }

    #[tokio::test]
    async fn fingerprint_match_short_circuits_the_exchange() {
        // A and B hold the IDENTICAL event set: the fingerprint probe must match and the
        // O(N) have-set exchange must be skipped entirely (server serves only the probe).
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        let event = Event::new(
            &a_id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        let event_id = event.id;
        let a_store = Mutex::new({
            let mut log = EventLog::default();
            log.append(event.clone()).unwrap();
            log
        });
        let event_for_b = event.clone();

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new({
                let mut log = EventLog::default();
                log.append(event_for_b).unwrap();
                log
            });
            let mut handled = 0u32;
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => handled += 1,
                }
            }
            handled
        });

        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        let report = request_round(&mut a_ch, &a_store, conv()).await.unwrap();
        drop(a_ch);
        let handled = server.await.unwrap();

        assert_eq!(report.applied, 0, "identical sets transfer no events");
        assert_eq!(
            handled, 1,
            "only the fingerprint probe is served — the O(N) id-set exchange is skipped"
        );
        assert!(a_store.lock().unwrap().has(&event_id));
    }

    #[tokio::test]
    async fn fingerprint_mismatch_merges_the_union_over_the_channel() {
        // A and B SHARE one event but each also holds a unique one: the fingerprint probe
        // must mismatch and the full reconciliation must merge to the UNION (both directions).
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        let shared = Event::new(
            &a_id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"shared".to_vec(),
        );
        let only_a = Event::new(
            &a_id,
            conv(),
            2,
            vec![shared.id],
            2,
            0,
            EventKind::Message,
            b"a".to_vec(),
        );
        let only_b = Event::new(
            &b_id,
            conv(),
            1,
            vec![shared.id],
            2,
            0,
            EventKind::Message,
            b"b".to_vec(),
        );
        let (sid, aid, bid) = (shared.id, only_a.id, only_b.id);

        let a_store = Mutex::new({
            let mut log = EventLog::default();
            log.append(shared.clone()).unwrap();
            log.append(only_a).unwrap();
            log
        });
        let shared_for_b = shared.clone();
        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new({
                let mut log = EventLog::default();
                log.append(shared_for_b).unwrap();
                log.append(only_b).unwrap();
                log
            });
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
            b_store.into_inner().unwrap()
        });

        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv()).await.unwrap();
        drop(a_ch);
        let b_log = server.await.unwrap();

        for id in [sid, aid, bid] {
            assert!(a_store.lock().unwrap().has(&id), "A converged to the union");
            assert!(b_log.has(&id), "B converged to the union");
        }
    }

    #[tokio::test]
    async fn round_pulls_events_from_the_responder_too() {
        // B (responder) has an event A lacks; one requester round must pull it to A.
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        let event = Event::new(
            &b_id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"y".to_vec(),
        );
        let event_id = event.id;

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new({
                let mut log = EventLog::default();
                log.append(event.clone()).unwrap();
                log
            });
            // Serve until the requester closes the channel.
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
        });

        let a_store = Mutex::new(EventLog::default());
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv()).await.unwrap();
        drop(a_ch);
        server.await.unwrap();

        assert!(
            a_store.lock().unwrap().has(&event_id),
            "A pulled B's event in one round"
        );
    }

    #[tokio::test]
    async fn serve_one_reports_closed_when_peer_hangs_up() {
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(EventLog::default());
            // The requester connects then drops without sending — recv sees EOF.
            serve_one(&mut b_ch, &b_store).await.unwrap()
        });

        let a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        drop(a_ch); // hang up immediately
        assert!(matches!(server.await.unwrap(), Served::Closed));
    }

    /// Build a responder store with `n` events in `conv`, each carrying a `payload_size`-byte
    /// ciphertext, so the combined encoded size can exceed MAX_PLAINTEXT.
    fn build_large_responder_store(
        id: &DeviceIdentity,
        conv: ConversationId,
        n: usize,
        payload_size: usize,
    ) -> EventLog {
        let mut log = EventLog::default();
        for i in 0..n {
            let ciphertext = vec![0xABu8; payload_size];
            let event = Event::new(
                id,
                conv,
                (i + 1) as u64,
                vec![],
                1,
                0,
                EventKind::Message,
                ciphertext,
            );
            log.append(event).unwrap();
        }
        log
    }

    #[tokio::test]
    async fn request_round_pushes_multiple_frames_when_remote_acceptance_advances() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let (aio, bio) = tokio::io::duplex(512 * 1024);
        let log = build_large_responder_store(&alice, conv(), 10, 20 * 1024);
        let ids = log.event_ids(&conv());
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(bio, &bob).await.unwrap();
            let store = Mutex::new(EventLog::default());
            let mut handled = 0;
            loop {
                match serve_one(&mut channel, &store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => handled += 1,
                }
            }
            (store.into_inner().unwrap(), handled)
        });
        let mut channel = SecureChannel::connect(aio, &alice, None).await.unwrap();
        let store = Mutex::new(log);
        let report = request_round(&mut channel, &store, conv()).await.unwrap();
        drop(channel);
        let (remote, handled) = server.await.unwrap();
        assert_eq!(
            report.applied, 0,
            "progress comes entirely from remote acceptance"
        );
        assert!(
            handled > 9,
            "transfer requires more rounds than the stall diagnostic cutoff"
        );
        assert_eq!(remote.event_ids(&conv()), ids);
        for event in store.lock().unwrap().events(&conv()) {
            assert_eq!(remote.get(&event.id), Some(event));
        }
    }

    #[tokio::test]
    async fn request_round_syncs_a_conversation_larger_than_one_frame() {
        // Build a responder store with 4 events of ~20 KB each (~80 KB total > MAX_PLAINTEXT=65519).
        // request_round loops internally until all events are transferred; assert full sync.
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        // Large duplex buffer so multiple frames can be in-flight.
        let (a_io, b_io) = tokio::io::duplex(512 * 1024);

        let conv_id = ConversationId::new([2u8; 32]);
        const EVENT_PAYLOAD: usize = 20 * 1024; // ~20 KB each → 4 events ≈ 80 KB > one frame
        const NUM_EVENTS: usize = 4;

        let b_log = build_large_responder_store(&b_id, conv_id, NUM_EVENTS, EVENT_PAYLOAD);
        let responder_event_count = b_log.event_ids(&conv_id).len();
        assert_eq!(responder_event_count, NUM_EVENTS);

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(b_log);
            // Serve messages until the requester closes the channel (multi-round loop).
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
            b_store.into_inner().unwrap()
        });

        // Requester starts empty; request_round loops until fully synced.
        let a_store = Mutex::new(EventLog::default());
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv_id).await.unwrap();
        // Drop the channel so the server's serve loop sees Closed and exits.
        drop(a_ch);

        let b_final = server.await.unwrap();
        let requester_count = a_store.lock().unwrap().event_ids(&conv_id).len();
        assert_eq!(
            requester_count, responder_event_count,
            "requester should have all {responder_event_count} events after multi-round sync"
        );
        // Responder also still has all events.
        assert_eq!(b_final.event_ids(&conv_id).len(), responder_event_count);
    }

    #[tokio::test]
    async fn request_round_syncs_a_conversation_with_more_ids_than_fit_one_frame() {
        // 2500 tiny events: the have id-set alone (2500 × 32 B ≈ 80 KB) exceeds
        // MAX_PLAINTEXT (65519). Before have-streaming this overflowed the frame and the
        // conversation could no longer sync; now the have set is chunked, so it converges.
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(1024 * 1024);
        let conv_id = ConversationId::new([3u8; 32]);
        const NUM_EVENTS: usize = 2500;

        let b_log = build_large_responder_store(&b_id, conv_id, NUM_EVENTS, 8);
        assert_eq!(b_log.event_ids(&conv_id).len(), NUM_EVENTS);

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(b_log);
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
        });

        let a_store = Mutex::new(EventLog::default());
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv_id).await.unwrap();
        drop(a_ch);
        server.await.unwrap();

        assert_eq!(
            a_store.lock().unwrap().event_ids(&conv_id).len(),
            NUM_EVENTS,
            "requester pulled all events despite a >1-frame have id-set",
        );
    }
}
