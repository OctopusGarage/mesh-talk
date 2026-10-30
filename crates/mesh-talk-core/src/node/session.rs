//! Driving event-log reconciliation over a [`SecureChannel`] — the networked
//! counterpart of Plan-4's in-process `reconcile`. The same handlers
//! (`build_request`/`handle_request`/`handle_response`/`handle_followup`) run on
//! each side; the three messages are framed (bincode) and sent over the channel.
//! The requester runs one round with [`request_round`]; the responder serves
//! rounds one message at a time with [`serve_one`].
//!
//! The store is shared as a `&Mutex<S>` and locked only for the synchronous
//! handler calls — never across an `.await` — so a `std::sync::Mutex` is correct.

use crate::eventlog::event::{ConversationId, EventId, EventKind};
use crate::eventlog::sync::network::NetworkSyncStore;
use crate::eventlog::sync::{
    ApplyReport, FileConsentStatus, FpRequest, FpResponse, MAX_REJECTED_DETAIL, SyncFollowup,
    SyncRequest, SyncResponse, SyncStore,
};
use crate::transport::{MAX_PLAINTEXT, SecureChannel, TransportError};
mod backend;
mod have_budget;
use backend::*;
use bincode::Options;
pub use have_budget::RemoteHaveBudget;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

/// Backstop on sync rounds for one conversation (a peer that resends
/// non-applying events forever can't spin us indefinitely).
const MAX_SYNC_ROUNDS: usize = 10_000;

/// Keep file bursts short enough for the transfer scheduler's per-step deadline
/// and to let other destinations make progress. Existing peers already accept
/// consecutive Followup frames, so this does not change the wire format.
pub(in crate::node) const FILE_FOLLOWUP_BURST: usize = 4;

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
pub(super) const MAX_HAVE_IDS_TOTAL: usize = 200_000;

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
    FileConsentRequest {
        file: ConversationId,
        manifest: EventId,
    },
    FileConsentResponse {
        file: ConversationId,
        manifest: EventId,
        status: FileConsentStatus,
    },
    /// Appended variants gate kind-14 events from pre-replica builds. Older
    /// decoders close on these before receiving an unsupported event.
    FpRequestV2(FpRequest),
    RequestV2(SyncRequest),
    FollowupV2(SyncFollowup),
    UpgradeRequired {
        conversation: ConversationId,
    },
    SignedFileConsentRequest {
        file: ConversationId,
        manifest: EventId,
    },
    SignedFileConsentResponse {
        file: ConversationId,
        manifest: EventId,
        control: Option<crate::eventlog::Event>,
    },
    /// History policy and key release require support for kinds 15 and 16.
    /// Keep these appended so every earlier envelope retains its wire tag.
    FpRequestV3(FpRequest),
    RequestV3(SyncRequest),
    FollowupV3(SyncFollowup),
    /// Account-principal membership and certified-device inheritance (17/18).
    FpRequestV4(FpRequest),
    RequestV4(SyncRequest),
    FollowupV4(SyncFollowup),
    /// Optional local-artwork reuse; old decoders close this disposable session.
    StickerFileConsentRequest {
        file: ConversationId,
        manifest: EventId,
        headers: Vec<crate::eventlog::Event>,
    },
    StickerFileConsentResponse {
        file: ConversationId,
        manifest: EventId,
        control: Option<crate::eventlog::Event>,
    },
    StickerFileProofRequest {
        file: ConversationId,
        manifest: EventId,
    },
    StickerFileProofResponse {
        file: ConversationId,
        manifest: EventId,
        headers: Option<Vec<crate::eventlog::Event>>,
    },
    /// Appended optional metadata exchange; MFM3 and history card bytes stay frozen.
    FileMessageIdentitiesRequest {
        requests: Vec<(ConversationId, EventId)>,
        offers: Vec<crate::eventlog::Event>,
    },
    FileMessageIdentitiesResponse {
        bundles: Vec<crate::eventlog::Event>,
    },
}

fn supports_event_kind(kind: EventKind, v2: bool, v3: bool, v4: bool) -> bool {
    match kind {
        EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance => v4,
        EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory => v3,
        EventKind::ChannelReplica => v2,
        _ => true,
    }
}

#[cfg(test)]
pub(in crate::node) fn is_followup(bytes: &[u8], conversation: ConversationId) -> bool {
    matches!(decode(bytes), Ok(SyncWire::Followup(frame) | SyncWire::FollowupV2(frame) | SyncWire::FollowupV3(frame) | SyncWire::FollowupV4(frame)) if frame.conversation == conversation)
}

#[cfg(test)]
pub(in crate::node) fn round_request_conversation(bytes: &[u8]) -> Option<ConversationId> {
    match decode(bytes) {
        Ok(
            SyncWire::Request(request)
            | SyncWire::RequestV2(request)
            | SyncWire::RequestV3(request)
            | SyncWire::RequestV4(request),
        ) => Some(request.conversation),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn sticker_test_frame(bytes: &[u8], file: ConversationId) -> (bool, usize) {
    let (proof, events) = match decode(bytes) {
        Ok(SyncWire::StickerFileConsentRequest { headers, .. }) => (true, headers),
        Ok(SyncWire::StickerFileProofRequest { .. }) => (true, vec![]),
        Ok(SyncWire::StickerFileProofResponse { headers, .. }) => {
            (true, headers.unwrap_or_default())
        }
        Ok(SyncWire::Response(response)) => (false, response.events),
        Ok(
            SyncWire::Followup(frame)
            | SyncWire::FollowupV2(frame)
            | SyncWire::FollowupV3(frame)
            | SyncWire::FollowupV4(frame),
        ) => (false, frame.events),
        _ => return (false, 0),
    };
    (
        proof,
        events
            .iter()
            .filter(|event| event.conversation_id == file)
            .map(|event| event.ciphertext.len())
            .sum(),
    )
}

#[cfg(test)]
pub(in crate::node) fn requests_response(bytes: &[u8]) -> bool {
    matches!(
        decode(bytes),
        Ok(SyncWire::Request(_)
            | SyncWire::RequestV2(_)
            | SyncWire::RequestV3(_)
            | SyncWire::RequestV4(_)
            | SyncWire::FpRequest(_)
            | SyncWire::FpRequestV2(_)
            | SyncWire::FpRequestV3(_)
            | SyncWire::FpRequestV4(_)
            | SyncWire::DurableHaveRequest { .. }
            | SyncWire::FileConsentRequest { .. }
            | SyncWire::SignedFileConsentRequest { .. }
            | SyncWire::StickerFileConsentRequest { .. }
            | SyncWire::StickerFileProofRequest { .. }
            | SyncWire::FileMessageIdentitiesRequest { .. })
    )
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
    /// Local event-store operation failed.
    Store(crate::eventlog::LogError),
    /// Channel send/recv failure.
    Transport(TransportError),
    /// (De)serialization of a wire message failed.
    Serialization(String),
    /// Received a wire message that doesn't fit the protocol state.
    UnexpectedMessage,
    /// Neither local application nor acceptance of the previous push advanced.
    NoProgress,
    /// Local process RAM admission/allocation failed; retry on a fresh channel.
    ResourceUnavailable,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Store(e) => write!(f, "sync store error: {e}"),
            SessionError::Transport(e) => write!(f, "sync transport error: {e}"),
            SessionError::Serialization(m) => write!(f, "sync serialization error: {m}"),
            SessionError::UnexpectedMessage => write!(f, "unexpected sync message"),
            SessionError::ResourceUnavailable => write!(f, "local sync resources unavailable"),
            SessionError::NoProgress => write!(f, "sync made no progress"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}
impl From<crate::eventlog::LogError> for SessionError {
    fn from(error: crate::eventlog::LogError) -> Self {
        Self::Store(error)
    }
}

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

/// Query a recipient after the manifest is durable and before a chunk is sent.
/// An older endpoint closes on this appended variant; the caller handles that
/// explicit compatibility fallback on a fresh connection.
pub(crate) async fn request_file_consent<IO>(
    channel: &mut SecureChannel<IO>,
    file: ConversationId,
    manifest: EventId,
) -> Result<FileConsentStatus, SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    channel
        .send(&encode(&SyncWire::FileConsentRequest { file, manifest })?)
        .await?;
    match decode(&channel.recv().await?)? {
        SyncWire::FileConsentResponse {
            file: got_file,
            manifest: got_manifest,
            status,
        } if got_file == file && got_manifest == manifest => Ok(status),
        _ => Err(SessionError::UnexpectedMessage),
    }
}

pub(crate) async fn request_signed_file_consent<IO>(
    channel: &mut SecureChannel<IO>,
    file: ConversationId,
    manifest: EventId,
) -> Result<Option<crate::eventlog::Event>, SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    channel
        .send(&encode(&SyncWire::SignedFileConsentRequest {
            file,
            manifest,
        })?)
        .await?;
    match decode(&channel.recv().await?)? {
        SyncWire::SignedFileConsentResponse {
            file: got_file,
            manifest: got_manifest,
            control,
        } if file == got_file && manifest == got_manifest => Ok(control),
        _ => Err(SessionError::UnexpectedMessage),
    }
}

/// Offer only signed metadata for local reconstruction before ordinary consent.
pub(crate) async fn request_sticker_file_consent<IO>(
    channel: &mut SecureChannel<IO>,
    file: ConversationId,
    manifest: EventId,
    headers: Vec<crate::eventlog::Event>,
) -> Result<Option<crate::eventlog::Event>, SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    channel
        .send(&encode(&SyncWire::StickerFileConsentRequest {
            file,
            manifest,
            headers,
        })?)
        .await?;
    match decode(&channel.recv().await?)? {
        SyncWire::StickerFileConsentResponse {
            file: got_file,
            manifest: got_manifest,
            control,
        } if file == got_file && manifest == got_manifest => Ok(control),
        _ => Err(SessionError::UnexpectedMessage),
    }
}

/// Ask an authorized source for metadata, never an image or a public hash lookup.
pub(crate) async fn request_sticker_file_proof<IO>(
    channel: &mut SecureChannel<IO>,
    file: ConversationId,
    manifest: EventId,
) -> Result<Option<Vec<crate::eventlog::Event>>, SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    channel
        .send(&encode(&SyncWire::StickerFileProofRequest {
            file,
            manifest,
        })?)
        .await?;
    match decode(&channel.recv().await?)? {
        SyncWire::StickerFileProofResponse {
            file: got_file,
            manifest: got_manifest,
            headers,
        } if file == got_file && manifest == got_manifest => Ok(headers),
        _ => Err(SessionError::UnexpectedMessage),
    }
}

pub(in crate::node) const MAX_FILE_IDENTITIES: usize = 8;

/// Optional exact-ID metadata, separately negotiated from attachment bytes/history.
pub(crate) async fn request_file_message_identities<IO>(
    channel: &mut SecureChannel<IO>,
    requests: Vec<(ConversationId, EventId)>,
    offers: Vec<crate::eventlog::Event>,
) -> Result<Vec<crate::eventlog::Event>, SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    if requests.len() > MAX_FILE_IDENTITIES || offers.len() > MAX_FILE_IDENTITIES {
        return Err(SessionError::UnexpectedMessage);
    }
    channel
        .send(&encode(&SyncWire::FileMessageIdentitiesRequest {
            requests,
            offers,
        })?)
        .await?;
    match decode(&channel.recv().await?)? {
        SyncWire::FileMessageIdentitiesResponse { bundles }
            if bundles.len() <= MAX_FILE_IDENTITIES =>
        {
            Ok(bundles)
        }
        _ => Err(SessionError::UnexpectedMessage),
    }
}

/// Stream a `have` id-set as chunk frames (always ≥1 frame, so the receiver gets an
/// explicit terminator even for an empty set). `wrap` selects `ReqHave` vs `RespHave`.
#[cfg(test)]
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
        let required = have.len() + chunk.ids.len();
        if required > have.capacity() {
            let target = required
                .max(have.capacity().saturating_mul(2))
                .min(MAX_HAVE_IDS_TOTAL);
            have.try_reserve_exact(target - have.len())
                .map_err(|_| SessionError::ResourceUnavailable)?;
            if have.capacity() > MAX_HAVE_IDS_TOTAL {
                return Err(SessionError::ResourceUnavailable);
            }
        }
        have.extend(chunk.ids);
        if !chunk.more {
            return Ok(have);
        }
    }
    Err(SessionError::UnexpectedMessage)
}

async fn recv_have_owned<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    want_req: bool,
    conversation: ConversationId,
    budget: Option<&RemoteHaveBudget>,
) -> Result<have_budget::ReceivedHave, SessionError> {
    // Never await admission: requester/responder endpoints may already be in
    // opposite network phases while holding other resources.
    let permit = budget.map(RemoteHaveBudget::try_admit).transpose()?;
    let ids = recv_have(channel, want_req, conversation).await?;
    have_budget::ReceivedHave::from_vec(ids, permit)
}

/// Run one reconciliation round as the REQUESTER over `channel`, for
/// `conversation`: send a Request (+ our streamed have), receive the Response
/// (ingesting its events) + the responder's streamed have, then send the Followup.
/// Converges the conversation in both directions. Returns what this side applied.
/// This compatibility entrypoint explicitly uses the legacy collecting adapter;
/// bounded source consumers call [`request_round_paged`].
pub async fn request_round<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<ApplyReport, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let mut progress = SyncProgress::default();
    while !progress.step(channel, store, conversation).await? {}
    log::debug!(
        target: "mesh_talk::sync",
        "sync conv={} have={} have_kib={} applied={} dup={} pushed={} diff={} rounds={}",
        hex::encode(&conversation.as_bytes()[..4]),
        progress.first_have,
        progress.first_have * EVENT_ID_BYTES / 1024,
        progress.total.applied,
        progress.total.duplicates,
        progress.pushed_total,
        progress.total.applied + progress.pushed_total,
        progress.rounds,
    );
    Ok(progress.total)
}

/// Resume only between complete protocol exchanges, never midway through
/// a Noise frame. A caller timing out a step must discard its channel and state.
#[derive(Default)]
pub(in crate::node) struct SyncProgress {
    started: bool,
    v2: bool,
    v3: bool,
    v4: bool,
    rounds: usize,
    previous_push: std::collections::HashSet<EventId>,
    total: ApplyReport,
    first_have: usize,
    pushed_total: usize,
    /// Intersection of local advertised IDs and peer availability, never custody.
    remote_have_count: Option<usize>,
}

impl SyncProgress {
    pub(in crate::node) fn remote_have_count(&self) -> Option<usize> {
        self.remote_have_count
    }

    pub(in crate::node) async fn step<S, IO>(
        &mut self,
        channel: &mut SecureChannel<IO>,
        store: &Mutex<S>,
        conversation: ConversationId,
    ) -> Result<bool, SessionError>
    where
        S: SyncStore,
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.step_burst(channel, store, conversation, 1).await
    }

    pub(in crate::node) async fn step_burst<S, IO>(
        &mut self,
        channel: &mut SecureChannel<IO>,
        store: &Mutex<S>,
        conversation: ConversationId,
        max_followups: usize,
    ) -> Result<bool, SessionError>
    where
        S: SyncStore,
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.step_backend(
            channel,
            &LegacyCollectingNetworkAdapter(store),
            conversation,
            max_followups,
        )
        .await
    }

    pub(in crate::node) async fn step_burst_paged<
        S: NetworkSyncStore,
        IO: AsyncRead + AsyncWrite + Unpin,
    >(
        &mut self,
        channel: &mut SecureChannel<IO>,
        store: &Mutex<S>,
        conversation: ConversationId,
        max_followups: usize,
    ) -> Result<bool, SessionError> {
        self.step_backend(
            channel,
            &PagedNetworkBackend(store, None),
            conversation,
            max_followups,
        )
        .await
    }
    async fn step_backend<B: SessionBackend, IO: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        channel: &mut SecureChannel<IO>,
        backend: &B,
        conversation: ConversationId,
        max_followups: usize,
    ) -> Result<bool, SessionError> {
        if max_followups == 0 || max_followups > FILE_FOLLOWUP_BURST {
            return Err(SessionError::UnexpectedMessage);
        }
        // Phase 0 — fingerprint short-circuit. Exchange a single 32-byte digest of our whole
        // id-set; if the responder's matches, the logs are identical and we skip the O(N)
        // have-set streaming entirely (the common idle-drain case). A mismatch falls through to
        // the full reconciliation below. (Correctness: a digest match means equal sets; a
        // false match is cryptographically negligible and, since ingest re-validates, could
        // only skip a diff — never corrupt — so this is safe.)
        if !self.started {
            self.started = true;
            let (metadata, probe_view) = backend.probe_metadata(&conversation)?;
            let local_count = usize::try_from(metadata.count)
                .map_err(|_| crate::eventlog::LogError::RepositoryCounterExhausted)?;
            let (local_v2, local_v3, local_v4) = (
                metadata.channel_v2,
                metadata.channel_v3,
                metadata.channel_v4,
            );
            self.v2 |= local_v2 || local_v3 || local_v4;
            self.v3 |= local_v3 || local_v4;
            self.v4 |= local_v4;
            if let Some(fp) = metadata.fingerprint {
                if let Some(view) = &probe_view {
                    backend.validate_view(view)?;
                }
                let request = FpRequest {
                    conversation,
                    fingerprint: fp,
                };
                channel
                    .send(&encode(&if self.v4 {
                        SyncWire::FpRequestV4(request)
                    } else if self.v3 {
                        SyncWire::FpRequestV3(request)
                    } else if self.v2 {
                        SyncWire::FpRequestV2(request)
                    } else {
                        SyncWire::FpRequest(request)
                    })?)
                    .await?;
                match decode(&channel.recv().await?)? {
                    SyncWire::UpgradeRequired { conversation: c }
                        if c == conversation && !self.v4 =>
                    {
                        self.v4 = self.v3;
                        self.v3 = self.v2;
                        self.v2 = true;
                        self.started = false;
                        return Ok(false);
                    }
                    SyncWire::FpResponse(FpResponse { matched: true }) => {
                        self.remote_have_count = Some(local_count);
                        log::debug!(
                            target: "mesh_talk::sync",
                            "sync conv={} fp_match skipped",
                            hex::encode(&conversation.as_bytes()[..4]),
                        );
                        return Ok(true);
                    }
                    SyncWire::FpResponse(FpResponse { matched: false }) => {
                        // A complete exchange is a scheduling boundary for
                        // bounded callers, even when reconciliation must follow.
                        return Ok(false);
                    }
                    _ => return Err(SessionError::UnexpectedMessage),
                }
            }
        }

        // Sync-cost telemetry (see node/session.rs sync docs): we re-stream the FULL id-set
        // (`have`) every round in both directions regardless of how small the diff is, so the
        // metadata cost is O(N) per round. Capture N, the diff, and the round count so we can
        // evaluate reconciliation scaling (e.g. range-based / Negentropy) on real workloads.
        let round = self.rounds;
        let have_view = backend.open_view(&conversation)?;
        let local_count = have_count(backend.advertised_count(&have_view)?)?;
        if round == 0 {
            self.first_have = local_count;
        }
        self.rounds += 1;
        // Opening Request carries an empty inline have; the real set is streamed.
        let request = SyncRequest {
            conversation,
            have: Vec::new(),
        };
        channel
            .send(&encode(&if self.v4 {
                SyncWire::RequestV4(request)
            } else if self.v3 {
                SyncWire::RequestV3(request)
            } else if self.v2 {
                SyncWire::RequestV2(request)
            } else {
                SyncWire::Request(request)
            })?)
            .await?;
        send_have_pages(
            channel,
            backend,
            &have_view,
            conversation,
            SyncWire::ReqHave,
        )
        .await?;

        // Response carries the (bounded) events; the responder's have is streamed after.
        let response = match decode(&channel.recv().await?)? {
            SyncWire::Response(r) => r,
            SyncWire::UpgradeRequired { conversation: c } if c == conversation && !self.v4 => {
                self.v4 = self.v3;
                self.v3 = self.v2;
                self.v2 = true;
                self.started = false;
                return Ok(false);
            }
            _ => return Err(SessionError::UnexpectedMessage),
        };
        if response.conversation != conversation
            || response.events.iter().any(|event| {
                event.conversation_id != conversation
                    || !supports_event_kind(event.kind, self.v2, self.v3, self.v4)
            })
        {
            return Err(SessionError::UnexpectedMessage);
        }
        let remote_have =
            recv_have_owned(channel, false, conversation, backend.remote_have_budget()).await?;
        self.remote_have_count = Some(intersection_count(backend, &have_view, &remote_have).await?);
        drop(have_view);
        let report = backend.apply(&response.events)?;
        if backend.admission_denied() {
            return Err(SessionError::UnexpectedMessage);
        }
        // Ingest may add causal parents; select a fresh outgoing cutoff.
        let outgoing_view = backend.open_view(&conversation)?;
        let mut followups = select_export_frames(
            backend,
            &outgoing_view,
            conversation,
            &remote_have,
            max_followups,
        )
        .await?;
        let made_progress = report.applied > 0;
        // New controls may arrive after the fingerprint probe. Complete this
        // round empty and restart with the required capability before sending.
        let needs_v3 = followups
            .iter()
            .flat_map(|frame| &frame.events)
            .any(|event| !supports_event_kind(event.kind, true, false, false));
        let needs_v4 = followups
            .iter()
            .flat_map(|frame| &frame.events)
            .any(|event| !supports_event_kind(event.kind, true, true, false));
        let unsupported = followups
            .iter()
            .flat_map(|frame| &frame.events)
            .any(|event| !supports_event_kind(event.kind, self.v2, self.v3, self.v4));
        if unsupported {
            let empty = SyncFollowup {
                conversation,
                events: Vec::new(),
            };
            let wire = if self.v4 {
                SyncWire::FollowupV4(empty)
            } else if self.v3 {
                SyncWire::FollowupV3(empty)
            } else if self.v2 {
                SyncWire::FollowupV2(empty)
            } else {
                SyncWire::Followup(empty)
            };
            channel.send(&encode(&wire)?).await?;
            self.total.applied += report.applied;
            self.total.duplicates += report.duplicates;
            self.v2 = true;
            self.v3 |= needs_v3;
            self.v4 |= needs_v4;
            self.started = false;
            self.previous_push.clear();
            return Ok(false);
        }
        // Sending is not progress: projected stores can ingest a duplicate but
        // omit it from their Have set. Require evidence of acceptance before
        // repeating a push, unless this round advanced our own store instead.
        if !self.previous_push.is_empty()
            && !made_progress
            && !remote_have.iter().any(|id| self.previous_push.contains(id))
        {
            return Err(SessionError::NoProgress);
        }
        let more_to_push = !followups.is_empty();
        self.pushed_total += followups
            .iter()
            .map(|frame| frame.events.len())
            .sum::<usize>();
        self.previous_push = followups
            .iter()
            .flat_map(|frame| frame.events.iter().map(|event| event.id))
            .collect();
        if followups.is_empty() {
            followups.push(SyncFollowup {
                conversation,
                events: Vec::new(),
            });
        }
        for followup in followups {
            backend.validate_view(&outgoing_view)?;
            channel
                .send(&encode(&if self.v4 {
                    SyncWire::FollowupV4(followup)
                } else if self.v3 {
                    SyncWire::FollowupV3(followup)
                } else if self.v2 {
                    SyncWire::FollowupV2(followup)
                } else {
                    SyncWire::Followup(followup)
                })?)
                .await?;
        }

        self.total.applied += report.applied;
        self.total.duplicates += report.duplicates;
        // Bound accumulated reject detail across rounds (diagnostics only) so a peer that
        // keeps a multi-round sync alive while interleaving bad events can't grow it.
        if self.total.rejected.len() < MAX_REJECTED_DETAIL {
            self.total.rejected.extend(report.rejected);
        }

        if !made_progress && !more_to_push {
            return Ok(true);
        }
        if round + 1 == MAX_SYNC_ROUNDS {
            return Err(SessionError::NoProgress);
        }
        Ok(false)
    }
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
/// clean close so the serve loop can stop). This compatibility entrypoint uses
/// the legacy collecting adapter; bounded sources call [`serve_one_paged`].
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
/// This is the legacy collecting wrapper; use [`serve_wire_bytes_paged`] for a
/// mandatory paged source.
pub async fn serve_wire_bytes<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    bytes: &[u8],
) -> Result<Served, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    serve_wire_backend(channel, &LegacyCollectingNetworkAdapter(store), bytes).await
}

async fn serve_wire_backend<B: SessionBackend, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    backend: &B,
    bytes: &[u8],
) -> Result<Served, SessionError> {
    match decode(bytes)? {
        SyncWire::FileMessageIdentitiesRequest { requests, offers } => {
            if requests.len() > MAX_FILE_IDENTITIES || offers.len() > MAX_FILE_IDENTITIES {
                return Err(SessionError::UnexpectedMessage);
            }
            let conversation = requests
                .first()
                .map(|(file, _)| *file)
                .or_else(|| offers.first().map(|event| event.conversation_id))
                .unwrap_or(ConversationId::new([0; 32]));
            for bundle in offers {
                backend.accept_file_message_identity(bundle)?;
            }
            let mut seen = std::collections::HashSet::new();
            let mut bundles = Vec::new();
            for (file, manifest) in requests {
                if let Some(bundle) = backend.file_message_identity(&file, &manifest)?
                    && seen.insert(bundle.id)
                {
                    bundles.push(bundle);
                }
            }
            channel
                .send(&encode(&SyncWire::FileMessageIdentitiesResponse {
                    bundles,
                })?)
                .await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::StickerFileProofRequest { file, manifest } => {
            let headers = backend.sticker_file_proof(&file, &manifest)?;
            channel
                .send(&encode(&SyncWire::StickerFileProofResponse {
                    file,
                    manifest,
                    headers,
                })?)
                .await?;
            Ok(Served::Handled(file))
        }
        SyncWire::StickerFileConsentRequest {
            file,
            manifest,
            headers,
        } => {
            let control = if headers.len() <= 64
                && headers.iter().all(|event| {
                    event.ciphertext.is_empty() && event.parents.len() <= 1 && event.sig.len() == 64
                }) {
                backend.sticker_file_consent(&file, &manifest, headers)?
            } else {
                None
            };
            channel
                .send(&encode(&SyncWire::StickerFileConsentResponse {
                    file,
                    manifest,
                    control,
                })?)
                .await?;
            Ok(Served::Handled(file))
        }
        SyncWire::SignedFileConsentRequest { file, manifest } => {
            let control = backend.signed_file_consent(&file, &manifest)?;
            channel
                .send(&encode(&SyncWire::SignedFileConsentResponse {
                    file,
                    manifest,
                    control,
                })?)
                .await?;
            Ok(Served::Handled(file))
        }
        SyncWire::FileConsentRequest { file, manifest } => {
            let status = backend.file_consent(&file, &manifest)?;
            channel
                .send(&encode(&SyncWire::FileConsentResponse {
                    file,
                    manifest,
                    status,
                })?)
                .await?;
            Ok(Served::Handled(file))
        }
        SyncWire::DurableHaveRequest {
            conversation,
            event_id,
        } => {
            let stored = backend.durable_have(&conversation, &event_id)?;
            channel
                .send(&encode(&SyncWire::DurableHaveResponse {
                    conversation,
                    event_id,
                    stored,
                })?)
                .await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::Request(request)
        | SyncWire::RequestV2(request)
        | SyncWire::RequestV3(request)
        | SyncWire::RequestV4(request) => {
            let conversation = request.conversation;
            let v4 = matches!(decode(bytes)?, SyncWire::RequestV4(_));
            let v3 = v4 || matches!(decode(bytes)?, SyncWire::RequestV3(_));
            let v2 = v3 || matches!(decode(bytes)?, SyncWire::RequestV2(_));
            // The Request's inline have is empty on the networked path; read the
            // requester's streamed have, then answer.
            let have =
                recv_have_owned(channel, true, conversation, backend.remote_have_budget()).await?;
            let view = backend.open_view(&conversation)?;
            let metadata = backend.metadata(&view)?;
            have_count(metadata.count)?;
            let response = if unsupported_metadata(metadata, v2, v3, v4) {
                None
            } else {
                let mut frames =
                    select_export_frames(backend, &view, conversation, &have, 1).await?;
                let events = frames.pop().map_or_else(Vec::new, |frame| frame.events);
                events
                    .iter()
                    .all(|event| supports_event_kind(event.kind, v2, v3, v4))
                    .then_some(events)
            };
            let Some(response) = response else {
                channel
                    .send(&encode(&SyncWire::UpgradeRequired { conversation })?)
                    .await?;
                return Ok(Served::Handled(conversation));
            };
            backend.validate_view(&view)?;
            channel
                .send(&encode(&SyncWire::Response(SyncResponse {
                    conversation,
                    events: response,
                    have: Vec::new(),
                }))?)
                .await?;
            send_have_pages(channel, backend, &view, conversation, SyncWire::RespHave).await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::Followup(followup)
        | SyncWire::FollowupV2(followup)
        | SyncWire::FollowupV3(followup)
        | SyncWire::FollowupV4(followup) => {
            let conversation = followup.conversation;
            let v4 = matches!(decode(bytes)?, SyncWire::FollowupV4(_));
            let v3 = v4 || matches!(decode(bytes)?, SyncWire::FollowupV3(_));
            let v2 = v3 || matches!(decode(bytes)?, SyncWire::FollowupV2(_));
            if followup.events.iter().any(|event| {
                event.conversation_id != conversation
                    || !supports_event_kind(event.kind, v2, v3, v4)
            }) {
                return Err(SessionError::UnexpectedMessage);
            }
            backend.apply(&followup.events)?;
            if backend.admission_denied() {
                return Err(SessionError::UnexpectedMessage);
            }
            Ok(Served::Handled(conversation))
        }
        SyncWire::FpRequest(req)
        | SyncWire::FpRequestV2(req)
        | SyncWire::FpRequestV3(req)
        | SyncWire::FpRequestV4(req) => {
            // Compare the requester's whole-set digest to ours. On match the requester
            // skips the full exchange; on mismatch it follows up with a normal Request.
            let conversation = req.conversation;
            let v4 = matches!(decode(bytes)?, SyncWire::FpRequestV4(_));
            let v3 = v4 || matches!(decode(bytes)?, SyncWire::FpRequestV3(_));
            let v2 = v3 || matches!(decode(bytes)?, SyncWire::FpRequestV2(_));
            let (metadata, probe_view) = backend.probe_metadata(&conversation)?;
            let matched = if unsupported_metadata(metadata, v2, v3, v4) {
                None
            } else {
                Some(metadata.fingerprint.is_some_and(|fp| fp == req.fingerprint))
            };
            let Some(matched) = matched else {
                channel
                    .send(&encode(&SyncWire::UpgradeRequired { conversation })?)
                    .await?;
                return Ok(Served::Handled(conversation));
            };
            if let Some(view) = &probe_view {
                backend.validate_view(view)?;
            }
            channel
                .send(&encode(&SyncWire::FpResponse(FpResponse { matched }))?)
                .await?;
            Ok(Served::Handled(conversation))
        }
        // A Response/FpResponse, or a stray have-chunk outside its stream, is out of order.
        SyncWire::Response(_)
        | SyncWire::ReqHave(_)
        | SyncWire::RespHave(_)
        | SyncWire::FpResponse(_)
        | SyncWire::UpgradeRequired { .. } => Err(SessionError::UnexpectedMessage),
        SyncWire::DurableHaveResponse { .. }
        | SyncWire::FileConsentResponse { .. }
        | SyncWire::SignedFileConsentResponse { .. }
        | SyncWire::StickerFileConsentResponse { .. }
        | SyncWire::StickerFileProofResponse { .. }
        | SyncWire::FileMessageIdentitiesResponse { .. } => Err(SessionError::UnexpectedMessage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::identity::device::DeviceIdentity;

    #[derive(Clone, Copy)]
    enum ReadFault {
        Fingerprint,
        Capabilities,
        Ids,
        Body,
        Ingest,
        Durable,
        Consent,
        SignedConsent,
        FileIdentity,
        AcceptFileIdentity,
        StickerProof,
        StickerConsent,
    }
    struct FailedReadStore(ReadFault);
    impl FailedReadStore {
        fn error() -> crate::eventlog::LogError {
            std::io::Error::other("injected local read failure").into()
        }
    }
    impl SyncStore for FailedReadStore {
        fn event_ids(&self, _: &ConversationId) -> Result<Vec<EventId>, crate::eventlog::LogError> {
            if matches!(self.0, ReadFault::Ids) {
                Err(Self::error())
            } else {
                Ok(vec![])
            }
        }
        fn events_excluding(
            &self,
            _: &ConversationId,
            _: &std::collections::HashSet<EventId>,
        ) -> Result<Vec<Event>, crate::eventlog::LogError> {
            if matches!(self.0, ReadFault::Body | ReadFault::Capabilities) {
                Err(Self::error())
            } else {
                Ok(vec![])
            }
        }
        fn fingerprint_and_len(
            &self,
            _: &ConversationId,
        ) -> Result<([u8; 32], usize), crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn durable_have(
            &self,
            _: &ConversationId,
            _: &EventId,
        ) -> Result<bool, crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn file_consent(
            &self,
            _: &ConversationId,
            _: &EventId,
        ) -> Result<FileConsentStatus, crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn signed_file_consent(
            &self,
            _: &ConversationId,
            _: &EventId,
        ) -> Result<Option<Event>, crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn file_message_identity(
            &self,
            _: &ConversationId,
            _: &EventId,
        ) -> Result<Option<Event>, crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn accept_file_message_identity(
            &mut self,
            _: Event,
        ) -> Result<bool, crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn sticker_file_proof(
            &self,
            _: &ConversationId,
            _: &EventId,
        ) -> Result<Option<Vec<Event>>, crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn sticker_file_consent(
            &mut self,
            _: &ConversationId,
            _: &EventId,
            _: Vec<Event>,
        ) -> Result<Option<Event>, crate::eventlog::LogError> {
            Err(Self::error())
        }
        fn ingest(
            &mut self,
            _: Event,
        ) -> Result<crate::eventlog::AppendOutcome, crate::eventlog::LogError> {
            Err(Self::error())
        }
    }

    #[tokio::test]
    async fn store_read_failures_over_loopback_emit_no_success_reply() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            for fault in [
                ReadFault::Fingerprint,
                ReadFault::Capabilities,
                ReadFault::Ids,
                ReadFault::Body,
                ReadFault::Ingest,
                ReadFault::Durable,
                ReadFault::Consent,
                ReadFault::SignedConsent,
                ReadFault::FileIdentity,
                ReadFault::AcceptFileIdentity,
                ReadFault::StickerProof,
                ReadFault::StickerConsent,
            ] {
                let alice = DeviceIdentity::generate();
                let bob = DeviceIdentity::generate();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut channel = SecureChannel::accept(stream, &bob).await.unwrap();
                    let result = serve_one(&mut channel, &Mutex::new(FailedReadStore(fault))).await;
                    assert!(matches!(
                        result,
                        Err(SessionError::Store(crate::eventlog::LogError::Io(_)))
                    ));
                    let error = result.unwrap_err();
                    let source = std::error::Error::source(&error).unwrap();
                    assert!(source.downcast_ref::<crate::eventlog::LogError>().is_some());
                    assert!(
                        source
                            .source()
                            .unwrap()
                            .downcast_ref::<std::io::Error>()
                            .is_some()
                    );
                });
                let stream = tokio::net::TcpStream::connect(address).await.unwrap();
                let mut channel = SecureChannel::connect(stream, &alice, None).await.unwrap();
                let event_id = EventId::new([3; 32]);
                let wire = match fault {
                    ReadFault::Fingerprint => SyncWire::FpRequestV4(FpRequest {
                        conversation: conv(),
                        fingerprint: crate::eventlog::sync::fingerprint(vec![]),
                    }),
                    ReadFault::Capabilities => SyncWire::FpRequest(FpRequest {
                        conversation: conv(),
                        fingerprint: crate::eventlog::sync::fingerprint(vec![]),
                    }),
                    ReadFault::Ingest => SyncWire::FollowupV4(SyncFollowup {
                        conversation: conv(),
                        events: vec![Event::new(
                            &alice,
                            conv(),
                            0,
                            vec![],
                            1,
                            0,
                            EventKind::Message,
                            vec![1],
                        )],
                    }),
                    ReadFault::Ids | ReadFault::Body => SyncWire::RequestV4(SyncRequest {
                        conversation: conv(),
                        have: vec![],
                    }),
                    ReadFault::Durable => SyncWire::DurableHaveRequest {
                        conversation: conv(),
                        event_id,
                    },
                    ReadFault::Consent => SyncWire::FileConsentRequest {
                        file: conv(),
                        manifest: event_id,
                    },
                    ReadFault::SignedConsent => SyncWire::SignedFileConsentRequest {
                        file: conv(),
                        manifest: event_id,
                    },
                    ReadFault::FileIdentity => SyncWire::FileMessageIdentitiesRequest {
                        requests: vec![(conv(), event_id)],
                        offers: vec![],
                    },
                    ReadFault::AcceptFileIdentity => SyncWire::FileMessageIdentitiesRequest {
                        requests: vec![],
                        offers: vec![Event::new(
                            &alice,
                            conv(),
                            1,
                            vec![],
                            1,
                            0,
                            EventKind::Message,
                            vec![],
                        )],
                    },
                    ReadFault::StickerProof => SyncWire::StickerFileProofRequest {
                        file: conv(),
                        manifest: event_id,
                    },
                    ReadFault::StickerConsent => SyncWire::StickerFileConsentRequest {
                        file: conv(),
                        manifest: event_id,
                        headers: vec![],
                    },
                };
                channel.send(&encode(&wire).unwrap()).await.unwrap();
                if matches!(fault, ReadFault::Ids | ReadFault::Body) {
                    send_have(&mut channel, conv(), &[], SyncWire::ReqHave)
                        .await
                        .unwrap();
                }
                assert!(
                    channel.recv().await.is_err(),
                    "a failed local read must not emit an empty, matched or custody response"
                );
                server.await.unwrap();
            }
        })
        .await
        .expect("loopback store-failure test timed out");
    }

    #[tokio::test]
    async fn rejected_relay_upload_over_loopback_preserves_retention_and_valid_delivery() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            use crate::postoffice::{PostOffice, opaque::RelayMailbox};
            use std::sync::Arc;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.log");
            let sender = DeviceIdentity::generate();
            let recipient = DeviceIdentity::generate();
            let relay = DeviceIdentity::generate();
            let relay_keys = relay.secret_bytes();
            let relay_public = relay.public();
            let mailbox = RelayMailbox::new(&sender, &recipient.public(), &relay_public).unwrap();
            let inner = Event::new(
                &sender,
                conv(),
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                b"private".to_vec(),
            );
            let outer = mailbox.wrap(&inner).unwrap();
            let conversation = outer.conversation_id;
            let mut office = PostOffice::open(&path, "pw", relay).unwrap();
            office.require_opaque_envelopes();
            let store = Arc::new(Mutex::new(office));
            let before = std::fs::read(path.with_extension("retention")).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server_store = Arc::clone(&store);
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let identity = DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1);
                let channel = SecureChannel::accept(stream, &identity).await.unwrap();
                crate::node::postbox::serve_relay_connection(channel, server_store).await;
            });
            let mut channel = SecureChannel::connect(
                tokio::net::TcpStream::connect(addr).await.unwrap(),
                &sender,
                Some(&relay_public),
            )
            .await
            .unwrap();
            let mut bad_signature = outer.clone();
            bad_signature.sig[0] ^= 1;
            let mut bad_id = outer.clone();
            bad_id.ciphertext[12] ^= 1;
            let events = (0..16)
                .map(|n| {
                    if n % 2 == 0 {
                        bad_signature.clone()
                    } else {
                        bad_id.clone()
                    }
                })
                .collect();
            channel
                .send(
                    &encode(&SyncWire::Followup(SyncFollowup {
                        conversation,
                        events,
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
            // This reply is a processing barrier for all preceding uploads.
            assert!(
                !request_durable_have(&mut channel, conversation, outer.id)
                    .await
                    .unwrap()
            );
            assert_eq!(
                std::fs::read(path.with_extension("retention")).unwrap(),
                before
            );

            channel
                .send(
                    &encode(&SyncWire::Followup(SyncFollowup {
                        conversation,
                        events: vec![outer.clone()],
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                request_durable_have(&mut channel, conversation, outer.id)
                    .await
                    .unwrap()
            );
            drop(channel);
            server.await.unwrap();
            drop(store);
            let reopened = PostOffice::open(
                &path,
                "pw",
                DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1),
            )
            .unwrap();
            assert!(reopened.has(&outer.id));
            let recipient_mailbox =
                RelayMailbox::new(&recipient, &sender.public(), &relay_public).unwrap();
            assert_eq!(recipient_mailbox.open(&outer).unwrap(), inner);
        })
        .await
        .expect("loopback relay upload timed out");
    }

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
        assert!(
            !request_durable_have(&mut channel, conv(), id)
                .await
                .unwrap()
        );
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
        assert!(
            request_durable_have(&mut channel, conv(), id)
                .await
                .is_err()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn replica_event_upgrades_before_transfer_and_legacy_peer_closes_safely() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let event = Event::new(
            &alice,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::ChannelReplica,
            vec![1],
        );
        let (a, b) = tokio::io::duplex(65536);
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
            let first = channel.recv().await.unwrap();
            assert!(bincode::deserialize::<LegacyWire>(&first).is_err());
        });
        let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
        let mut log = EventLog::default();
        log.append(event).unwrap();
        assert!(
            request_round(&mut channel, &Mutex::new(log), conv())
                .await
                .is_err()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn legacy_initiator_upgrades_before_receiving_replica_event() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let event = Event::new(
            &bob,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::ChannelReplica,
            vec![1],
        );
        let id = event.id;
        let (a, b) = tokio::io::duplex(65536);
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
            let mut log = EventLog::default();
            log.append(event).unwrap();
            let store = Mutex::new(log);
            while let Ok(Served::Handled(_)) = serve_one(&mut channel, &store).await {}
        });
        let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
        let store = Mutex::new(EventLog::default());
        request_round(&mut channel, &store, conv()).await.unwrap();
        assert!(store.lock().unwrap().has(&id));
        drop(channel);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn old_initiator_cannot_decode_upgrade_notice_or_receive_replica_event() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let event = Event::new(
            &bob,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::ChannelReplica,
            vec![1],
        );
        let (a, b) = tokio::io::duplex(65536);
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
            let mut log = EventLog::default();
            log.append(event).unwrap();
            serve_one(&mut channel, &Mutex::new(log)).await.unwrap();
        });
        let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
        channel
            .send(
                &encode(&SyncWire::FpRequest(FpRequest {
                    conversation: conv(),
                    fingerprint: [0; 32],
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let answer = channel.recv().await.unwrap();
        assert!(
            matches!(decode(&answer), Ok(SyncWire::UpgradeRequired { conversation }) if conversation == conv())
        );
        assert!(bincode::deserialize::<LegacyWire>(&answer).is_err());
        server.await.unwrap();
    }

    #[test]
    fn channel_history_envelopes_preserve_existing_wire_tags() {
        let request = || SyncRequest {
            conversation: conv(),
            have: vec![],
        };
        let fp = || FpRequest {
            conversation: conv(),
            fingerprint: [0; 32],
        };
        let followup = || SyncFollowup {
            conversation: conv(),
            events: vec![],
        };
        for (wire, tag) in [
            (SyncWire::FpRequestV2(fp()), 11_u32),
            (SyncWire::RequestV2(request()), 12),
            (SyncWire::FollowupV2(followup()), 13),
            (
                SyncWire::UpgradeRequired {
                    conversation: conv(),
                },
                14,
            ),
            (
                SyncWire::SignedFileConsentRequest {
                    file: conv(),
                    manifest: EventId::new([0; 32]),
                },
                15,
            ),
            (
                SyncWire::SignedFileConsentResponse {
                    file: conv(),
                    manifest: EventId::new([0; 32]),
                    control: None,
                },
                16,
            ),
            (SyncWire::FpRequestV3(fp()), 17),
            (SyncWire::RequestV3(request()), 18),
            (SyncWire::FollowupV3(followup()), 19),
            (SyncWire::FpRequestV4(fp()), 20),
            (SyncWire::RequestV4(request()), 21),
            (SyncWire::FollowupV4(followup()), 22),
        ] {
            let encoded = encode(&wire).unwrap();
            assert_eq!(&encoded[..4], &tag.to_le_bytes());
            assert!(decode(&encoded).is_ok());
        }
    }

    #[tokio::test]
    async fn account_authority_requires_v4_for_fingerprint_and_event_transfer() {
        for kind_index in [17_u32, 18] {
            let kind: EventKind = bincode::deserialize(&kind_index.to_le_bytes()).unwrap();
            let alice = DeviceIdentity::generate();
            let bob = DeviceIdentity::generate();
            let event = Event::new(&bob, conv(), 1, vec![], 1, 0, kind, vec![1]);
            let (a, b) = tokio::io::duplex(65536);
            let server = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                let mut log = EventLog::default();
                log.append(event).unwrap();
                let store = Mutex::new(log);
                for _ in 0..6 {
                    serve_one(&mut channel, &store).await.unwrap();
                }
            });
            let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
            for version in [1, 2, 3] {
                let probe = FpRequest {
                    conversation: conv(),
                    fingerprint: [0; 32],
                };
                let wire = if version == 1 {
                    SyncWire::FpRequest(probe)
                } else if version == 2 {
                    SyncWire::FpRequestV2(probe)
                } else {
                    SyncWire::FpRequestV3(probe)
                };
                channel.send(&encode(&wire).unwrap()).await.unwrap();
                assert!(
                    matches!(decode(&channel.recv().await.unwrap()).unwrap(), SyncWire::UpgradeRequired { conversation } if conversation == conv())
                );
                let request = SyncRequest {
                    conversation: conv(),
                    have: vec![],
                };
                let wire = if version == 1 {
                    SyncWire::Request(request)
                } else if version == 2 {
                    SyncWire::RequestV2(request)
                } else {
                    SyncWire::RequestV3(request)
                };
                channel.send(&encode(&wire).unwrap()).await.unwrap();
                send_have(&mut channel, conv(), &[], SyncWire::ReqHave)
                    .await
                    .unwrap();
                assert!(
                    matches!(decode(&channel.recv().await.unwrap()).unwrap(), SyncWire::UpgradeRequired { conversation } if conversation == conv())
                );
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn channel_history_requires_v3_for_fingerprint_and_event_transfer() {
        for kind_index in [15_u32, 16] {
            let kind: EventKind = bincode::deserialize(&kind_index.to_le_bytes()).unwrap();
            let alice = DeviceIdentity::generate();
            let bob = DeviceIdentity::generate();
            let event = Event::new(&bob, conv(), 1, vec![], 1, 0, kind, vec![1]);
            let (a, b) = tokio::io::duplex(65536);
            let server = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                let mut log = EventLog::default();
                log.append(event).unwrap();
                let store = Mutex::new(log);
                for _ in 0..4 {
                    serve_one(&mut channel, &store).await.unwrap();
                }
            });
            let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
            for version in [1, 2] {
                let probe = FpRequest {
                    conversation: conv(),
                    fingerprint: [0; 32],
                };
                let wire = if version == 1 {
                    SyncWire::FpRequest(probe)
                } else {
                    SyncWire::FpRequestV2(probe)
                };
                channel.send(&encode(&wire).unwrap()).await.unwrap();
                assert!(
                    matches!(decode(&channel.recv().await.unwrap()).unwrap(), SyncWire::UpgradeRequired { conversation } if conversation == conv())
                );
                let request = SyncRequest {
                    conversation: conv(),
                    have: vec![],
                };
                let wire = if version == 1 {
                    SyncWire::Request(request)
                } else {
                    SyncWire::RequestV2(request)
                };
                channel.send(&encode(&wire).unwrap()).await.unwrap();
                send_have(&mut channel, conv(), &[], SyncWire::ReqHave)
                    .await
                    .unwrap();
                assert!(
                    matches!(decode(&channel.recv().await.unwrap()).unwrap(), SyncWire::UpgradeRequired { conversation } if conversation == conv())
                );
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn account_authority_rejects_events_in_older_followup_envelopes() {
        for kind in [
            EventKind::ChannelAccountAuthority,
            EventKind::ChannelDeviceInheritance,
        ] {
            for version in [1, 2, 3] {
                let alice = DeviceIdentity::generate();
                let bob = DeviceIdentity::generate();
                let event = Event::new(&alice, conv(), 1, vec![], 1, 0, kind, vec![1]);
                let (a, b) = tokio::io::duplex(65536);
                let server = tokio::spawn(async move {
                    let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                    let store = Mutex::new(EventLog::default());
                    assert!(matches!(
                        serve_one(&mut channel, &store).await,
                        Err(SessionError::UnexpectedMessage)
                    ));
                    assert!(store.lock().unwrap().events(&conv()).is_empty());
                });
                let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
                let followup = SyncFollowup {
                    conversation: conv(),
                    events: vec![event],
                };
                let wire = if version == 1 {
                    SyncWire::Followup(followup)
                } else if version == 2 {
                    SyncWire::FollowupV2(followup)
                } else {
                    SyncWire::FollowupV3(followup)
                };
                channel.send(&encode(&wire).unwrap()).await.unwrap();
                server.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn channel_history_rejects_events_in_older_followup_envelopes() {
        for kind in [
            EventKind::ChannelHistoryPolicy,
            EventKind::ChannelJoinHistory,
        ] {
            for version in [1, 2] {
                let alice = DeviceIdentity::generate();
                let bob = DeviceIdentity::generate();
                let event = Event::new(&alice, conv(), 1, vec![], 1, 0, kind, vec![1]);
                let (a, b) = tokio::io::duplex(65536);
                let server = tokio::spawn(async move {
                    let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                    let store = Mutex::new(EventLog::default());
                    assert!(matches!(
                        serve_one(&mut channel, &store).await,
                        Err(SessionError::UnexpectedMessage)
                    ));
                    assert!(store.lock().unwrap().events(&conv()).is_empty());
                });
                let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
                let followup = SyncFollowup {
                    conversation: conv(),
                    events: vec![event],
                };
                let wire = if version == 1 {
                    SyncWire::Followup(followup)
                } else {
                    SyncWire::FollowupV2(followup)
                };
                channel.send(&encode(&wire).unwrap()).await.unwrap();
                server.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn account_authority_arriving_after_probe_restarts_without_legacy_followup_leak() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let alice = DeviceIdentity::generate();
            let bob = DeviceIdentity::generate();
            let prior = Event::new(&bob, conv(), 1, vec![], 1, 0, EventKind::Message, vec![1]);
            let added = Event::new(
                &alice,
                conv(),
                1,
                vec![],
                1,
                0,
                EventKind::ChannelAccountAuthority,
                vec![1],
            );
            let added_id = added.id;
            let mut remote = EventLog::default();
            remote.append(prior).unwrap();
            let remote = std::sync::Arc::new(Mutex::new(remote));
            let serving = remote.clone();
            let (a, b) = tokio::io::duplex(65536);
            let server = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                while let Ok(Served::Handled(_)) = serve_one(&mut channel, &serving).await {}
            });
            let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
            let local = Mutex::new(EventLog::default());
            let mut progress = SyncProgress::default();
            assert!(!progress.step(&mut channel, &local, conv()).await.unwrap());
            assert!(!progress.v4);
            local.lock().unwrap().append(added).unwrap();
            assert!(!progress.step(&mut channel, &local, conv()).await.unwrap());
            assert!(progress.v4);
            assert!(
                !remote.lock().unwrap().has(&added_id),
                "the incomplete legacy round must carry no new-version event"
            );
            let mut complete = false;
            for _ in 0..8 {
                if progress.step(&mut channel, &local, conv()).await.unwrap() {
                    complete = true;
                    break;
                }
            }
            assert!(complete);
            assert!(remote.lock().unwrap().has(&added_id));
            drop(channel);
            server.await.unwrap();
        })
        .await
        .expect("late authority negotiation must converge within a bounded exchange");
    }

    #[tokio::test]
    async fn account_authority_negotiates_v4_and_transfers_in_both_directions() {
        for kind_index in [17_u32, 18] {
            for responder_owns_event in [false, true] {
                let kind: EventKind = bincode::deserialize(&kind_index.to_le_bytes()).unwrap();
                let alice = DeviceIdentity::generate();
                let bob = DeviceIdentity::generate();
                let event = Event::new(&alice, conv(), 1, vec![], 1, 0, kind, vec![1]);
                let id = event.id;
                let mut source = EventLog::default();
                source.append(event).unwrap();
                let (initiator_log, responder_log) = if responder_owns_event {
                    (EventLog::default(), source)
                } else {
                    (source, EventLog::default())
                };
                let (a, b) = tokio::io::duplex(65536);
                let responder_log = std::sync::Arc::new(Mutex::new(responder_log));
                let server_store = responder_log.clone();
                let server = tokio::spawn(async move {
                    let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                    while let Ok(Served::Handled(_)) = serve_one(&mut channel, &server_store).await
                    {
                    }
                });
                let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
                let store = Mutex::new(initiator_log);
                request_round(&mut channel, &store, conv()).await.unwrap();
                assert!(store.lock().unwrap().has(&id));
                assert!(responder_log.lock().unwrap().has(&id));
                drop(channel);
                server.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn channel_history_negotiates_v3_and_transfers_in_both_directions() {
        for kind_index in [15_u32, 16] {
            for responder_owns_event in [false, true] {
                let kind: EventKind = bincode::deserialize(&kind_index.to_le_bytes()).unwrap();
                let alice = DeviceIdentity::generate();
                let bob = DeviceIdentity::generate();
                let event = Event::new(&alice, conv(), 1, vec![], 1, 0, kind, vec![1]);
                let id = event.id;
                let mut source = EventLog::default();
                source.append(event).unwrap();
                let (initiator_log, responder_log) = if responder_owns_event {
                    (EventLog::default(), source)
                } else {
                    (source, EventLog::default())
                };
                let (a, b) = tokio::io::duplex(65536);
                let responder_log = std::sync::Arc::new(Mutex::new(responder_log));
                let server_store = responder_log.clone();
                let server = tokio::spawn(async move {
                    let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
                    while let Ok(Served::Handled(_)) = serve_one(&mut channel, &server_store).await
                    {
                    }
                });
                let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
                let store = Mutex::new(initiator_log);
                request_round(&mut channel, &store, conv()).await.unwrap();
                assert!(store.lock().unwrap().has(&id));
                assert!(responder_log.lock().unwrap().has(&id));
                drop(channel);
                server.await.unwrap();
            }
        }
    }

    /// Repeatable protocol-scale rehearsal. Run with `--ignored --nocapture` to
    /// inspect the sender contact count and convergence time at each size.
    /// The scheduler and real LAN discovery have separate node tests.
    #[tokio::test]
    #[ignore]
    async fn channel_replica_scale_rehearsal() {
        async fn copy(
            source: std::sync::Arc<Mutex<EventLog>>,
            target: std::sync::Arc<Mutex<EventLog>>,
            identity: &DeviceIdentity,
        ) {
            let responder = DeviceIdentity::generate();
            let (a, b) = tokio::io::duplex(65536);
            let task = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(b, &responder).await.unwrap();
                while let Ok(Served::Handled(_)) = serve_one(&mut channel, &source).await {}
            });
            let mut channel = SecureChannel::connect(a, identity, None).await.unwrap();
            request_round(&mut channel, &target, conv()).await.unwrap();
            drop(channel);
            task.await.unwrap();
        }
        let identity = DeviceIdentity::generate();
        let event = Event::new(
            &identity,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![7; 1024],
        );
        for size in [5usize, 20, 50] {
            let started = std::time::Instant::now();
            let stores: Vec<_> = (0..size)
                .map(|_| std::sync::Arc::new(Mutex::new(EventLog::default())))
                .collect();
            stores[0].lock().unwrap().append(event.clone()).unwrap();
            let online: Vec<_> = (1..size).filter(|i| i % 5 != 4).collect();
            let initial = if size <= 5 { online.len() } else { 2 };
            for &index in online.iter().take(initial) {
                copy(stores[0].clone(), stores[index].clone(), &identity).await;
            }
            for (offset, &index) in online.iter().enumerate().skip(initial) {
                let source = online[offset - 1];
                copy(stores[source].clone(), stores[index].clone(), &identity).await;
            }
            // Origin exits. Late members recover from an ordinary replica.
            for index in (1..size).filter(|i| i % 5 == 4) {
                copy(stores[online[0]].clone(), stores[index].clone(), &identity).await;
            }
            assert!(
                stores
                    .iter()
                    .all(|store| store.lock().unwrap().has(&event.id))
            );
            eprintln!(
                "channel-scale members={size} sender_contacts={initial} sender_event_payload_bytes_estimate={} convergence_ms={}",
                initial * bincode::serialized_size(&event).unwrap() as usize,
                started.elapsed().as_millis()
            );
        }
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
        let ids = log.event_ids(&conv()).unwrap();
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
        let mut progress = SyncProgress::default();
        let mut saw_partial_recipient_have = false;
        let mut complete = false;
        for _ in 0..100 {
            complete = progress.step(&mut channel, &store, conv()).await.unwrap();
            if progress
                .remote_have_count()
                .is_some_and(|count| count > 0 && count < ids.len())
            {
                saw_partial_recipient_have = true;
            }
            if complete {
                break;
            }
        }
        assert!(complete, "bounded rounds must finish the transfer");
        assert!(
            saw_partial_recipient_have,
            "recipient Have exposes intermediate progress"
        );
        assert_eq!(progress.remote_have_count(), Some(ids.len()));
        drop(channel);
        let (remote, handled) = server.await.unwrap();
        assert_eq!(
            progress.total.applied, 0,
            "progress comes entirely from remote acceptance"
        );
        assert!(
            handled > 9,
            "transfer requires more rounds than the stall diagnostic cutoff"
        );
        assert_eq!(remote.event_ids(&conv()).unwrap(), ids);
        for event in store.lock().unwrap().events(&conv()) {
            assert_eq!(remote.get(&event.id), Some(event));
        }
    }

    #[tokio::test]
    async fn file_burst_reuses_one_have_exchange_for_several_legacy_followups() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let (aio, bio) = tokio::io::duplex(512 * 1024);
        let log = build_large_responder_store(&alice, conv(), 10, 20 * 1024);
        let ids = log.event_ids(&conv()).unwrap();
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(bio, &bob).await.unwrap();
            let store = Mutex::new(EventLog::default());
            while let Ok(Served::Handled(_)) = serve_one(&mut channel, &store).await {}
            store.into_inner().unwrap()
        });
        let mut channel = SecureChannel::connect(aio, &alice, None).await.unwrap();
        let store = Mutex::new(log);
        let mut progress = SyncProgress::default();
        for _ in 0..10 {
            if progress
                .step_burst(&mut channel, &store, conv(), FILE_FOLLOWUP_BURST)
                .await
                .unwrap()
            {
                break;
            }
        }
        assert_eq!(progress.remote_have_count(), Some(ids.len()));
        assert!(
            progress.rounds <= 3,
            "burst must avoid per-chunk reconciliation"
        );
        drop(channel);
        assert_eq!(server.await.unwrap().event_ids(&conv()).unwrap(), ids);
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
        let responder_event_count = b_log.event_ids(&conv_id).unwrap().len();
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
        let requester_count = a_store.lock().unwrap().event_ids(&conv_id).unwrap().len();
        assert_eq!(
            requester_count, responder_event_count,
            "requester should have all {responder_event_count} events after multi-round sync"
        );
        // Responder also still has all events.
        assert_eq!(
            b_final.event_ids(&conv_id).unwrap().len(),
            responder_event_count
        );
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
        assert_eq!(b_log.event_ids(&conv_id).unwrap().len(), NUM_EVENTS);

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
            a_store.lock().unwrap().event_ids(&conv_id).unwrap().len(),
            NUM_EVENTS,
            "requester pulled all events despite a >1-frame have id-set",
        );
    }

    #[tokio::test]
    async fn optional_file_message_identity_exchange_keeps_metadata_out_of_history() {
        struct Identities {
            log: EventLog,
            bundle: Event,
            manifest: EventId,
            imported: Vec<Event>,
        }
        impl SyncStore for Identities {
            fn event_ids(
                &self,
                conversation: &ConversationId,
            ) -> Result<Vec<EventId>, crate::eventlog::LogError> {
                self.log.event_ids(conversation)
            }
            fn events_excluding(
                &self,
                conversation: &ConversationId,
                have: &std::collections::HashSet<EventId>,
            ) -> Result<Vec<Event>, crate::eventlog::LogError> {
                self.log.events_excluding(conversation, have)
            }
            fn ingest(
                &mut self,
                event: Event,
            ) -> Result<crate::eventlog::AppendOutcome, crate::eventlog::LogError> {
                self.log.append(event)
            }
            fn file_message_identity(
                &self,
                file: &ConversationId,
                manifest: &EventId,
            ) -> Result<Option<Event>, crate::eventlog::LogError> {
                Ok((*file == conv() && *manifest == self.manifest).then(|| self.bundle.clone()))
            }
            fn accept_file_message_identity(
                &mut self,
                bundle: Event,
            ) -> Result<bool, crate::eventlog::LogError> {
                self.imported.push(bundle);
                Ok(true)
            }
        }
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let manifest = EventId::new([97; 32]);
        let bundle = Event::new(
            &bob,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"identity".to_vec(),
        );
        let offered = Event::new(
            &alice,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"offered identity".to_vec(),
        );
        let offered_id = offered.id;
        let expected_bundle = bundle.clone();
        let server_message = Event::new(
            &bob,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"ordinary server message".to_vec(),
        );
        let server_message_id = server_message.id;
        let client_message = Event::new(
            &alice,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"ordinary client message".to_vec(),
        );
        let client_message_id = client_message.id;
        let (a, b) = tokio::io::duplex(65536);
        let server = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &bob).await.unwrap();
            let store = Mutex::new(Identities {
                log: EventLog::default(),
                bundle,
                manifest,
                imported: vec![],
            });
            serve_one(&mut channel, &store).await.unwrap();
            {
                let mut snapshot = store.lock().unwrap();
                assert_eq!(snapshot.imported[0].id, offered_id);
                assert!(snapshot.log.event_ids(&conv()).unwrap().is_empty());
                snapshot.log.append(server_message).unwrap();
            }
            loop {
                if matches!(
                    serve_one(&mut channel, &store).await.unwrap(),
                    Served::Closed
                ) {
                    break;
                }
            }
            let snapshot = store.lock().unwrap();
            assert_eq!(snapshot.log.event_ids(&conv()).unwrap().len(), 2);
            assert!(snapshot.log.get(&client_message_id).is_some());
            assert!(snapshot.log.get(&offered_id).is_none());
        });
        let mut channel = SecureChannel::connect(a, &alice, None).await.unwrap();
        let bundles = request_file_message_identities(
            &mut channel,
            vec![(conv(), manifest), (conv(), EventId::new([98; 32]))],
            vec![offered],
        )
        .await
        .unwrap();
        assert_eq!(bundles, vec![expected_bundle]);
        let mut ordinary = EventLog::default();
        ordinary.append(client_message).unwrap();
        let ordinary = Mutex::new(ordinary);
        request_round(&mut channel, &ordinary, conv())
            .await
            .unwrap();
        assert_eq!(
            ordinary.lock().unwrap().event_ids(&conv()).unwrap().len(),
            2
        );
        assert!(ordinary.lock().unwrap().get(&server_message_id).is_some());
        drop(channel);
        server.await.unwrap();
    }

    async fn attachment_test_channels() -> (
        SecureChannel<tokio::io::DuplexStream>,
        SecureChannel<tokio::io::DuplexStream>,
    ) {
        let requester = DeviceIdentity::generate();
        let responder = DeviceIdentity::generate();
        let (client_io, server_io) = tokio::io::duplex(65536);
        let (client, server) = tokio::join!(
            SecureChannel::connect(client_io, &requester, None),
            SecureChannel::accept(server_io, &responder)
        );
        (client.unwrap(), server.unwrap())
    }

    #[tokio::test]
    async fn legacy_store_attachment_extensions_fail_closed_and_keep_ordinary_sync_usable() {
        let source = DeviceIdentity::generate();
        let ordinary = Event::new(
            &source,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"retained ordinary message".to_vec(),
        );
        let ordinary_id = ordinary.id;
        let offered = Event::new(
            &DeviceIdentity::generate(),
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"optional unrecognized metadata".to_vec(),
        );
        let offered_id = offered.id;
        let (mut client, mut server) = attachment_test_channels().await;
        let server_task = tokio::spawn(async move {
            let mut log = EventLog::default();
            log.append(ordinary).unwrap();
            let store = Mutex::new(log);
            loop {
                if matches!(
                    serve_one(&mut server, &store).await.unwrap(),
                    Served::Closed
                ) {
                    break;
                }
            }
            let snapshot = store.lock().unwrap();
            assert_eq!(snapshot.event_ids(&conv()).unwrap(), vec![ordinary_id]);
            assert!(
                snapshot.get(&offered_id).is_none(),
                "unsupported control metadata is never an ordinary history event"
            );
        });
        let manifest = EventId::new([52; 32]);
        assert_eq!(
            request_file_consent(&mut client, conv(), manifest)
                .await
                .unwrap(),
            FileConsentStatus::Pending
        );
        assert!(
            request_signed_file_consent(&mut client, conv(), manifest)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            request_sticker_file_consent(&mut client, conv(), manifest, vec![])
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            request_sticker_file_proof(&mut client, conv(), manifest)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            request_file_message_identities(&mut client, vec![(conv(), manifest)], vec![offered])
                .await
                .unwrap()
                .is_empty()
        );
        let destination = Mutex::new(EventLog::default());
        request_round(&mut client, &destination, conv())
            .await
            .unwrap();
        assert_eq!(
            destination.lock().unwrap().event_ids(&conv()).unwrap(),
            vec![ordinary_id]
        );
        drop(client);
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn optional_attachment_replies_must_match_exact_requested_file_and_manifest() {
        for protocol in 0..3 {
            for wrong_file in [false, true] {
                let (mut client, mut server) = attachment_test_channels().await;
                let manifest = EventId::new([61; 32]);
                let response_file = if wrong_file {
                    ConversationId::new([62; 32])
                } else {
                    conv()
                };
                let response_manifest = if wrong_file {
                    manifest
                } else {
                    EventId::new([63; 32])
                };
                let response = match protocol {
                    0 => SyncWire::SignedFileConsentResponse {
                        file: response_file,
                        manifest: response_manifest,
                        control: None,
                    },
                    1 => SyncWire::StickerFileConsentResponse {
                        file: response_file,
                        manifest: response_manifest,
                        control: None,
                    },
                    _ => SyncWire::StickerFileProofResponse {
                        file: response_file,
                        manifest: response_manifest,
                        headers: None,
                    },
                };
                let responder = tokio::spawn(async move {
                    let request = decode(&server.recv().await.unwrap()).unwrap();
                    assert!(matches!(
                        request,
                        SyncWire::SignedFileConsentRequest { .. }
                            | SyncWire::StickerFileConsentRequest { .. }
                            | SyncWire::StickerFileProofRequest { .. }
                    ));
                    server.send(&encode(&response).unwrap()).await.unwrap();
                });
                let rejected = match protocol {
                    0 => matches!(
                        request_signed_file_consent(&mut client, conv(), manifest).await,
                        Err(SessionError::UnexpectedMessage)
                    ),
                    1 => matches!(
                        request_sticker_file_consent(&mut client, conv(), manifest, vec![]).await,
                        Err(SessionError::UnexpectedMessage)
                    ),
                    _ => matches!(
                        request_sticker_file_proof(&mut client, conv(), manifest).await,
                        Err(SessionError::UnexpectedMessage)
                    ),
                };
                assert!(
                    rejected,
                    "optional replies cannot authorize another request's attachment"
                );
                responder.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn identity_client_enforces_request_offer_and_response_budgets() {
        let signer = DeviceIdentity::generate();
        let control = Event::new(
            &signer,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"identity control".to_vec(),
        );
        let manifest = EventId::new([64; 32]);
        let (mut client, mut server) = attachment_test_channels().await;
        assert!(matches!(
            request_file_message_identities(
                &mut client,
                vec![(conv(), manifest); MAX_FILE_IDENTITIES + 1],
                vec![]
            )
            .await,
            Err(SessionError::UnexpectedMessage)
        ));
        client.send(b"after rejected request budget").await.unwrap();
        assert_eq!(
            server.recv().await.unwrap(),
            b"after rejected request budget",
            "over-budget requests must not reach the wire"
        );
        assert!(matches!(
            request_file_message_identities(
                &mut client,
                vec![],
                vec![control.clone(); MAX_FILE_IDENTITIES + 1]
            )
            .await,
            Err(SessionError::UnexpectedMessage)
        ));
        client.send(b"after rejected offer budget").await.unwrap();
        assert_eq!(server.recv().await.unwrap(), b"after rejected offer budget");
        let responder = tokio::spawn(async move {
            assert!(matches!(
                decode(&server.recv().await.unwrap()).unwrap(),
                SyncWire::FileMessageIdentitiesRequest { .. }
            ));
            server
                .send(
                    &encode(&SyncWire::FileMessageIdentitiesResponse {
                        bundles: vec![control; MAX_FILE_IDENTITIES + 1],
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        });
        assert!(
            matches!(
                request_file_message_identities(&mut client, vec![(conv(), manifest)], vec![])
                    .await,
                Err(SessionError::UnexpectedMessage)
            ),
            "over-budget replies cannot enter the identity registry"
        );
        responder.await.unwrap();
    }

    struct AttachmentAdmissionProbe {
        log: EventLog,
        control: Event,
        proof_calls: usize,
        imported: usize,
        identity_requests: std::sync::atomic::AtomicUsize,
    }
    impl SyncStore for AttachmentAdmissionProbe {
        fn event_ids(
            &self,
            conversation: &ConversationId,
        ) -> Result<Vec<EventId>, crate::eventlog::LogError> {
            self.log.event_ids(conversation)
        }
        fn events_excluding(
            &self,
            conversation: &ConversationId,
            have: &std::collections::HashSet<EventId>,
        ) -> Result<Vec<Event>, crate::eventlog::LogError> {
            self.log.events_excluding(conversation, have)
        }
        fn ingest(
            &mut self,
            event: Event,
        ) -> Result<crate::eventlog::AppendOutcome, crate::eventlog::LogError> {
            self.log.append(event)
        }
        fn file_message_identity(
            &self,
            _file: &ConversationId,
            _manifest: &EventId,
        ) -> Result<Option<Event>, crate::eventlog::LogError> {
            self.identity_requests
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Some(self.control.clone()))
        }
        fn accept_file_message_identity(
            &mut self,
            _bundle: Event,
        ) -> Result<bool, crate::eventlog::LogError> {
            self.imported += 1;
            Ok(true)
        }
        fn sticker_file_consent(
            &mut self,
            _file: &ConversationId,
            _manifest: &EventId,
            _headers: Vec<Event>,
        ) -> Result<Option<Event>, crate::eventlog::LogError> {
            self.proof_calls += 1;
            Ok(Some(self.control.clone()))
        }
    }

    #[tokio::test]
    async fn attachment_wire_bounds_reject_before_callbacks_and_bounded_metadata_stays_out_of_history()
     {
        use std::sync::atomic::Ordering;
        let signer = DeviceIdentity::generate();
        let control = Event::new(
            &signer,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"control".to_vec(),
        );
        let manifest = EventId::new([65; 32]);
        let store = Mutex::new(AttachmentAdmissionProbe {
            log: EventLog::default(),
            control: control.clone(),
            proof_calls: 0,
            imported: 0,
            identity_requests: std::sync::atomic::AtomicUsize::new(0),
        });
        for request in [
            SyncWire::FileMessageIdentitiesRequest {
                requests: vec![(conv(), manifest); MAX_FILE_IDENTITIES + 1],
                offers: vec![],
            },
            SyncWire::FileMessageIdentitiesRequest {
                requests: vec![(conv(), manifest)],
                offers: vec![control.clone(); MAX_FILE_IDENTITIES + 1],
            },
        ] {
            let (mut client, mut server) = attachment_test_channels().await;
            client.send(&encode(&request).unwrap()).await.unwrap();
            let wire = server.recv().await.unwrap();
            assert!(matches!(
                serve_wire_bytes(&mut server, &store, &wire).await,
                Err(SessionError::UnexpectedMessage)
            ));
            let snapshot = store.lock().unwrap();
            assert_eq!(snapshot.imported, 0);
            assert_eq!(snapshot.identity_requests.load(Ordering::Relaxed), 0);
        }
        let mut malformed = Vec::new();
        let mut header = control.clone();
        header.ciphertext.clear();
        malformed.push(vec![header.clone(); 65]);
        let mut payload = header.clone();
        payload.ciphertext.push(1);
        malformed.push(vec![payload]);
        let mut ancestry = header.clone();
        ancestry.parents = vec![EventId::new([66; 32]), EventId::new([67; 32])];
        malformed.push(vec![ancestry]);
        let mut signature = header.clone();
        signature.sig.pop();
        malformed.push(vec![signature]);
        for headers in malformed {
            let (mut client, mut server) = attachment_test_channels().await;
            client
                .send(
                    &encode(&SyncWire::StickerFileConsentRequest {
                        file: conv(),
                        manifest,
                        headers,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            let wire = server.recv().await.unwrap();
            serve_wire_bytes(&mut server, &store, &wire).await.unwrap();
            assert!(matches!(
                decode(&client.recv().await.unwrap()).unwrap(),
                SyncWire::StickerFileConsentResponse { control: None, .. }
            ));
            assert_eq!(
                store.lock().unwrap().proof_calls,
                0,
                "invalid header envelopes must not invoke storage admission"
            );
        }
        let (mut client, mut server) = attachment_test_channels().await;
        client
            .send(
                &encode(&SyncWire::StickerFileConsentRequest {
                    file: conv(),
                    manifest,
                    headers: vec![header],
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let wire = server.recv().await.unwrap();
        serve_wire_bytes(&mut server, &store, &wire).await.unwrap();
        assert!(matches!(
            decode(&client.recv().await.unwrap()).unwrap(),
            SyncWire::StickerFileConsentResponse {
                control: Some(_),
                ..
            }
        ));
        assert_eq!(store.lock().unwrap().proof_calls, 1);
        let requests = vec![(conv(), manifest); MAX_FILE_IDENTITIES];
        client
            .send(
                &encode(&SyncWire::FileMessageIdentitiesRequest {
                    requests,
                    offers: vec![control.clone(); MAX_FILE_IDENTITIES],
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let wire = server.recv().await.unwrap();
        serve_wire_bytes(&mut server, &store, &wire).await.unwrap();
        let SyncWire::FileMessageIdentitiesResponse { bundles } =
            decode(&client.recv().await.unwrap()).unwrap()
        else {
            panic!("expected bounded metadata response")
        };
        assert_eq!(
            bundles,
            vec![control.clone()],
            "identical returned controls are deduplicated within the request budget"
        );
        {
            let snapshot = store.lock().unwrap();
            assert_eq!(snapshot.imported, MAX_FILE_IDENTITIES);
            assert_eq!(
                snapshot.identity_requests.load(Ordering::Relaxed),
                MAX_FILE_IDENTITIES
            );
            assert!(snapshot.log.event_ids(&conv()).unwrap().is_empty());
        }
        for response in [
            SyncWire::StickerFileConsentResponse {
                file: conv(),
                manifest,
                control: None,
            },
            SyncWire::StickerFileProofResponse {
                file: conv(),
                manifest,
                headers: None,
            },
            SyncWire::FileMessageIdentitiesResponse { bundles: vec![] },
        ] {
            assert!(
                matches!(
                    serve_wire_bytes(&mut server, &store, &encode(&response).unwrap()).await,
                    Err(SessionError::UnexpectedMessage)
                ),
                "unsolicited metadata responses cannot mutate the responder"
            );
        }
        let normal_server = Event::new(
            &signer,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"normal server event".to_vec(),
        );
        let normal_server_id = normal_server.id;
        store.lock().unwrap().log.append(normal_server).unwrap();
        let normal_client = Event::new(
            &DeviceIdentity::generate(),
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"normal client event".to_vec(),
        );
        let normal_client_id = normal_client.id;
        let mut client_log = EventLog::default();
        client_log.append(normal_client).unwrap();
        let client_log = Mutex::new(client_log);
        let responder = tokio::spawn(async move {
            loop {
                if matches!(
                    serve_one(&mut server, &store).await.unwrap(),
                    Served::Closed
                ) {
                    break;
                }
            }
            let snapshot = store.lock().unwrap();
            assert_eq!(snapshot.log.event_ids(&conv()).unwrap().len(), 2);
            assert!(snapshot.log.get(&normal_client_id).is_some());
            assert!(snapshot.log.get(&control.id).is_none());
        });
        request_round(&mut client, &client_log, conv())
            .await
            .unwrap();
        assert!(client_log.lock().unwrap().get(&normal_server_id).is_some());
        drop(client);
        responder.await.unwrap();
    }
}

#[cfg(test)]
mod paged_tests;

/// Direct mandatory paged requester. On cancellation/error discard the channel;
/// a partially streamed exchange cannot be reused.
#[allow(dead_code)] // Direct callers land in the separate SQL runtime cutover.
pub async fn request_round_paged<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<ApplyReport, SessionError> {
    let mut progress = SyncProgress::default();
    while !progress
        .step_backend(channel, &PagedNetworkBackend(store, None), conversation, 1)
        .await?
    {}
    Ok(progress.total)
}
pub(in crate::node) async fn request_file_round_paged<
    S: NetworkSyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<ApplyReport, SessionError> {
    let mut progress = SyncProgress::default();
    while !progress
        .step_burst_paged(channel, store, conversation, FILE_FOLLOWUP_BURST)
        .await?
    {}
    Ok(progress.total)
}
/// Serve one frame using the direct bounded store contract.
#[allow(dead_code)] // Direct callers land in the separate SQL runtime cutover.
pub async fn serve_one_paged<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
) -> Result<Served, SessionError> {
    let bytes = match channel.recv().await {
        Ok(bytes) => bytes,
        Err(_) => return Ok(Served::Closed),
    };
    serve_wire_bytes_paged(channel, store, &bytes).await
}
/// Serve an already-read frame through the direct paged backend.
#[allow(dead_code)] // Direct callers land in the separate SQL runtime cutover.
pub async fn serve_wire_bytes_paged<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    bytes: &[u8],
) -> Result<Served, SessionError> {
    serve_wire_backend(channel, &PagedNetworkBackend(store, None), bytes).await
}

/// Dedicated runtime requester; the process owner's shared Have budget is mandatory.
pub(crate) async fn request_round_budgeted<
    S: NetworkSyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
    budget: &RemoteHaveBudget,
) -> Result<ApplyReport, SessionError> {
    let mut progress = SyncProgress::default();
    while !progress
        .step_backend(
            channel,
            &PagedNetworkBackend(store, Some(budget)),
            conversation,
            1,
        )
        .await?
    {}
    Ok(progress.total)
}
/// Dedicated runtime responder; every path retains the process owner RAM gate.
pub(crate) async fn serve_wire_bytes_budgeted<
    S: NetworkSyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    bytes: &[u8],
    budget: &RemoteHaveBudget,
) -> Result<Served, SessionError> {
    serve_wire_backend(channel, &PagedNetworkBackend(store, Some(budget)), bytes).await
}
pub(crate) async fn serve_one_budgeted<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    budget: &RemoteHaveBudget,
) -> Result<Served, SessionError> {
    let bytes = match channel.recv().await {
        Ok(bytes) => bytes,
        Err(_) => return Ok(Served::Closed),
    };
    serve_wire_bytes_budgeted(channel, store, &bytes, budget).await
}
