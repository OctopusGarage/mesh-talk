//! The node App context: owns the device identity, an in-memory event log, and a
//! shared roster; wires DM crypto + the event log + the networked sync driver
//! into `send_dm` (outbound) and a per-connection serve loop (inbound) that
//! surfaces received DMs on an mpsc channel. No global singletons. Starting
//! discovery and binding the TCP listener is the binary's job (next plan); this
//! module provides the pieces and proves them with an in-process two-node
//! exchange over loopback TCP.

use crate::channel::sender_key::SenderKey;
use crate::discovery::roster::{Roster, UserId};
use crate::eventlog::LogError;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::persist::PersistentEventLog;
use crate::identity::account::Account;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::channel::{ChannelBook, ReceivedChannelMessage};
use crate::node::channel_custody::ChannelCustodyStore;
use crate::node::channel_senders::ChannelSenderStore;
use crate::node::dm_ratchet::DmRatchet;
use crate::node::filebook::{FileBook, ReceivedFile};
use crate::node::pairing::PendingLink;
use crate::node::ratchet_sessions::RatchetSessions;
use crate::node::received_log::ReceivedLog;
use crate::node::sentlog::SentLog;
use crate::node::session::SessionError;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Maximum raw file size for `send_file_*`. The `have` id-set is now STREAMED across
/// multiple `ReqHave`/`RespHave` frames (see `node::session`), so the old one-frame
/// id-set limit is gone; the real constraints are now (a) bounded memory — both sides
/// stream chunk-by-chunk to/from disk (see `stage_file` / `assemble_file`), and (b)
/// the relay's `MAX_HAVE_IDS_TOTAL` (200 000 ids). At 48 KiB/chunk that ceiling is
/// ~9 GB; we cap at a conservative 4 GiB and reject only absurd sizes up front.
pub(in crate::node) const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// A peer's avatar, propagated to us via a signed profile and surfaced to the app so the
/// UI can render it keyed by the sender's ACCOUNT id. `avatar` is `None` when the peer
/// CLEARED their avatar (the UI reverts that account to its glyph).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedProfile {
    pub account_id: String,
    pub avatar: Option<Vec<u8>>,
}

/// A received direct message, surfaced to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDm {
    pub from: UserId,
    pub from_name: String,
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
}

/// Account material a device receives when it links to an existing device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedAccount {
    pub secret: [u8; 32],
    pub account_id: String,
}

/// A channel summary for listing in the UI.
#[derive(Debug, Clone)]
pub struct ChannelSummary {
    pub id: ConversationId,
    pub name: String,
    pub member_count: usize,
    /// The channel owner's device `user_id` — only the owner may rename the channel or
    /// change its membership. The UI gates the rename action on this.
    pub owner: String,
    /// Restored history from a previous device; this device has no sender key.
    pub archived: bool,
    /// Owner-authored terminal event has closed this channel for new activity.
    pub dissolved: bool,
    /// A departed local member may still read retained history, but cannot send.
    pub left: bool,
    pub pending_invite: bool,
}

fn archived_channel_summaries(log: &PersistentEventLog, book: &ChannelBook) -> Vec<ChannelSummary> {
    log.conversations()
        .into_iter()
        .filter(|id| book.state(id).is_none())
        .filter_map(|id| {
            let state = ChannelBook::projected_metadata(id, &log.events(&id))?;
            Some(ChannelSummary {
                id,
                name: state.name().to_string(),
                member_count: state.members().len(),
                owner: state.owner().to_string(),
                archived: true,
                dissolved: state.is_dissolved(),
                left: false,
                pending_invite: false,
            })
        })
        .collect()
}

/// A search match: a message whose text contains the query, with enough context to
/// label it and navigate to its conversation.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub is_channel: bool,
    /// Verified account binding from the same roster snapshot as this hit.
    /// UI filtering/navigation must not depend on a later, possibly evicted roster.
    pub account_id: Option<String>,
    /// The peer user-id (DM) or channel id hex (channel) — the navigation target.
    pub target: String,
    /// The peer or channel display name.
    pub label: String,
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub wall_clock: u64,
}

/// A merged conversation-history entry (sent or received), for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: EventId,
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub wall_clock: u64,
    pub reply_to: Option<EventId>,
    /// Present when this entry is a file/media message (a `FileManifest` event) rather
    /// than a text message. Carries the metadata the UI needs to render an inline media
    /// bubble or a file card; `text` is empty for a file entry.
    pub file: Option<FileHistoryInfo>,
    /// True when this message was recalled (by its author). The UI renders a placeholder
    /// ("X recalled a message"); `text`/`file` are blanked and never returned.
    pub recalled: bool,
    /// For a message WE recalled, the original text — so the sender can "re-edit" it back
    /// into the composer (WeChat behaviour). `None` for peers' recalls (never expose their
    /// content) and for non-text (file) messages. Only ever set when `recalled` is true.
    pub recalled_text: Option<Vec<u8>>,
    /// An animated-sticker id (emoji codepoint) when this message is a sticker. The UI
    /// renders the bundled animation; `text` holds the fallback emoji char.
    pub sticker: Option<String>,
}

/// File metadata for a [`HistoryEntry`] that represents a file/media message. Lets the
/// UI render media inline (image/video) or a file card, and `read_file`/`save_file` the
/// bytes by `file_conv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHistoryInfo {
    /// The per-file conversation holding the chunk events (pass to `read_file`/`save_file`).
    pub file_conv: ConversationId,
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// Inline MEDIA (media button) vs generic attachment (attach button) — by sender intent.
    pub media: bool,
}

/// Errors from node operations.
#[derive(Debug)]
pub enum NodeError {
    /// Caller-supplied local data cannot be accepted.
    InvalidInput(String),
    RequestConflict,
    /// The host rejected a local operation under its owner guard.
    Authorization(String),
    /// `send_dm` to a `user_id` not in the roster.
    UnknownPeer(UserId),
    /// Sealing the DM payload failed.
    Seal(crate::dm::DmError),
    /// Appending the event locally failed.
    Log(crate::eventlog::LogError),
    /// The networked sync session failed.
    Session(SessionError),
    /// A channel operation failed (unknown channel, or key/message sealing).
    Channel(String),
    /// A file operation failed (read/seal/reassemble/write, unknown file, or a file
    /// too large for the current single-frame sync — see `send_file_*`).
    File(String),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::RequestConflict => {
                write!(f, "message request conflicts with prior acceptance")
            }
            NodeError::InvalidInput(m) => write!(f, "invalid input: {m}"),
            NodeError::Authorization(m) => write!(f, "authorization: {m}"),
            NodeError::UnknownPeer(u) => write!(f, "unknown peer: {u}"),
            NodeError::Seal(e) => write!(f, "seal error: {e}"),
            NodeError::Log(e) => write!(f, "log error: {e}"),
            NodeError::Session(e) => write!(f, "session error: {e}"),
            NodeError::Channel(m) => write!(f, "channel error: {m}"),
            NodeError::File(m) => write!(f, "file error: {m}"),
        }
    }
}

impl std::error::Error for NodeError {}

#[cfg(test)]
type BlockingTestHook = Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>;

/// The node: identity + event log + shared roster + an outbound stream of
/// received DMs. Construct with [`Node::open`]; share as `Arc<Node>`.
pub struct Node {
    pub(in crate::node) outgoing_requests_gate: tokio::sync::Mutex<()>,
    pub(in crate::node) sessions: super::session_pool::SessionPool,
    pub(in crate::node) expiry_clock_start: (std::time::Instant, u64),
    pub(in crate::node) expiry_first_seen: Mutex<HashMap<EventId, std::time::Instant>>,
    pub(in crate::node) runtime_work: Arc<super::runtime_work::RuntimeWork>,
    #[cfg(test)]
    pub(in crate::node) delivery_snapshot_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) peer_snapshot_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) remember_peer_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) file_transfer_peak: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(in crate::node) empty_file_cache_self_wakes: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(in crate::node) accepted_hook: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub(in crate::node) privacy: Arc<super::privacy_runtime::PrivacyControl>,
    /// Host preference. Relay selection and every relay dial consult this live flag.
    pub(in crate::node) post_office_enabled: std::sync::atomic::AtomicBool,
    pub(in crate::node) post_office_send_enabled: std::sync::atomic::AtomicBool,
    pub(in crate::node) post_office_receive_enabled: std::sync::atomic::AtomicBool,
    /// Session-only IDs of relay envelopes already processed. Losing this cache
    /// on restart causes a safe re-download, never a lost inner event.
    pub(in crate::node) opaque_seen: Mutex<HashMap<ConversationId, HashSet<EventId>>>,
    // Fields are `pub(in crate::node)` so the per-domain `impl Node` blocks in sibling
    // files (dm/channels/files/queries/linking/serving) can reach them; they stay
    // private to the `node` module (no wider exposure).
    pub(in crate::node) identity: DeviceIdentity,
    /// This node's cryptographic account (cross-device handle). A per-device account
    /// derived from the device key when opened via [`Node::open`]; the real shared
    /// account is injected by [`Node::open_with_account`]. Holds the secret so this
    /// device can serve a pairing request (transfer the account to a new device).
    pub(in crate::node) account: Account,
    pub(in crate::node) account_devices: Mutex<super::account_devices::AccountDevices>,
    pub(in crate::node) account_suppression: Mutex<super::account_suppression::AccountSuppression>,
    pub(in crate::node) account_sync_policy:
        Mutex<super::account_sync_policy::DeviceSyncPolicyStore>,
    pub(in crate::node) account_preferences: Mutex<super::account_preferences::AccountPreferences>,
    pub(in crate::node) account_file_proofs: Mutex<super::account_file_proofs::AccountFileProofs>,
    pub(in crate::node) account_sync_notify: tokio::sync::Notify,
    pub(in crate::node) account_history_revision: std::sync::atomic::AtomicU64,
    pub(in crate::node) account_sync_status:
        Mutex<std::collections::HashMap<String, super::account_sync::DeviceSyncStatus>>,
    /// One-time pairing code while in "link a device" mode (linker side). Set by
    /// [`Node::start_linking`], cleared on success or [`Node::stop_linking`].
    pub(in crate::node) pending_link: Mutex<Option<PendingLink>>,
    pub(in crate::node) log: Mutex<PersistentEventLog>,
    pub(in crate::node) sentlog: Mutex<SentLog>,
    pub(in crate::node) roster: Arc<Mutex<Roster>>,
    /// Session-local preferred address after a successful pinned Noise handshake.
    pub(in crate::node) preferred_routes: Mutex<HashMap<UserId, std::net::SocketAddr>>,
    pub(in crate::node) incoming: mpsc::UnboundedSender<ReceivedDm>,
    pub(in crate::node) channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
    pub(in crate::node) channels: Mutex<ChannelBook>,
    pub(in crate::node) pending_channel_plaintext: Mutex<super::serving::PendingChannelPlaintext>,
    pub(in crate::node) channel_projection_cache:
        Mutex<super::channel_projection_cache::ChannelProjectionCache>,
    #[cfg(test)]
    pub(in crate::node) channel_projection_replays: std::sync::atomic::AtomicUsize,
    /// Fixed read-only summaries recovered from previous devices at open time.
    pub(in crate::node) archived_channels: Vec<ChannelSummary>,
    /// Public author of the device that produced a restored local history.
    /// Never contains the retired device's private key.
    pub(in crate::node) recovery_origins: Vec<crate::eventlog::Author>,
    pub(in crate::node) recovery_contacts: Vec<String>,
    pub(in crate::node) recovered_sent_ids:
        Mutex<Option<super::recovered_sent_ids::RecoveredSentIds>>,
    /// Durable, encrypted store of this node's per-channel sending keys (`my_sender`).
    /// `my_sender` is in-memory only and rebuilt empty on restart; persisting it here
    /// lets us resume our sending chains so receivers (first-wins on chains they hold)
    /// can still decrypt our post-restart messages.
    pub(in crate::node) channel_senders: Mutex<ChannelSenderStore>,
    /// Last confirmed durable holders of our own channel events.
    pub(in crate::node) channel_custody: Mutex<ChannelCustodyStore>,
    #[cfg(test)]
    pub(in crate::node) channel_last_send: Mutex<Option<(usize, u64)>>,
    pub(in crate::node) emitted: Mutex<HashSet<EventId>>,
    pub(in crate::node) file_incoming: mpsc::UnboundedSender<ReceivedFile>,
    pub(in crate::node) files: Mutex<FileBook>,
    /// Per-file conversations we've seen a manifest for but don't yet hold all chunks of —
    /// surfaced this session and awaiting their data. A background task pulls these DIRECTLY
    /// from peers (the post-office drain only covers files when a PO exists; on a plain
    /// peer-to-peer LAN the sender's one-shot push is otherwise the only delivery, so a
    /// missed push would strand the file at 0 chunks). Cleared once a file completes; not
    /// seeded from saved files (whose chunks are pruned), so we never re-pull a saved file.
    pub(in crate::node) pending_files: Mutex<HashSet<ConversationId>>,
    /// Encrypted, account-local attachment receive preferences and exact offer decisions.
    pub(in crate::node) attachment_consent: Mutex<super::attachment_consent::AttachmentConsent>,
    /// Durable pairwise file-consent controls, separate from local receive policy.
    pub(in crate::node) file_consent: Mutex<super::file_consent_store::FileConsentStore>,
    /// Durable chat-media store under `<account_dir>/media`: the plaintext bytes of
    /// image/screenshot/video messages, copied here on send + receive-complete so the
    /// inline preview loads from a durable source that survives chunk prune + restart.
    /// SEPARATE from the chunked attachment path (generic files are never written here).
    pub(in crate::node) media: crate::node::media_store::MediaStore,
    /// Per-peer Double Ratchet sessions (forward-secret DM crypto), encrypted on disk.
    pub(in crate::node) dm_ratchet: Mutex<DmRatchet>,
    /// Serializes every DM ratchet consumer and ordered recovery.
    pub(in crate::node) delivery: Mutex<super::delivery_store::DeliveryStore>,
    /// Transient recipient Have counts for active outgoing file destinations.
    pub(in crate::node) outgoing_file_acks: Mutex<HashMap<(EventId, EventId), u32>>,
    /// Last authenticated recipient answer for each outgoing manifest destination.
    pub(in crate::node) outgoing_file_consents:
        Mutex<HashMap<(EventId, EventId), crate::eventlog::sync::FileConsentStatus>>,
    pub(in crate::node) channel_recovery_notify: tokio::sync::Notify,
    pub(in crate::node) channel_recovery:
        Mutex<HashMap<ConversationId, super::channels::ChannelRecovery>>,
    pub(in crate::node) delivery_notify: tokio::sync::Notify,
    pub(in crate::node) delivery_status_notify: tokio::sync::Notify,
    /// Exact device fingerprints the host user has safety-number verified for
    /// recipient-initiated DM catch-up. Discovery alone never adds entries.
    pub(in crate::node) verified_dm_pull_peers: Mutex<HashSet<UserId>>,
    /// Ephemeral identities introduced by an explicitly enabled bridge relay.
    pub(in crate::node) bridge_dm_pull_peers: Mutex<HashMap<UserId, std::time::Instant>>,
    pub(in crate::node) physical_lan_only: std::sync::atomic::AtomicBool,
    pub(in crate::node) bridge_discovery_enabled: std::sync::atomic::AtomicBool,
    pub(in crate::node) bridge_hub_id: Mutex<Option<String>>,
    pub(in crate::node) bridge_notify: tokio::sync::Notify,
    pub(in crate::node) bridge_refresh: tokio::sync::Mutex<()>,
    pub(in crate::node) bridge_generation: std::sync::atomic::AtomicU64,
    pub(in crate::node) delivery_control_ids:
        Mutex<HashMap<EventId, super::delivery_store::ControlBinding>>,
    pub(in crate::node) delivery_suspended: std::sync::atomic::AtomicBool,
    /// Decrypted received-message plaintext, for serving history after the wire key is gone.
    pub(in crate::node) received: Mutex<ReceivedLog>,
    /// Durable record of file manifests we've SURFACED (reuses the ReceivedLog format),
    /// so the file book's emitted set + manifests survive restart without marking
    /// never-opened manifests as emitted (which would lose the file). See `Node::open`.
    pub(in crate::node) received_files: Mutex<ReceivedLog>,
    /// Durable record of OUR OWN recalls (sealed to peers, so un-openable from our own log).
    /// `history`/`account_history` read this to tombstone our recalled messages; it survives
    /// restart (unlike own reactions).
    pub(in crate::node) recalls: Mutex<crate::node::recall::RecallLog>,
    /// Avatars peers PROPAGATED to us via signed profiles, keyed by account id (one per
    /// account, newest-wins, durable). Read by `peer_avatar`/`peer_avatars` and surfaced
    /// live on `profile_incoming`.
    pub(in crate::node) profiles: Mutex<crate::node::profile_store::ProfileStore>,
    pub(in crate::node) language_state: Mutex<super::language_state::LanguageState>,
    /// Live stream of received peer profiles (avatar updates), surfaced to the app. The
    /// node owns BOTH ends (created in `open_with_account`) so the constructor signature
    /// is unchanged across its many call sites; the runtime claims the receiver once via
    /// [`Node::take_profile_receiver`].
    pub(in crate::node) profile_incoming: mpsc::UnboundedSender<ReceivedProfile>,
    pub(in crate::node) profile_rx: Mutex<Option<mpsc::UnboundedReceiver<ReceivedProfile>>>,
    /// Live stream of inbound call signals (WebRTC SDP/bye), surfaced to the app. Like
    /// `profile_incoming`, the node owns BOTH ends (created in `open_with_account`) so the
    /// constructor signature is unchanged; the runtime claims the receiver once via
    /// [`Node::take_call_signal_receiver`]. Ephemeral — never touches the event log.
    pub(in crate::node) call_signal_incoming:
        mpsc::UnboundedSender<crate::node::call::ReceivedCallSignal>,
    pub(in crate::node) call_signal_rx:
        Mutex<Option<mpsc::UnboundedReceiver<crate::node::call::ReceivedCallSignal>>>,
    /// Profile events we've already surfaced this session (dedup), so re-syncing a peer's
    /// device-pair conversation doesn't re-emit a profile we already applied.
    pub(in crate::node) profiles_emitted: Mutex<HashSet<EventId>>,
    /// Our OWN current profile (the avatar we last set), held so we can re-publish it to a
    /// peer the moment it's freshly discovered. `None` until the user sets/clears one this
    /// session (or it's reloaded on open). Behind a mutex; cheap to clone for a send.
    pub(in crate::node) my_profile: Mutex<Option<crate::node::profile::ProfilePayload>>,
    /// Durable latest local actions; outbound sealed reactions cannot be reopened by us.
    pub(in crate::node) my_dm_reactions: Mutex<super::reaction_store::ReactionStore>,
}

impl Node {
    /// Open a node backed by a durable event log at `log_path` and a sent-plaintext
    /// sidecar at `sent_path` (both encrypted with `password`). Restored history is
    /// seeded into the already-emitted set so it is NOT re-streamed — only events
    /// received after open are surfaced live.
    pub fn open(
        identity: DeviceIdentity,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
        channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
        file_incoming: mpsc::UnboundedSender<ReceivedFile>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        // Default: a deterministic per-device account derived from the device key, so
        // each unlinked device is its own stable account until it links (Plan 4).
        let account = Account::from_secret_bytes(identity.secret_bytes().0);
        Self::open_with_account(
            identity,
            account,
            roster,
            incoming,
            channel_incoming,
            file_incoming,
            log_path,
            sent_path,
            password,
        )
    }

    /// Open a node bound to an explicit cryptographic `account` (the cross-device
    /// handle). Used by the real app/CLI, which load the account from its keystore;
    /// most tests use [`Node::open`], which defaults the account to the device key.
    #[allow(clippy::too_many_arguments)]
    pub fn open_with_account(
        identity: DeviceIdentity,
        account: Account,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
        channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
        file_incoming: mpsc::UnboundedSender<ReceivedFile>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        let dir = log_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        // Each store does its own slow, CPU-bound password KDF over an independent
        // file. Open them on scoped threads so the KDFs run in parallel: on a
        // multi-core machine the wall-clock cost collapses to roughly one KDF.
        let (
            log_res,
            sent_res,
            sessions_res,
            received_res,
            csenders_res,
            custody_res,
            recv_files_res,
            profiles_res,
            language_res,
            recalls_res,
            consent_res,
            consent_controls_res,
            origin_res,
            contacts_res,
            recovered_ids_res,
        ) = std::thread::scope(|s| {
            let h_log = s.spawn(|| PersistentEventLog::open(log_path, password));
            let h_sent = s.spawn(|| SentLog::open(sent_path, password));
            let h_sess = s.spawn(|| RatchetSessions::open(&dir.join("ratchet.sessions"), password));
            let h_recv = s.spawn(|| ReceivedLog::open(&dir.join("received.log"), password));
            let h_csnd =
                s.spawn(|| ChannelSenderStore::open(&dir.join("channel.senders"), password));
            let h_custody =
                s.spawn(|| ChannelCustodyStore::open(&dir.join("channel.custody"), password));
            // Durable record of surfaced file manifests (reuses the ReceivedLog format),
            // so the file book's emitted set survives restart WITHOUT marking unopened
            // manifests as emitted — see the seeding below.
            let h_files =
                s.spawn(|| ReceivedLog::open_file_cards(&dir.join("received_files.log"), password));
            // Durable store of avatars peers propagated to us (one per account, newest-wins).
            let h_language = s.spawn(|| {
                super::language_state::LanguageState::open(&dir.join("language.log"), password)
            });
            let h_profiles = s.spawn(|| {
                crate::node::profile_store::ProfileStore::open(&dir.join("profiles.log"), password)
            });
            // Durable record of OUR OWN recalls: our recall payloads are sealed to peers (we
            // can't open them from our own log), so this is how `history` knows which of our
            // messages we recalled — and it must survive restart.
            let h_recalls = s
                .spawn(|| crate::node::recall::RecallLog::open(&dir.join("recalls.log"), password));
            let h_consent = s.spawn(|| {
                super::attachment_consent::AttachmentConsent::open(
                    &dir.join("attachment-consent.log"),
                    password,
                )
            });
            let h_consent_controls = s.spawn(|| {
                super::file_consent_store::FileConsentStore::open(
                    &dir.join("file-consent-controls.log"),
                    password,
                )
            });
            let h_origin = s.spawn(|| {
                super::recovery::load_recovery_origins(&dir.join("recovery-origin.log"), password)
            });
            let h_contacts = s.spawn(|| {
                super::recovery::load_recovery_contacts(
                    &dir.join("recovery-contacts.log"),
                    password,
                )
            });
            let h_recovered_ids = s.spawn(|| {
                let path = dir.join("recovery-sent-ids.log");
                if path.exists() {
                    super::recovered_sent_ids::RecoveredSentIds::open(&path, password).map(Some)
                } else {
                    Ok(None)
                }
            });
            (
                h_log.join(),
                h_sent.join(),
                h_sess.join(),
                h_recv.join(),
                h_csnd.join(),
                h_custody.join(),
                h_files.join(),
                h_profiles.join(),
                h_language.join(),
                h_recalls.join(),
                h_consent.join(),
                h_consent_controls.join(),
                h_origin.join(),
                h_contacts.join(),
                h_recovered_ids.join(),
            )
        });
        // A join() error means the opening thread panicked; re-raise the panic so it
        // is not silently swallowed. A normal open failure surfaces as the inner Err.
        let mut log = log_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let mut sentlog = sent_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let sessions = sessions_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let mut received = received_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let mut channel_senders = csenders_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let channel_custody = custody_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let mut received_files = recv_files_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let profiles = profiles_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let language_state =
            language_res.unwrap_or_else(|error| std::panic::resume_unwind(error))?;
        let my_profile = profiles
            .snapshot_entries()
            .into_iter()
            .find(|(id, _, _)| id == &account.account_id())
            .map(|(_, updated_at, avatar)| {
                crate::node::profile::ProfilePayload::new(&account, avatar, updated_at)
            });
        let my_profile = match (my_profile, language_state.profile(&account.account_id())) {
            (Some(avatar), Some(policy)) => {
                Some(crate::node::profile::ProfilePayload::new_with_language(
                    &account,
                    avatar.avatar,
                    avatar.updated_at.max(policy.updated_at),
                    policy.language,
                ))
            }
            (avatar, policy) => policy.or(avatar),
        };
        let recalls = recalls_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let attachment_consent = consent_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let file_consent = consent_controls_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let recovery_origins = origin_res
            .unwrap_or_else(|e| std::panic::resume_unwind(e))?
            .into_iter()
            .map(crate::eventlog::Author::from_ed25519)
            .collect();
        let recovery_contacts = contacts_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let recovered_sent_ids =
            recovered_ids_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let mut dm_ratchet = DmRatchet::new(sessions);
        let mut delivery = super::delivery_store::DeliveryStore::open_for_log(log_path, password)?;
        delivery.bind_profile(
            log_path,
            password,
            &identity.public(),
            &account.account_id(),
        )?;
        delivery.validate_owner(&identity.public(), &account.account_id())?;
        while delivery
            .recover_next_with_channels(
                &mut dm_ratchet,
                &mut log,
                &mut sentlog,
                &mut received,
                Some(&mut received_files),
                Some(&mut channel_senders),
            )?
            .is_some()
        {}
        received_files
            .remove_where(|entry| delivery.file_erased(entry.conversation, entry.event_id))?;
        let delivery_control_ids = delivery.control_ids();
        // Seed `emitted` with the events we have ALREADY recorded to the received store — NOT
        // every id in the log. An event that was ingested durably but never recorded (it
        // arrived before we learned its author — the DM-convergence race — or before its
        // ratchet key was derivable) must stay eligible for `emit_new_messages` to retry after
        // a restart; seeding it as emitted here would drop it from history forever. Self-
        // authored events are excluded by the author check in `emit_new_messages`, so they
        // need not be seeded.
        let emitted: HashSet<EventId> = received
            .conversations()
            .iter()
            .flat_map(|c| received.entries(c))
            .map(|e| e.event_id)
            .collect();
        let mut account_devices = super::account_devices::AccountDevices::open(
            &super::account_devices::file_path(dir, &account.account_id()),
            password,
            &account.account_id(),
        )?;
        account_devices.insert(super::account_devices::AccountDevice {
            public: identity.public(),
            certificate: account.certify(&identity.public().ed25519_pub),
        })?;
        let account_suppression = super::account_suppression::AccountSuppression::open(
            &dir.join("account-suppression.log"),
            password,
        )?;
        let own_device_ids: HashSet<_> = account_devices
            .devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        let mut channels = ChannelBook::new();
        for conv in log.conversations() {
            let events = log.events(&conv);
            let mut replay = super::channel::ChannelReplay::new(&events);
            // Reinstall projections lost between signed-log ingestion and a
            // failed/crashed sidecar write. Erases are authoritative; recipient
            // deadlines keep the existing conservative restart policy.
            loop {
                let (messages, limited) = channels.process_replay(
                    &identity,
                    conv,
                    &mut replay,
                    64,
                    super::channel::CHANNEL_PENDING_BYTES,
                );
                let done = !limited;
                for msg in messages {
                    if received.entry(msg.event_id).is_some()
                        || account_suppression.contains(conv, msg.event_id)
                    {
                        continue;
                    }
                    let own = own_device_ids.contains(&msg.from);
                    if !own && crate::expiry::deadline(&msg.wrapped).is_some() {
                        continue;
                    }
                    received.record_durable(&super::received_log::ReceivedEntry {
                        event_id: msg.event_id,
                        conversation: conv,
                        from: msg.from,
                        wall_clock: msg.wall_clock,
                        plaintext: msg.wrapped,
                    })?;
                }
                if done {
                    break;
                }
            }
        }
        let archived_channels = archived_channel_summaries(&log, &channels);
        // Restore our persisted sending chains into each channel state. The event log
        // only carries OTHER members' sender keys, so `my_sender` rebuilds empty above;
        // without this, a post-restart send would mint a fresh chain that receivers
        // (first-wins on the chain they already hold) cannot follow.
        for channel in channels.channel_ids() {
            for (epoch, bytes) in channel_senders.get(&channel) {
                if let Some(sk) = SenderKey::deserialize(&bytes)
                    && let Some(state) = channels.state_mut(&channel)
                {
                    state.import_my_sender(epoch, sk);
                }
            }
        }
        // Seed the file book from the durable `received_files` record — the manifests we
        // actually SURFACED last session (restoring them so they stay saveable), and only
        // THOSE ids into the emitted set. A manifest that was ingested durably but never
        // opened (its author not yet in the roster — the DM-convergence race) is absent
        // here, so `process_file_events` retries it after restart instead of dropping the
        // file forever. (Mirrors the message-path `emitted` seeding above.)
        let account_file_proofs = super::account_file_proofs::AccountFileProofs::open(
            &dir.join("account-file-proofs.log"),
            password,
        )?;
        let mut files = FileBook::new();
        for c in received_files.conversations() {
            for entry in received_files.entries(&c) {
                if delivery.file_erased(entry.conversation, entry.event_id)
                    || delivery.manifest_event_erased(entry.event_id)
                {
                    continue;
                }
                let decoded = super::files::validated_manifest(&entry.plaintext);
                let existing = decoded
                    .as_ref()
                    .and_then(|m| files.manifest(&m.file_conv()));
                let proof = log
                    .get(&entry.event_id)
                    .or_else(|| account_file_proofs.get(&entry.event_id));
                if let Some(manifest) = super::files::eligible_manifest_row(&entry, proof, existing)
                {
                    files.record_event(entry.event_id, proof.unwrap().author, manifest);
                    files.mark_emitted(entry.event_id);
                }
            }
        }
        // Durable encrypted media survives chunk prune and restart.
        let media = crate::node::media_store::MediaStore::open(dir, password, &identity.user_id())
            .map_err(|e| LogError::Io(std::io::Error::other(format!("media store open: {e}"))))?;
        let live_media: std::collections::HashSet<_> = received_files
            .conversations()
            .iter()
            .flat_map(|conversation| received_files.entries(conversation))
            .filter_map(|row| {
                super::files::validated_manifest(&row.plaintext)
                    .map(|manifest| manifest.file_conv())
            })
            .collect();
        media
            .prune_unreferenced(&live_media)
            .map_err(|e| LogError::Io(std::io::Error::other(format!("media prune: {e}"))))?;
        let (profile_tx, profile_rx) = mpsc::unbounded_channel::<ReceivedProfile>();
        let (call_signal_tx, call_signal_rx) =
            mpsc::unbounded_channel::<crate::node::call::ReceivedCallSignal>();
        let account_sync_policy = super::account_sync_policy::DeviceSyncPolicyStore::open(
            &dir.join(format!("account-sync-policy-{}.log", account.account_id())),
            password,
        )?;
        let account_preferences = super::account_preferences::AccountPreferences::open(
            &dir.join("account-preferences.log"),
            password,
        )?;
        let my_dm_reactions = super::reaction_store::ReactionStore::open(
            &log_path.with_extension(format!("{}-reactions.log", account.account_id())),
            password,
        )?;
        let node = Arc::new(Self {
            expiry_clock_start: (std::time::Instant::now(), now_millis()),
            expiry_first_seen: Mutex::new(HashMap::new()),
            runtime_work: Arc::new(super::runtime_work::RuntimeWork::default()),
            #[cfg(test)]
            delivery_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            peer_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            remember_peer_hook: Mutex::new(None),
            #[cfg(test)]
            file_transfer_peak: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            empty_file_cache_self_wakes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            accepted_hook: Mutex::new(None),
            privacy: Arc::new(super::privacy_runtime::PrivacyControl::default()),
            identity,
            account,
            account_devices: Mutex::new(account_devices),
            account_suppression: Mutex::new(account_suppression),
            account_sync_policy: Mutex::new(account_sync_policy),
            account_preferences: Mutex::new(account_preferences),
            account_file_proofs: Mutex::new(account_file_proofs),
            account_sync_notify: tokio::sync::Notify::new(),
            account_history_revision: std::sync::atomic::AtomicU64::new(0),
            account_sync_status: Mutex::new(std::collections::HashMap::new()),
            profiles: Mutex::new(profiles),
            language_state: Mutex::new(language_state),
            profile_incoming: profile_tx,
            profile_rx: Mutex::new(Some(profile_rx)),
            call_signal_incoming: call_signal_tx,
            call_signal_rx: Mutex::new(Some(call_signal_rx)),
            profiles_emitted: Mutex::new(HashSet::new()),
            my_profile: Mutex::new(my_profile),
            pending_link: Mutex::new(None),
            log: Mutex::new(log),
            sentlog: Mutex::new(sentlog),
            roster,
            incoming,
            channel_incoming,
            channels: Mutex::new(channels),
            pending_channel_plaintext: Mutex::new(Default::default()),
            archived_channels,
            recovery_origins,
            recovery_contacts,
            recovered_sent_ids: Mutex::new(recovered_sent_ids),
            outgoing_requests_gate: tokio::sync::Mutex::new(()),
            channel_senders: Mutex::new(channel_senders),
            channel_custody: Mutex::new(channel_custody),
            channel_projection_cache: Mutex::new(Default::default()),
            #[cfg(test)]
            channel_projection_replays: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            channel_last_send: Mutex::new(None),
            emitted: Mutex::new(emitted),
            file_incoming,
            files: Mutex::new(files),
            pending_files: Mutex::new(HashSet::new()),
            attachment_consent: Mutex::new(attachment_consent),
            file_consent: Mutex::new(file_consent),
            media,
            dm_ratchet: Mutex::new(dm_ratchet),
            delivery: Mutex::new(delivery),
            outgoing_file_acks: Mutex::new(HashMap::new()),
            outgoing_file_consents: Mutex::new(HashMap::new()),
            channel_recovery_notify: tokio::sync::Notify::new(),
            channel_recovery: Mutex::new(HashMap::new()),
            delivery_notify: tokio::sync::Notify::new(),
            delivery_status_notify: tokio::sync::Notify::new(),
            verified_dm_pull_peers: Mutex::new(HashSet::new()),
            bridge_dm_pull_peers: Mutex::new(HashMap::new()),
            physical_lan_only: std::sync::atomic::AtomicBool::new(false),
            bridge_discovery_enabled: std::sync::atomic::AtomicBool::new(false),
            bridge_hub_id: Mutex::new(None),
            bridge_notify: tokio::sync::Notify::new(),
            bridge_refresh: tokio::sync::Mutex::new(()),
            sessions: super::session_pool::SessionPool::default(),
            bridge_generation: std::sync::atomic::AtomicU64::new(0),
            delivery_control_ids: Mutex::new(delivery_control_ids),
            delivery_suspended: std::sync::atomic::AtomicBool::new(false),
            post_office_enabled: std::sync::atomic::AtomicBool::new(true),
            post_office_send_enabled: std::sync::atomic::AtomicBool::new(true),
            post_office_receive_enabled: std::sync::atomic::AtomicBool::new(true),
            opaque_seen: Mutex::new(HashMap::new()),
            preferred_routes: Mutex::new(HashMap::new()),
            received: Mutex::new(received),
            received_files: Mutex::new(received_files),
            recalls: Mutex::new(recalls),
            my_dm_reactions: Mutex::new(my_dm_reactions),
        });
        node.prune_expired_received_on_open()
            .map_err(|error| LogError::CorruptFile(format!("expiry cleanup: {error}")))?;
        node.queue_approved_incomplete_files();
        Ok(node)
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> UserId {
        self.identity.public().user_id()
    }

    /// This node's cryptographic account id (cross-device handle).
    pub fn account_id(&self) -> String {
        self.account.account_id()
    }

    /// Certified devices known to share this account, including this device.
    pub fn linked_devices(&self) -> Vec<super::account_devices::AccountDevice> {
        self.account_devices
            .lock()
            .expect("account_devices mutex not poisoned")
            .devices()
    }

    /// Build a freshly-signed account-bound announce advertising `display_name` and
    /// `tcp_port`. The device key (and account cert) are held by the node, so this is
    /// how the runtime re-signs the announce when the display name changes — the keys
    /// never leave the node.
    pub fn signed_announce(
        &self,
        display_name: impl Into<String>,
        tcp_port: u16,
    ) -> crate::discovery::Announce {
        crate::discovery::Announce::new_with_account(
            &self.identity,
            &self.account,
            display_name,
            tcp_port,
        )
    }

    /// Produce only the domain-separated response to a routed discovery query.
    /// The host never receives this node's private signing key.
    pub fn signed_discovery_response(
        &self,
        nonce: crate::discovery::query::Nonce,
        announce: &crate::discovery::Announce,
    ) -> Option<Vec<u8>> {
        crate::discovery::query::encode_response_signed(
            nonce,
            announce,
            &self.identity.public(),
            |input| self.identity.sign(input),
        )
    }

    /// Claim the receiver for live peer-profile (avatar) updates. Returns it once; later
    /// calls return `None` (the runtime takes it exactly once at startup).
    pub fn take_profile_receiver(&self) -> Option<mpsc::UnboundedReceiver<ReceivedProfile>> {
        self.profile_rx
            .lock()
            .expect("profile_rx mutex not poisoned")
            .take()
    }

    /// Claim the receiver for live inbound call signals (WebRTC SDP/bye). Returns it once;
    /// later calls return `None` (the runtime takes it exactly once at startup).
    pub fn take_call_signal_receiver(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<crate::node::call::ReceivedCallSignal>> {
        self.call_signal_rx
            .lock()
            .expect("call_signal_rx mutex not poisoned")
            .take()
    }

    /// The avatar a peer ACCOUNT propagated to us (data-URL bytes), or `None` if unknown
    /// or cleared. Durable across restart.
    pub fn peer_avatar(&self, account_id: &str) -> Option<Vec<u8>> {
        self.profiles
            .lock()
            .expect("profiles mutex not poisoned")
            .avatar(account_id)
    }

    /// Every peer avatar we hold, as `account_id -> data-URL bytes`. For seeding the UI
    /// on startup so received avatars survive a relaunch.
    pub fn peer_avatars(&self) -> HashMap<String, Vec<u8>> {
        self.profiles
            .lock()
            .expect("profiles mutex not poisoned")
            .all()
    }

    /// Durable signed profile state, including clear tombstones for startup reconciliation.
    pub fn peer_profile_states(&self) -> HashMap<String, Option<Vec<u8>>> {
        self.profiles
            .lock()
            .expect("profiles mutex not poisoned")
            .snapshot_entries()
            .into_iter()
            .map(|(account, _, avatar)| (account, avatar))
            .collect()
    }

    /// Summaries of all channels this node is a member of.
    pub fn list_channels(&self) -> Vec<ChannelSummary> {
        let own_id = self.user_id();
        let book = self.channels.lock().expect("channels mutex not poisoned");
        let mut channels: Vec<_> = book
            .channel_ids()
            .into_iter()
            .filter_map(|id| {
                book.state(&id).map(|s| ChannelSummary {
                    id,
                    name: s.name().to_string(),
                    member_count: s.members().len(),
                    owner: s.owner().to_string(),
                    archived: false,
                    dissolved: s.is_dissolved(),
                    left: !s.is_member(&own_id),
                    pending_invite: s.pending_invite(),
                })
            })
            .collect();
        channels.extend(
            self.archived_channels
                .iter()
                .filter(|archive| book.state(&archive.id).is_none())
                .cloned(),
        );
        channels
    }

    /// The current members of `channel` (empty if the channel is unknown).
    pub fn channel_members(&self, channel: ConversationId) -> Vec<PublicIdentity> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        book.state(&channel)
            .map(|s| s.members().to_vec())
            .unwrap_or_default()
    }

    pub fn channel_replica_enabled(&self, channel: ConversationId, member: &str) -> bool {
        self.channels
            .lock()
            .expect("channels mutex not poisoned")
            .replica_enabled(channel, member)
    }

    pub fn channel_pending_members(&self, channel: ConversationId) -> Vec<String> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        book.state(&channel)
            .map(|state| {
                state
                    .members()
                    .iter()
                    .map(PublicIdentity::user_id)
                    .filter(|user_id| !state.is_accepted_member(user_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn channel_activity(&self, channel: ConversationId) -> Vec<super::ChannelActivity> {
        self.channels
            .lock()
            .expect("channels mutex not poisoned")
            .activity(&channel)
    }

    /// The current owner of `channel`, or empty if the channel is unknown.
    pub fn channel_owner(&self, channel: ConversationId) -> String {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        book.state(&channel)
            .map(|s| s.owner().to_string())
            .unwrap_or_default()
    }

    /// Append a local event, sequencing it from the conversation's log position. Returns
    /// the event's `seq` (its position in our own per-author chain), used to index the
    /// sent sidecar for channel history.
    pub(in crate::node) fn append_event(
        &self,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) -> Result<u64, NodeError> {
        if self
            .channels
            .lock()
            .expect("channels mutex not poisoned")
            .state(&channel)
            .is_some_and(|state| state.is_dissolved())
        {
            return Err(NodeError::Channel("channel is dissolved".into()));
        }
        let independent_profile = kind == EventKind::Profile
            && self.linked_devices().iter().any(|device| {
                super::conversation::dm_conversation_id(&self.identity.public(), &device.public)
                    == channel
            });
        // Accepted immutable intents reserve their author's sequence even before
        // installation. Serialize every local allocation with that reservation.
        let mut delivery = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut log = self.log.lock().expect("log mutex not poisoned");
        let (mut parents, lamport) = log.prepare(&channel).map_err(NodeError::Log)?;
        if independent_profile {
            // Signed profile leaves must remain ingestible while sibling content
            // categories are paused. The event log permits sparse author sequences.
            parents.clear();
        }
        let seq = log
            .next_sequence(&channel, &self_author)
            .map_err(NodeError::Log)?;
        let event = Event::new(
            &self.identity,
            channel,
            seq,
            parents,
            lamport,
            now_millis(),
            kind,
            ciphertext,
        );
        if event.kind == EventKind::FileManifest && !super::session::event_fits_frame(&event) {
            return Err(NodeError::File(
                "file manifest exceeds transport frame".into(),
            ));
        }
        log.append(event).map_err(NodeError::Log)?;
        Ok(seq)
    }
}

/// Milliseconds since the Unix epoch, for an event's wall-clock field.
pub(in crate::node) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A fresh random 32-byte logical message id (shared across an account send's
/// per-device copies, so reactions/replies can target it account-wide).
pub(in crate::node) fn random_msg_id() -> [u8; 32] {
    use rand_core::RngCore;
    let mut id = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut id);
    id
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
