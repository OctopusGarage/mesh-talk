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
use crate::node::channel_custody::IndexedChannelCustodyStore as ChannelCustodyStore;
use crate::node::channel_senders::ChannelSenderStore;
use crate::node::dm_ratchet::DmRatchet;
use crate::node::filebook::ReceivedFile;
use crate::node::filebook::indexed::IndexedFileBook;
use crate::node::pairing::PendingLink;
use crate::node::ratchet_sessions::RatchetSessions;
use crate::node::received_store::ReceivedStore as ReceivedLog;
use crate::node::sent_store::SentStore as SentLog;
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
    /// Account-envelope logical ID matching account conversation history.
    /// Legacy device messages have no logical account-message identity.
    pub message_id: Option<EventId>,
    /// Authenticated account-envelope sender, when present.
    pub from_account: Option<String>,
    pub from: UserId,
    pub from_name: String,
    pub text: Vec<u8>,
    pub protection: Option<super::MessageProtection>,
    pub reply_to: Option<EventId>,
}

/// Account material a device receives when it links to an existing device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedAccount {
    pub secret: [u8; 32],
    pub account_id: String,
    pub created_at_ms: Option<u64>,
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

fn archived_channel_summaries(
    checkpoints: &super::channel::checkpoint::ChannelCheckpointStore,
) -> Result<Vec<ChannelSummary>, LogError> {
    let mut summaries = Vec::new();
    let mut after = None;
    loop {
        let page = checkpoints.metadata_page(after, false)?;
        if page.is_empty() {
            break;
        }
        for state in page {
            after = Some(state.id());
            summaries.push(ChannelSummary {
                id: state.id(),
                name: state.name().to_string(),
                member_count: state.members().len(),
                owner: state.owner().to_string(),
                archived: true,
                dissolved: state.is_dissolved(),
                left: false,
                pending_invite: false,
            });
        }
    }
    Ok(summaries)
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
    /// Authenticated absolute UTC deadline shared by sender and recipient.
    pub expires_at_ms: Option<u64>,
    pub id: EventId,
    /// Author-authenticated per-device manifest aliases for this logical file message.
    pub file_message_aliases: Vec<EventId>,
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub protection: Option<super::MessageProtection>,
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
    /// A stage-specific device enrollment failure.
    Linking(super::linking::LinkingError),
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
            NodeError::Linking(error) => write!(f, "device linking: {error}"),
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

impl std::error::Error for NodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Log(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::Linking(error) => Some(error),
            Self::Seal(error) => Some(error),
            _ => None,
        }
    }
}
impl From<crate::eventlog::LogError> for NodeError {
    fn from(error: crate::eventlog::LogError) -> Self {
        Self::Log(error)
    }
}

#[cfg(test)]
type BlockingTestHook = Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>;

/// The node: identity + event log + shared roster + an outbound stream of
/// received DMs. Construct with [`Node::open`]; share as `Arc<Node>`.
pub struct Node {
    pub(in crate::node) forwarding: Mutex<super::forwarding::ForwardStore>,
    pub(in crate::node) forwarding_gate: tokio::sync::Mutex<()>,
    pub(in crate::node) outgoing_requests_gate: tokio::sync::Mutex<()>,
    pub(in crate::node) sessions: super::session_pool::SessionPool,
    pub(in crate::node) expiry_clock_start: (std::time::Instant, u64),
    pub(in crate::node) expiry_first_seen: Mutex<HashMap<EventId, std::time::Instant>>,
    pub(in crate::node) runtime_work: Arc<super::runtime_work::RuntimeWork>,
    #[cfg(test)]
    pub(in crate::node) control_fanout_start_test_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) group_file_asset_recv_test_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) delivery_snapshot_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) withheld_file_scopes: Mutex<HashSet<ConversationId>>,
    #[cfg(test)]
    pub(in crate::node) untracked_chunk_validations: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(in crate::node) before_sent_delete_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) profile_read_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) profile_prepare_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) peer_snapshot_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) expiry_cleanup_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) remember_peer_hook: BlockingTestHook,
    #[cfg(test)]
    pub(in crate::node) file_transfer_peak: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(in crate::node) empty_file_cache_self_wakes: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(in crate::node) accepted_hook: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    #[cfg(test)]
    pub(in crate::node) account_session_test_hook:
        Mutex<Option<Arc<super::account_session::AccountSessionTestHook>>>,
    #[cfg(test)]
    pub(in crate::node) account_client_test_hook:
        Mutex<Option<Arc<super::account_session::AccountClientTestHook>>>,
    #[cfg(test)]
    pub(in crate::node) account_channel_maintenance_test_hook:
        Mutex<Option<Arc<super::channel_account_maintenance::AccountChannelMaintenanceTestHook>>>,
    pub(in crate::node) privacy: Arc<super::privacy_runtime::PrivacyControl>,
    /// Host preference. Relay selection and every relay dial consult this live flag.
    pub(in crate::node) post_office_enabled: std::sync::atomic::AtomicBool,
    pub(in crate::node) post_office_send_enabled: std::sync::atomic::AtomicBool,
    pub(in crate::node) post_office_receive_enabled: std::sync::atomic::AtomicBool,
    /// Optional disk hints, checked against retained history during mailbox sync.
    pub(in crate::node) opaque_receive_cache:
        Mutex<super::opaque_receive_cache::OpaqueReceiveCache>,
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
    pub(in crate::node) account_transitions:
        Mutex<super::account_transition_store::AccountTransitionStore>,
    pub(in crate::node) account_documents:
        Mutex<super::account_documents::store::AccountDocumentStore>,
    pub(in crate::node) group_file_prepare_gate: Arc<tokio::sync::Mutex<()>>,
    pub(in crate::node) group_operations:
        Mutex<super::account_group::operations::journal::OperationJournal>,
    pub(in crate::node) account_document_updates: tokio::sync::watch::Sender<u64>,
    pub(in crate::node) account_group_versions:
        Mutex<HashMap<String, super::account_documents::sync::GroupProtocolSupport>>,
    pub(in crate::node) operation_epoch: u64,
    pub(in crate::node) operation_queries:
        Mutex<HashMap<super::AccountGroupOperationId, (u64, std::time::Instant)>>,
    pub(in crate::node) operation_seen: Mutex<HashMap<String, [u8; 32]>>,
    pub(in crate::node) group_operation_versions:
        Mutex<HashMap<String, super::account_documents::sync::GroupOperationSupport>>,
    pub(in crate::node) account_suppression:
        Mutex<super::account_suppression::IndexedAccountSuppression>,
    pub(in crate::node) favorites: Mutex<super::favorites::FavoriteStore>,
    pub(in crate::node) favorite_media: super::media_store::MediaStore,
    pub(in crate::node) account_sync_policy:
        Mutex<super::account_sync_policy::DeviceSyncPolicyStore>,
    pub(super) account_access: Mutex<super::account_access::AccountAccessStore>,
    pub(super) account_access_updates:
        tokio::sync::watch::Sender<Option<super::AccountAccessSnapshot>>,
    pub(in crate::node) account_preferences: Mutex<super::account_preferences::AccountPreferences>,
    pub(in crate::node) file_message_identity_cursor: std::sync::atomic::AtomicUsize,
    pub(in crate::node) file_message_identities:
        Mutex<super::file_message_identity::IndexedFileMessageIdentities>,
    pub(in crate::node) account_file_proofs:
        Mutex<super::account_file_repository::IndexedAccountFileProofs>,
    pub(in crate::node) account_sync_notify: tokio::sync::Notify,
    pub(in crate::node) account_history_revision: std::sync::atomic::AtomicU64,
    pub(in crate::node) account_archive_cache: Mutex<super::account_sync::ArchiveCache>,
    pub(in crate::node) account_sync_commit: Mutex<()>,
    pub(in crate::node) account_wake: super::account_sync::AccountWake,
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
    pub(in crate::node) incoming: super::runtime_delivery::HostPayload<ReceivedDm>,
    pub(in crate::node) channel_incoming:
        super::runtime_delivery::HostPayload<ReceivedChannelMessage>,
    pub(in crate::node) reception_effects: super::reception_effects::ReceptionEffects,
    /// Serialize owner membership commits with conditional sibling enrollment.
    pub(in crate::node) channel_membership_commit: Mutex<()>,
    pub(in crate::node) channels: Mutex<ChannelBook>,
    pub(in crate::node) pending_channel_plaintext: Mutex<super::serving::PendingChannelPlaintext>,
    pub(in crate::node) pending_channel_files: Mutex<Option<super::files::ChannelFileTurn>>,
    pub(in crate::node) channel_projection_cache:
        Mutex<super::channel_projection_cache::ChannelProjectionCache>,
    pub(in crate::node) raw_group_views: Mutex<super::account_group::RawViewCache>,
    pub(in crate::node) group_metadata_cache: Mutex<super::group_metadata::MetadataCache>,
    #[cfg(test)]
    pub(in crate::node) channel_projection_replays: std::sync::atomic::AtomicUsize,
    /// Fixed read-only summaries recovered from previous devices at open time.
    pub(in crate::node) archived_channels: Vec<ChannelSummary>,
    /// Public author of the device that produced a restored local history.
    /// Never contains the retired device's private key.
    pub(in crate::node) recovery_origins: Vec<crate::eventlog::Author>,
    pub(in crate::node) recovery_contacts: Vec<String>,
    pub(in crate::node) recovered_sent_ids:
        Mutex<Option<super::recovered_sent_ids::IndexedRecoveredSentIds>>,
    /// Durable, encrypted store of this node's per-channel sending keys (`my_sender`).
    /// `my_sender` is in-memory only and rebuilt empty on restart; persisting it here
    /// lets us resume our sending chains so receivers (first-wins on chains they hold)
    /// can still decrypt our post-restart messages.
    pub(in crate::node) channel_migration:
        Mutex<super::channel_account_maintenance::ChannelMigrationStore>,
    pub(in crate::node) channel_senders: Mutex<ChannelSenderStore>,
    pub(in crate::node) channel_checkpoints: super::channel::checkpoint::ChannelCheckpointStore,
    /// Last confirmed durable holders of our own channel events.
    pub(in crate::node) channel_custody: Mutex<ChannelCustodyStore>,
    #[cfg(test)]
    pub(in crate::node) channel_last_send: Mutex<Option<(usize, u64)>>,
    pub(in crate::node) file_incoming: super::runtime_delivery::HostPayload<ReceivedFile>,
    pub(in crate::node) files: Mutex<IndexedFileBook>,
    pub(in crate::node) manifest_scopes: super::filebook::scopes::IndexedManifestScopes,
    pub(super) group_file_sharing: super::network_cost::GroupFileSharingPolicy,
    /// Coalesced hint for the durable download work owned by `files`.
    pub(in crate::node) file_pull_notify: tokio::sync::Notify,
    pub(in crate::node) file_pull_cursor: std::sync::atomic::AtomicUsize,
    pub(in crate::node) file_pull_gate: tokio::sync::Mutex<()>,
    pub(super) group_file_delivery: super::group_file_delivery::GroupFileDelivery,
    pub(in crate::node) relay_retry_after: Mutex<HashMap<String, std::time::Instant>>,
    pub(in crate::node) relay_drain_cursor: Mutex<HashMap<String, usize>>,
    pub(in crate::node) ui_changes: tokio::sync::watch::Sender<u64>,
    pub(in crate::node) file_changes: tokio::sync::watch::Sender<u64>,
    /// Encrypted, account-local attachment receive preferences and exact offer decisions.
    pub(in crate::node) attachment_consent:
        Mutex<super::attachment_consent::IndexedAttachmentConsent>,
    /// Durable pairwise file-consent controls, separate from local receive policy.
    pub(in crate::node) file_consent: Mutex<super::file_consent_store::FileConsentStore>,
    /// Durable chat-media store under `<account_dir>/media`: the plaintext bytes of
    /// image/screenshot/video messages, copied here on send + receive-complete so the
    /// inline preview loads from a durable source that survives chunk prune + restart.
    /// SEPARATE from the chunked attachment path (generic files are never written here).
    pub(in crate::node) media: crate::node::media_store::MediaStore,
    pub(super) sticker_sources: Mutex<super::sticker_media::StickerSources>,
    /// Per-peer Double Ratchet sessions (forward-secret DM crypto), encrypted on disk.
    pub(in crate::node) dm_ratchet: Mutex<DmRatchet>,
    /// Serializes every DM ratchet consumer and ordered recovery.
    pub(in crate::node) delivery: Mutex<super::delivery_store::DeliveryStore>,
    pub(in crate::node) delivery_observations:
        Mutex<super::delivery_observation::DeliveryObservations>,
    /// Transient recipient Have counts for active outgoing file destinations.
    pub(in crate::node) outgoing_file_acks: Mutex<HashMap<(EventId, EventId), u32>>,
    /// Last authenticated recipient answer for each outgoing manifest destination.
    pub(in crate::node) outgoing_file_consents:
        Mutex<HashMap<(EventId, EventId), crate::eventlog::sync::FileConsentStatus>>,
    pub(in crate::node) channel_changes: tokio::sync::broadcast::Sender<ConversationId>,
    pub(in crate::node) channel_recovery_pending: Mutex<HashSet<ConversationId>>,
    pub(in crate::node) join_history_work: Mutex<super::join_history::indexed::HistoryWork>,
    pub(in crate::node) channel_recovery_notify: tokio::sync::Notify,
    pub(in crate::node) channel_recovery:
        Mutex<HashMap<ConversationId, super::channels::ChannelRecovery>>,
    pub(in crate::node) delivery_notify: tokio::sync::Notify,
    /// Single receive producer owns this retained mailbox-policy wake.
    pub(in crate::node) post_office_receive_notify: tokio::sync::Notify,
    /// Local interface changes invalidate in-flight work without retiring identity or queues.
    pub(in crate::node) network_changes: tokio::sync::watch::Sender<u64>,
    pub(in crate::node) delivery_status_notify: tokio::sync::Notify,
    /// Exact device fingerprints the host user has safety-number verified for
    /// recipient-initiated DM catch-up. Discovery alone never adds entries.
    pub(in crate::node) verified_dm_pull_peers: Mutex<HashSet<UserId>>,
    /// Ephemeral identities introduced by an explicitly enabled bridge relay.
    pub(in crate::node) bridge_dm_pull_peers: Mutex<HashMap<UserId, std::time::Instant>>,
    pub(in crate::node) physical_lan_only: std::sync::atomic::AtomicBool,
    pub(in crate::node) bridge_discovery_enabled: std::sync::atomic::AtomicBool,
    pub(in crate::node) origin_sharing_enabled: std::sync::atomic::AtomicBool,
    pub(in crate::node) bridge_hub_id: Mutex<Option<String>>,
    pub(in crate::node) bridge_notify: tokio::sync::Notify,
    pub(in crate::node) bridge_refresh:
        tokio::sync::Mutex<super::bridge_client::BridgeRefreshState>,
    pub(in crate::node) bridge_generation: std::sync::atomic::AtomicU64,
    pub(in crate::node) delivery_control_ids: Mutex<super::delivery_store::DeliveryControlIndex>,
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
    pub(in crate::node) recalls: Mutex<crate::node::recall::IndexedRecallLog>,
    /// Owns profile journals, local publication and coordinated account transitions.
    pub(in crate::node) trust_cards: Mutex<super::trust_card::IndexedTrustCardStore>,
    pub(in crate::node) profiles: Mutex<super::account_profile_data::IndexedAccountProfileStore>,
    pub(in crate::node) account_profiles: super::account_profile::AccountProfiles,
    /// Live stream of received peer profiles (avatar updates), surfaced to the app. The
    /// node owns BOTH ends (created in `open_with_account`) so the constructor signature
    /// is unchanged across its many call sites; the runtime claims the receiver once via
    /// [`Node::take_profile_receiver`].
    pub(in crate::node) profile_incoming: super::runtime_delivery::HostPayload<ReceivedProfile>,
    pub(in crate::node) profile_rx: Mutex<Option<mpsc::UnboundedReceiver<ReceivedProfile>>>,
    /// Live stream of inbound call signals (WebRTC SDP/bye), surfaced to the app. Like
    /// `profile_incoming`, the node owns BOTH ends (created in `open_with_account`) so the
    /// constructor signature is unchanged; the runtime claims the receiver once via
    /// [`Node::take_call_signal_receiver`]. Ephemeral — never touches the event log.
    pub(in crate::node) call_signal_incoming: super::runtime_delivery::CallInbox,
    pub(in crate::node) call_signal_rx:
        Mutex<Option<mpsc::UnboundedReceiver<crate::node::call::ReceivedCallSignal>>>,
    /// Exact profile receive facts and bounded metadata discovery/retry.
    pub(in crate::node) profile_receive_work:
        Mutex<super::profile_receive_work::ProfileReceiveWork>,
    /// Durable latest local actions; outbound sealed reactions cannot be reopened by us.
    pub(in crate::node) reaction_queries: Mutex<super::reactions::ReactionQueryWork>,
    pub(in crate::node) my_dm_reactions: Mutex<super::reaction_store::IndexedReactionStore>,
    // Declared last so every legacy handle drops before the connection and lease.
    pub(in crate::node) storage: Arc<crate::storage::profile::context::ProfileStorage>,
}

/// Explicit participant inputs for a caller that already owns the profile lease.
/// Exact event and sent paths are preserved, including external sent roots.
pub struct NodeOpenRequest<'a> {
    pub identity: DeviceIdentity,
    pub account: Account,
    pub roster: Arc<Mutex<Roster>>,
    pub incoming: mpsc::UnboundedSender<ReceivedDm>,
    pub channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
    pub file_incoming: mpsc::UnboundedSender<ReceivedFile>,
    pub log_path: &'a Path,
    pub sent_path: &'a Path,
    pub password: &'a str,
}

/// Production hosts observe durable reception/current state on demand. This
/// constructor has no caller-owned payload queues; legacy constructors remain
/// live-only SDK collecting APIs with caller-owned unbounded delivery.
pub struct BoundedNodeOpenRequest<'a> {
    pub identity: DeviceIdentity,
    pub account: Account,
    pub roster: Arc<Mutex<Roster>>,
    pub log_path: &'a Path,
    pub sent_path: &'a Path,
    pub password: &'a str,
}

pub(crate) struct LegacyPayloadTargets {
    pub incoming: mpsc::UnboundedSender<ReceivedDm>,
    pub channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
    pub file_incoming: mpsc::UnboundedSender<ReceivedFile>,
}

impl Node {
    /// Close every legacy handle and the encrypted connection before directory
    /// relocation. Independently retained Node/facade owners prevent publication.
    pub fn close_profile(self: Arc<Self>) -> Result<(), LogError> {
        let node = Arc::try_unwrap(self).map_err(|_| {
            LogError::Io(std::io::Error::other(
                "profile still has retained Node handles",
            ))
        })?;
        let storage = node.storage.clone();
        drop(node);
        Arc::try_unwrap(storage)
            .map_err(|_| {
                LogError::Io(std::io::Error::other(
                    "profile still has retained storage work",
                ))
            })?
            .close()
    }

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
        crate::storage::profile::participant::validate_authority_leaves(log_path, sent_path)?;
        let dir = log_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let lease = crate::storage::profile::ProfileStorageLease::acquire(dir)?;
        Self::open_with_profile_lease(
            lease,
            NodeOpenRequest {
                identity,
                account,
                roster,
                incoming,
                channel_incoming,
                file_incoming,
                log_path,
                sent_path,
                password,
            },
        )
    }

    /// Consume a pre-keystore owner once. Compatibility constructors acquire
    /// their own lease and never bypass exclusion for the current process.
    pub fn open_with_profile_lease(
        lease: crate::storage::profile::ProfileStorageLease,
        request: NodeOpenRequest<'_>,
    ) -> Result<Arc<Self>, LogError> {
        let storage = crate::storage::profile::context::ProfileStorage::open_participant(
            lease,
            &request.identity.public(),
            &request.account.public(),
            request.log_path,
            request.sent_path,
            request.password,
        )?;
        Self::open_in_storage(storage, request)
    }

    pub fn open_bounded_with_profile_lease(
        lease: crate::storage::profile::ProfileStorageLease,
        request: BoundedNodeOpenRequest<'_>,
    ) -> Result<Arc<Self>, LogError> {
        let storage = crate::storage::profile::context::ProfileStorage::open_participant(
            lease,
            &request.identity.public(),
            &request.account.public(),
            request.log_path,
            request.sent_path,
            request.password,
        )?;
        Self::open_in_storage_delivery(storage, request, None)
    }

    pub(crate) fn open_in_storage(
        storage: Arc<crate::storage::profile::context::ProfileStorage>,
        request: NodeOpenRequest<'_>,
    ) -> Result<Arc<Self>, LogError> {
        let NodeOpenRequest {
            identity,
            account,
            roster,
            incoming,
            channel_incoming,
            file_incoming,
            log_path,
            sent_path,
            password,
        } = request;
        Self::open_in_storage_delivery(
            storage,
            BoundedNodeOpenRequest {
                identity,
                account,
                roster,
                log_path,
                sent_path,
                password,
            },
            Some(LegacyPayloadTargets {
                incoming,
                channel_incoming,
                file_incoming,
            }),
        )
    }

    pub(crate) fn open_in_storage_delivery(
        storage: Arc<crate::storage::profile::context::ProfileStorage>,
        request: BoundedNodeOpenRequest<'_>,
        targets: Option<LegacyPayloadTargets>,
    ) -> Result<Arc<Self>, LogError> {
        let BoundedNodeOpenRequest {
            identity,
            account,
            roster,
            log_path,
            sent_path,
            password,
        } = request;
        let legacy_delivery = targets.is_some();
        let ui_changes = tokio::sync::watch::channel(0).0;
        let (incoming, channel_incoming, file_incoming) = match targets {
            Some(targets) => (
                super::runtime_delivery::HostPayload::legacy(targets.incoming, ui_changes.clone()),
                super::runtime_delivery::HostPayload::legacy(
                    targets.channel_incoming,
                    ui_changes.clone(),
                ),
                super::runtime_delivery::HostPayload::legacy(
                    targets.file_incoming,
                    ui_changes.clone(),
                ),
            ),
            None => (
                super::runtime_delivery::HostPayload::projection(ui_changes.clone()),
                super::runtime_delivery::HostPayload::projection(ui_changes.clone()),
                super::runtime_delivery::HostPayload::projection(ui_changes.clone()),
            ),
        };
        storage.validate_participant_request(
            &identity.public(),
            &account.public(),
            log_path,
            sent_path,
        )?;
        let dir = log_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        // Each store does its own slow, CPU-bound password KDF over an independent
        // file. Open them on scoped threads so the KDFs run in parallel: on a
        // multi-core machine the wall-clock cost collapses to roughly one KDF.
        let (
            sent_res,
            sessions_res,
            received_res,
            csenders_res,
            custody_res,
            recv_files_res,
            profiles_res,
            recalls_res,
            consent_res,
            consent_controls_res,
            origin_res,
            contacts_res,
            recovered_ids_res,
        ) = std::thread::scope(|s| {
            let h_sent = s.spawn(|| super::transcript_repository::publication::PublishedHistorySource::authenticate_legacy::<super::sentlog::SentEntry>(&storage,sent_path,password,super::transcript_repository::SENT,b"MTSENT"));
            let h_sess = s.spawn(|| RatchetSessions::open(&dir.join("ratchet.sessions"), password));
            let h_recv = s.spawn(|| super::transcript_repository::publication::PublishedHistorySource::authenticate_legacy::<super::received_log::ReceivedEntry>(&storage,&dir.join("received.log"),password,super::transcript_repository::RECEIVED,b"MTRECV"));
            let h_csnd = s.spawn(|| {
                ChannelSenderStore::authenticate_legacy(
                    &storage,
                    &dir.join("channel.senders"),
                    password,
                )
            });
            let h_custody = s.spawn(|| {
                ChannelCustodyStore::authenticate_legacy(
                    &storage,
                    &dir.join("channel.custody"),
                    password,
                )
            });
            // Durable record of surfaced file manifests (reuses the ReceivedLog format),
            // so the file book's emitted set survives restart WITHOUT marking unopened
            // manifests as emitted — see the seeding below.
            let h_files =
                s.spawn(|| super::transcript_repository::publication::PublishedHistorySource::authenticate_legacy::<super::received_log::ReceivedEntry>(&storage,&dir.join("received_files.log"),password,super::transcript_repository::FILES,b"MTRECV"));
            // Durable store of avatars peers propagated to us (one per account, newest-wins).
            let h_profiles = s.spawn(|| {
                crate::node::profile_store::IndexedProfileStore::authenticate_legacy(
                    &storage,
                    &dir.join("profiles.log"),
                    password,
                )
            });
            // Durable record of OUR OWN recalls: our recall payloads are sealed to peers (we
            // can't open them from our own log), so this is how `history` knows which of our
            // messages we recalled — and it must survive restart.
            let h_recalls = s
                .spawn(|| super::transcript_repository::publication::PublishedHistorySource::authenticate_legacy::<super::recall::RecallMark>(&storage,&dir.join("recalls.log"),password,super::transcript_repository::RECALL_MARKS,b"MTRECL"));
            let h_consent = s.spawn(|| {
                super::attachment_consent::IndexedAttachmentConsent::authenticate_legacy(
                    &storage,
                    &dir.join("attachment-consent.log"),
                    password,
                )
            });
            let h_consent_controls = s.spawn(|| {
                super::file_consent_store::FileConsentStore::authenticate_legacy(
                    &storage,
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
                super::recovered_sent_ids::IndexedRecoveredSentIds::authenticate_legacy(
                    &storage, &path, password,
                )
            });
            (
                h_sent.join(),
                h_sess.join(),
                h_recv.join(),
                h_csnd.join(),
                h_custody.join(),
                h_files.join(),
                h_profiles.join(),
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
        sent_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let sessions = sessions_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        received_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        csenders_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        custody_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        recv_files_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        profiles_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        super::trust_card::IndexedTrustCardStore::authenticate_legacy(
            &storage,
            &dir.join("trust-cards.log"),
            password,
        )?;
        super::account_profile_data::IndexedAccountProfileStore::authenticate_legacy(
            &storage,
            &dir.join("account-profiles.log"),
            password,
        )?;
        recalls_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        consent_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        consent_controls_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let recovery_origins = origin_res
            .unwrap_or_else(|e| std::panic::resume_unwind(e))?
            .into_iter()
            .map(crate::eventlog::Author::from_ed25519)
            .collect();
        let recovery_contacts = contacts_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        recovered_ids_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let mut dm_ratchet = DmRatchet::new(sessions);
        let mut group_operations =
            super::account_group::operations::journal::OperationJournal::open(
                dir,
                password,
                account.public().ed25519_pub,
                identity.public(),
            )?;
        let mut delivery = super::delivery_store::DeliveryStore::open_for_log(log_path, password)?;
        let previous_account = storage
            .participant_database()
            .map(|database| {
                crate::storage::profile::participant::previous_account(database, &account.public())
            })
            .transpose()?
            .flatten();
        let legacy_previous = delivery.bind_profile_authorized(
            log_path,
            password,
            &identity.public(),
            &account.account_id(),
            previous_account.as_deref(),
            storage.participant_database().is_none(),
        )?;
        delivery.validate_owner(&identity.public(), &account.account_id())?;
        storage.validate_legacy_events(password)?;
        // Authenticate every remaining legacy owner before first SQL source
        // publication. Keep these handles for recovery instead of opening twice.
        let mut account_devices = super::account_devices::AccountDevices::open(
            &super::account_devices::file_path(dir, &account.account_id()),
            password,
            &account.account_id(),
        )?;
        account_devices.insert(super::account_devices::AccountDevice {
            public: identity.public(),
            certificate: account.certify(&identity.public().ed25519_pub),
        })?;
        super::account_suppression::IndexedAccountSuppression::authenticate_legacy(
            &storage,
            &dir.join("account-suppression.log"),
            password,
        )?;
        super::account_file_repository::IndexedAccountFileProofs::authenticate(
            &storage,
            &dir.join("account-file-proofs.log"),
            password,
        )?;
        super::file_message_identity::IndexedFileMessageIdentities::authenticate(
            &storage,
            &dir.join("file-message-identities.log"),
            password,
        )?;
        super::filebook::scopes::IndexedManifestScopes::authenticate(
            &storage,
            &dir.join("sent-manifest-scopes.log"),
            password,
        )?;
        let forwarding = super::forwarding::ForwardStore::open(
            &dir.join(format!("forwarding-{}.log", account.account_id())),
            password,
        )?;
        let favorites = super::favorites::FavoriteStore::open(
            &dir.join(format!("favorites-{}.log", account.account_id())),
            password,
        )?;
        let account_sync_policy = super::account_sync_policy::DeviceSyncPolicyStore::open(
            &dir.join(format!("account-sync-policy-{}.log", account.account_id())),
            password,
        )?;
        let account_preferences = super::account_preferences::AccountPreferences::open_for_account(
            dir,
            password,
            &account.account_id(),
        )?;
        let account_access = super::account_access::AccountAccessStore::open(
            &dir.join("account-access.log"),
            password,
        )?;
        let account_access_updates =
            tokio::sync::watch::channel(account_access.current(&account.account_id())).0;
        let reaction_path =
            log_path.with_extension(format!("{}-reactions.log", account.account_id()));
        super::reaction_store::IndexedReactionStore::authenticate_legacy(
            &storage,
            &reaction_path,
            password,
            account.public().ed25519_pub,
        )?;
        let channel_migration = super::channel_account_maintenance::ChannelMigrationStore::open(
            &dir.join(format!("channel-migration-{}.log", account.account_id())),
            password,
            account.public().ed25519_pub,
            &identity.public(),
        )?;
        let mut account_transitions =
            super::account_transition_store::AccountTransitionStore::open(
                &dir.join("account-binding-transitions.log"),
                password,
            )?;
        account_transitions.commit_own(&identity.public(), &account.public().ed25519_pub)?;
        account_transitions.discard_pending(&identity.public(), &account.public().ed25519_pub)?;
        let account_document_updates = tokio::sync::watch::channel(0).0;
        storage.complete_participant(password, legacy_previous.as_deref())?;
        let media = super::media_store::MediaStore::open_profile(
            storage.clone(),
            dir,
            password,
            &identity.user_id(),
            None,
        )
        .map_err(LogError::Io)?;
        let favorite_media = super::media_store::MediaStore::open_profile(
            storage.clone(),
            &dir.join(format!("favorites-{}", account.account_id())),
            password,
            &account.account_id(),
            None,
        )
        .map_err(LogError::Io)?;
        let account_documents =
            super::account_documents::store::AccountDocumentStore::open_profile(
                storage.clone(),
                dir,
                password,
                &account.public().ed25519_pub,
            )?;

        let channel_custody = ChannelCustodyStore::from_profile(
            storage.clone(),
            &dir.join("channel.custody"),
            password,
        )?;
        let my_dm_reactions = super::reaction_store::IndexedReactionStore::from_profile(
            storage.clone(),
            &reaction_path,
            password,
            account.public().ed25519_pub,
        )?;
        let avatars = super::profile_store::IndexedProfileStore::from_profile(
            storage.clone(),
            &dir.join("profiles.log"),
            password,
        )?;
        let account_profiles = super::account_profile::AccountProfiles::new(&account, avatars)?;
        let trust_cards = Mutex::new(super::trust_card::IndexedTrustCardStore::from_profile(
            storage.clone(),
            &dir.join("trust-cards.log"),
            password,
        )?);
        let profiles = Mutex::new(
            super::account_profile_data::IndexedAccountProfileStore::from_profile(
                storage.clone(),
                &dir.join("account-profiles.log"),
                password,
            )?,
        );

        let profile_receive_work =
            super::profile_receive_work::ProfileReceiveWork::open(storage.clone())?;
        let reception_effects = super::reception_effects::ReceptionEffects::open(storage.clone())?;
        let account_file_proofs =
            super::account_file_repository::IndexedAccountFileProofs::from_profile(
                storage.clone(),
                &dir.join("account-file-proofs.log"),
                password,
            )?;
        let file_message_identities =
            super::file_message_identity::IndexedFileMessageIdentities::from_profile(
                storage.clone(),
                &dir.join("file-message-identities.log"),
                password,
            )?;
        let mut sentlog = SentLog::from_profile(storage.clone(), sent_path, password)?;
        let mut received =
            ReceivedLog::from_profile(storage.clone(), &dir.join("received.log"), password)?;
        let mut received_files = ReceivedLog::file_cards_from_profile(
            storage.clone(),
            &dir.join("received_files.log"),
            password,
        )?;
        let account_suppression =
            super::account_suppression::IndexedAccountSuppression::from_profile(
                storage.clone(),
                &dir.join("account-suppression.log"),
                password,
            )?;
        let mut recalls = super::recall::IndexedRecallLog::from_profile(
            storage.clone(),
            &dir.join("recalls.log"),
            password,
        )?;
        let recovered_sent_ids = Some(
            super::recovered_sent_ids::IndexedRecoveredSentIds::from_profile(
                storage.clone(),
                &dir.join("recovery-sent-ids.log"),
                password,
            )?,
        );
        let mut channel_senders = ChannelSenderStore::from_profile(
            storage.clone(),
            &dir.join("channel.senders"),
            password,
        )?;
        let mut log = PersistentEventLog::open_profile(storage.clone(), password)?;
        let manifest_scopes = super::filebook::scopes::IndexedManifestScopes::from_profile(
            storage.clone(),
            &dir.join("sent-manifest-scopes.log"),
            password,
            log.repository()?,
            crate::eventlog::Author::from_ed25519(identity.public().ed25519_pub),
        )?;
        // Derived file eligibility must exist before a manifest WAL intent can
        // install its body, exact evidence and reception effect atomically.
        let files = IndexedFileBook::open(storage.clone())?;
        let attachment_consent = super::attachment_consent::IndexedAttachmentConsent::from_profile(
            storage.clone(),
            &dir.join("attachment-consent.log"),
            password,
        )?;
        let file_consent = super::file_consent_store::FileConsentStore::from_profile(
            storage.clone(),
            &dir.join("file-consent-controls.log"),
            password,
        )?;
        log.repository()?.initialize_ancestry()?;
        let channel_checkpoints =
            super::channel::checkpoint::ChannelCheckpointStore::open(storage.clone())?;
        loop {
            if let Some(tx) = delivery.pending_transactions().first() {
                delivery.validate_message_action_transaction(tx, &log, &identity)?;
            }
            let recall = delivery
                .pending_transactions()
                .first()
                .and_then(|tx| match tx.body() {
                    super::delivery_store::DeliveryTransaction::ChannelMessageAction {
                        recall,
                        ..
                    } => recall.clone(),
                    _ => None,
                });
            if delivery
                .recover_next_with_group_operations(
                    &mut dm_ratchet,
                    &mut log,
                    &mut sentlog,
                    &mut received,
                    Some(&mut received_files),
                    Some(&mut channel_senders),
                    Some(&mut |reference, events| {
                        super::account_group::operations::persist_action_recall_completion(
                            reference,
                            recall.as_ref(),
                            &mut recalls,
                        )?;
                        group_operations.complete(reference, events)
                    }),
                )?
                .is_none()
            {
                break;
            }
        }
        for (conversation, id) in delivery.file_erasures() {
            received_files.remove_exact(conversation, id)?;
        }
        let delivery_control_ids = delivery.control_ids();
        // Startup restores only last validated current presentation after the
        // account adoption commit below. Historical authority/receiver work is
        // resumed by the existing bounded channel recovery loop.
        let channels = ChannelBook::new();
        let archived_channels = Vec::new();
        super::files::import_file_book(
            &files,
            &received_files,
            &log,
            &account_file_proofs,
            &delivery,
            &file_message_identities,
            &account_suppression,
        )?;
        // The existing FILES reference owner is consulted in bounded background
        // turns; warm startup only schedules a persisted inventory cutoff.
        media.schedule_gc().map_err(LogError::Io)?;
        let (profile_tx, profile_rx, call_signal_tx, call_signal_rx) = if legacy_delivery {
            let (profile_tx, profile_rx) = mpsc::unbounded_channel();
            let (call_tx, call_rx) = mpsc::unbounded_channel();
            (
                super::runtime_delivery::HostPayload::legacy(profile_tx, ui_changes.clone()),
                Some(profile_rx),
                super::runtime_delivery::CallInbox::legacy(call_tx),
                Some(call_rx),
            )
        } else {
            (
                super::runtime_delivery::HostPayload::projection(ui_changes.clone()),
                None,
                super::runtime_delivery::CallInbox::bounded(ui_changes.clone()),
                None,
            )
        };
        favorite_media.schedule_gc().map_err(LogError::Io)?;
        let opaque_receive_cache = super::opaque_receive_cache::OpaqueReceiveCache::open(
            storage.clone(),
            log_path,
            password,
            &identity.public(),
            &account.account_id(),
        );
        let privacy = Arc::new(super::privacy_runtime::PrivacyControl::with_storage(
            storage.clone(),
        ));
        let mut node = Self {
            storage,
            expiry_clock_start: (std::time::Instant::now(), now_millis()),
            expiry_first_seen: Mutex::new(HashMap::new()),
            runtime_work: Arc::new(super::runtime_work::RuntimeWork::default()),
            #[cfg(test)]
            control_fanout_start_test_hook: Mutex::new(None),
            #[cfg(test)]
            group_file_asset_recv_test_hook: Mutex::new(None),
            #[cfg(test)]
            delivery_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            withheld_file_scopes: Mutex::new(HashSet::new()),
            #[cfg(test)]
            untracked_chunk_validations: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            before_sent_delete_hook: Mutex::new(None),
            #[cfg(test)]
            profile_read_hook: Mutex::new(None),
            #[cfg(test)]
            profile_prepare_hook: Mutex::new(None),
            #[cfg(test)]
            peer_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            expiry_cleanup_hook: Mutex::new(None),
            #[cfg(test)]
            remember_peer_hook: Mutex::new(None),
            #[cfg(test)]
            file_transfer_peak: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            empty_file_cache_self_wakes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            accepted_hook: Mutex::new(None),
            #[cfg(test)]
            account_session_test_hook: Mutex::new(None),
            #[cfg(test)]
            account_client_test_hook: Mutex::new(None),
            #[cfg(test)]
            account_channel_maintenance_test_hook: Mutex::new(None),
            privacy,
            identity,
            account,
            account_devices: Mutex::new(account_devices),
            account_transitions: Mutex::new(account_transitions),
            account_documents: Mutex::new(account_documents),
            group_file_prepare_gate: Arc::new(tokio::sync::Mutex::new(())),
            group_operations: Mutex::new(group_operations),
            account_document_updates,
            account_group_versions: Mutex::new(HashMap::new()),
            group_operation_versions: Mutex::new(HashMap::new()),
            operation_epoch: rand_core::RngCore::next_u64(&mut rand_core::OsRng),
            operation_queries: Mutex::new(HashMap::new()),
            operation_seen: Mutex::new(HashMap::new()),
            account_suppression: Mutex::new(account_suppression),
            favorites: Mutex::new(favorites),
            favorite_media,
            account_sync_policy: Mutex::new(account_sync_policy),
            account_preferences: Mutex::new(account_preferences),
            account_access: Mutex::new(account_access),
            account_access_updates,
            account_file_proofs: Mutex::new(account_file_proofs),
            file_message_identities: Mutex::new(file_message_identities),
            file_message_identity_cursor: std::sync::atomic::AtomicUsize::new(0),
            account_sync_notify: tokio::sync::Notify::new(),
            account_history_revision: std::sync::atomic::AtomicU64::new(0),
            account_archive_cache: Mutex::new(Default::default()),
            account_sync_commit: Mutex::new(()),
            account_wake: Default::default(),
            account_sync_status: Mutex::new(std::collections::HashMap::new()),
            account_profiles,
            trust_cards,
            profiles,
            profile_incoming: profile_tx,
            profile_rx: Mutex::new(profile_rx),
            call_signal_incoming: call_signal_tx,
            call_signal_rx: Mutex::new(call_signal_rx),
            profile_receive_work: Mutex::new(profile_receive_work),
            pending_link: Mutex::new(None),
            log: Mutex::new(log),
            sentlog: Mutex::new(sentlog),
            roster,
            incoming,
            channel_incoming,
            reception_effects,
            channel_membership_commit: Mutex::new(()),
            channels: Mutex::new(channels),
            pending_channel_plaintext: Mutex::new(Default::default()),
            pending_channel_files: Mutex::new(None),
            archived_channels,
            recovery_origins,
            recovery_contacts,
            recovered_sent_ids: Mutex::new(recovered_sent_ids),
            forwarding: Mutex::new(forwarding),
            forwarding_gate: tokio::sync::Mutex::new(()),
            outgoing_requests_gate: tokio::sync::Mutex::new(()),
            channel_migration: Mutex::new(channel_migration),
            channel_senders: Mutex::new(channel_senders),
            channel_checkpoints,
            channel_custody: Mutex::new(channel_custody),
            channel_projection_cache: Mutex::new(Default::default()),
            raw_group_views: Mutex::new(Default::default()),
            group_metadata_cache: Mutex::new(Default::default()),
            #[cfg(test)]
            channel_projection_replays: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            channel_last_send: Mutex::new(None),
            file_incoming,
            files: Mutex::new(files),
            manifest_scopes,
            group_file_sharing: Default::default(),
            file_pull_notify: tokio::sync::Notify::new(),
            file_pull_cursor: std::sync::atomic::AtomicUsize::new(0),
            file_pull_gate: tokio::sync::Mutex::new(()),
            group_file_delivery: Default::default(),
            relay_retry_after: Mutex::new(HashMap::new()),
            relay_drain_cursor: Mutex::new(HashMap::new()),
            ui_changes,
            file_changes: tokio::sync::watch::channel(0).0,
            attachment_consent: Mutex::new(attachment_consent),
            file_consent: Mutex::new(file_consent),
            media,
            sticker_sources: Mutex::new(Default::default()),
            dm_ratchet: Mutex::new(dm_ratchet),
            delivery: Mutex::new(delivery),
            delivery_observations: Mutex::new(Default::default()),
            outgoing_file_acks: Mutex::new(HashMap::new()),
            outgoing_file_consents: Mutex::new(HashMap::new()),
            channel_changes: tokio::sync::broadcast::channel(256).0,
            channel_recovery_pending: Mutex::new(HashSet::new()),
            join_history_work: Mutex::new(Default::default()),
            channel_recovery_notify: tokio::sync::Notify::new(),
            channel_recovery: Mutex::new(HashMap::new()),
            delivery_notify: tokio::sync::Notify::new(),
            post_office_receive_notify: tokio::sync::Notify::new(),
            network_changes: tokio::sync::watch::channel(0).0,
            delivery_status_notify: tokio::sync::Notify::new(),
            verified_dm_pull_peers: Mutex::new(HashSet::new()),
            bridge_dm_pull_peers: Mutex::new(HashMap::new()),
            physical_lan_only: std::sync::atomic::AtomicBool::new(false),
            bridge_discovery_enabled: std::sync::atomic::AtomicBool::new(false),
            origin_sharing_enabled: std::sync::atomic::AtomicBool::new(false),
            bridge_hub_id: Mutex::new(None),
            bridge_notify: tokio::sync::Notify::new(),
            bridge_refresh: tokio::sync::Mutex::new(Default::default()),
            sessions: super::session_pool::SessionPool::default(),
            bridge_generation: std::sync::atomic::AtomicU64::new(0),
            delivery_control_ids: Mutex::new(super::delivery_store::DeliveryControlIndex::new(
                delivery_control_ids,
            )),
            delivery_suspended: std::sync::atomic::AtomicBool::new(false),
            post_office_enabled: std::sync::atomic::AtomicBool::new(true),
            post_office_send_enabled: std::sync::atomic::AtomicBool::new(true),
            post_office_receive_enabled: std::sync::atomic::AtomicBool::new(true),
            opaque_receive_cache: Mutex::new(opaque_receive_cache),
            preferred_routes: Mutex::new(HashMap::new()),
            received: Mutex::new(received),
            received_files: Mutex::new(received_files),
            recalls: Mutex::new(recalls),
            reaction_queries: Mutex::default(),
            my_dm_reactions: Mutex::new(my_dm_reactions),
        };
        node.reconcile_account_profiles_on_open().map_err(|error| {
            LogError::CorruptFile(format!("account profile projection: {error}"))
        })?;
        if let Err(error) = node.project_public_profile_documents() {
            log::debug!("contact profile projection deferred: {error}");
        }
        if let Err(error) = node.publish_public_profile_documents() {
            log::debug!("contact profile publication deferred: {error}");
        }
        node.prune_expired_received_on_open()
            .map_err(|error| LogError::CorruptFile(format!("expiry cleanup: {error}")))?;
        if node.retry_conversation_personal_cleanup().is_err() {
            log::debug!("personal conversation cleanup remains pending");
        }
        node.storage
            .complete_participant(password, legacy_previous.as_deref())?;
        node.delivery
            .lock()
            .expect("delivery lock not poisoned")
            .finish_profile_binding()?;
        crate::storage::profile::publication::cut("participant-after-legacy-binding")?;
        crate::storage::profile::participant::finish_adoption(
            node.storage.database(),
            &node.account.public(),
        )?;
        crate::storage::profile::publication::cut("participant-after-adoption-clear")?;
        // Schema was initialized before WAL recovery. Capture the final account
        // only after the existing authenticated adoption owner has committed.
        node.reception_effects =
            super::reception_effects::ReceptionEffects::open(node.storage.clone())?;
        node.channel_checkpoints =
            super::channel::checkpoint::ChannelCheckpointStore::open(node.storage.clone())?;
        node.received
            .lock()
            .expect("received lock")
            .recapture_reception_owner(&node.account.public())?;
        node.received_files
            .lock()
            .expect("received files lock")
            .recapture_reception_owner(&node.account.public())?;
        node.profile_receive_work = Mutex::new(
            super::profile_receive_work::ProfileReceiveWork::open(node.storage.clone())?,
        );
        node.files = Mutex::new(IndexedFileBook::open(node.storage.clone())?);
        node.attachment_consent
            .lock()
            .expect("consent lock")
            .recapture()?;
        node.file_consent
            .lock()
            .expect("file consent lock")
            .recapture()?;
        node.manifest_scopes.recapture()?;
        node.queue_approved_incomplete_files();
        node.channel_checkpoints.begin_runtime_reopen()?;
        {
            let mut book = node.channels.lock().expect("channels lock");
            let mut after = None;
            while let Some(channel) = node.channel_checkpoints.restore_presentation_page(
                &mut book,
                &node.user_id(),
                after,
            )? {
                after = Some(channel);
                if let Some(state) = book.state_mut(&channel)
                    && let Some(bytes) = node
                        .channel_senders
                        .lock()
                        .expect("sender store lock")
                        .current(&channel, state.epoch())?
                {
                    let key = SenderKey::deserialize(&bytes).ok_or_else(|| {
                        LogError::CorruptFile("invalid channel sender checkpoint".into())
                    })?;
                    state.import_my_sender(state.epoch(), key);
                }
            }
        }
        node.archived_channels = archived_channel_summaries(&node.channel_checkpoints)?;
        node.channel_recovery_notify.notify_one();
        node.log
            .lock()
            .expect("log mutex")
            .repository()?
            .initialize_receiving(
                crate::eventlog::Author::from_ed25519(node.identity.public().ed25519_pub),
                &node.account.public(),
            )?;
        node.bootstrap_private_dm_controls().map_err(|error| {
            LogError::CorruptFile(format!("account control projection: {error}"))
        })?;
        Ok(Arc::new(node))
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
    pub fn peer_avatar(&self, account_id: &str) -> Result<Option<Vec<u8>>, LogError> {
        self.account_profiles.avatar(account_id)
    }

    /// Summaries of all channels this node is a member of.
    pub fn list_channels(&self) -> Result<Vec<ChannelSummary>, NodeError> {
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
                    left: !self.channel_identity_matches_account(s),
                    pending_invite: s.pending_invite(),
                })
            })
            .collect();
        let local_ids: std::collections::HashSet<_> = book.channel_ids().into_iter().collect();
        channels.extend(
            self.archived_channels
                .iter()
                .filter(|archive| book.state(&archive.id).is_none())
                .cloned(),
        );
        drop(book);
        for channel in &mut channels {
            let (accounts, unbound) = self.channel_member_account_sets(channel.id)?;
            if !channel.archived || !accounts.is_empty() || !unbound.is_empty() {
                channel.member_count = accounts.len() + unbound.len();
            }
        }
        let mut by_id: std::collections::BTreeMap<_, _> = channels
            .into_iter()
            .map(|channel| (*channel.id.as_bytes(), channel))
            .collect();
        for reference in self.group_inventory_references().into_values() {
            let Ok(bytes) = hex::decode(&reference.channel_id) else {
                continue;
            };
            let Ok(bytes) = <[u8; 32]>::try_from(bytes) else {
                continue;
            };
            let id = ConversationId::new(bytes);
            if !local_ids.contains(&id) {
                by_id.insert(
                    bytes,
                    ChannelSummary {
                        id,
                        name: reference.name,
                        member_count: reference.member_count,
                        owner: reference.owner,
                        archived: true,
                        dissolved: reference.dissolved,
                        left: reference.left,
                        pending_invite: reference.pending_invite,
                    },
                );
            }
        }
        Ok(by_id.into_values().collect())
    }

    /// Owner account authority from the validated group projection.
    pub fn channel_owner_account_id(&self, channel: ConversationId) -> Option<String> {
        self.channels
            .lock()
            .expect("channels mutex not poisoned")
            .state(&channel)
            .and_then(|state| state.owner_account_id().map(str::to_owned))
    }
    pub fn channel_can_manage(&self, channel: ConversationId) -> Result<bool, NodeError> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        let Some(state) = book.state(&channel) else {
            return Ok(false);
        };
        Ok(self.channel_identity_can_manage(state)?)
    }

    pub fn channel_can_send(&self, channel: ConversationId) -> Result<bool, NodeError> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        let Some(state) = book.state(&channel) else {
            return Ok(false);
        };
        Ok(!state.is_dissolved() && self.channel_identity_is_current(state)?)
    }
    pub fn channel_member_account_binding(
        &self,
        channel: ConversationId,
        member: &PublicIdentity,
    ) -> Option<String> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        if let Some(state) = book.state(&channel)
            && state.owner_account_id().is_some()
        {
            return state
                .members()
                .contains(member)
                .then(|| {
                    state
                        .member_account_id(&member.user_id())
                        .map(str::to_owned)
                })
                .flatten();
        }
        drop(book);
        self.account_id_for_device(member)
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

    pub fn channel_activity(
        &self,
        channel: ConversationId,
    ) -> Result<Vec<super::ChannelActivity>, super::NodeError> {
        if self
            .channels
            .lock()
            .expect("channels mutex not poisoned")
            .state(&channel)
            .is_none()
        {
            return Ok(self.shared_group_activity(channel));
        }
        Ok(self
            .channel_activity_page(channel, None, Default::default())?
            .items)
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
        self.append_event_with_profile_parent(channel, kind, ciphertext, None)
            .map(|(seq, _)| seq)
    }

    /// Reuse ordinary allocation while retaining a required signed-profile parent.
    pub(in crate::node) fn append_event_with_profile_parent(
        &self,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
        required_profile_parent: Option<EventId>,
    ) -> Result<(u64, EventId), NodeError> {
        self.append_event_checked(
            channel,
            kind,
            ciphertext,
            required_profile_parent,
            None,
            |_| Ok(()),
        )
    }

    pub(in crate::node) fn append_event_if(
        &self,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
        check: impl FnOnce(&super::delivery_store::DeliveryStore) -> Result<(), NodeError>,
    ) -> Result<u64, NodeError> {
        self.append_event_checked(channel, kind, ciphertext, None, None, check)
            .map(|(seq, _)| seq)
    }

    pub(in crate::node) fn append_channel_control_from_source(
        &self,
        source: &super::channel::checkpoint::ChannelSource,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) -> Result<(), NodeError> {
        self.append_event_checked(source.channel, kind, ciphertext, None, Some(source), |_| {
            Ok(())
        })?;
        Ok(())
    }

    fn append_event_checked(
        &self,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
        required_profile_parent: Option<EventId>,
        source: Option<&super::channel::checkpoint::ChannelSource>,
        check: impl FnOnce(&super::delivery_store::DeliveryStore) -> Result<(), NodeError>,
    ) -> Result<(u64, EventId), NodeError> {
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
        check(&delivery)?;
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut log = self.log.lock().expect("log mutex not poisoned");
        if let Some(source) = source {
            // The allocation lock excludes remote appends until this control
            // is installed. Recheck after pending local WAL recovery as well.
            self.channel_checkpoints.check_source(source)?;
        }
        let (mut parents, lamport) = log.prepare(&channel).map_err(NodeError::Log)?;
        if independent_profile {
            // Signed profile leaves must remain ingestible while sibling content
            // categories are paused. The event log permits sparse author sequences.
            parents.clear();
        }
        if let Some(parent) = required_profile_parent {
            if kind != EventKind::Profile
                || !log.get(&parent)?.is_some_and(|event| {
                    event.kind == EventKind::Profile
                        && event.conversation_id == channel
                        && event.author == self_author
                })
            {
                return Err(NodeError::InvalidInput(
                    "required profile parent is not a local profile in this conversation".into(),
                ));
            }
            // Even independent sibling profiles retain this Profile-only edge. It
            // never depends on paused message/file categories and prevents a legacy
            // avatar projection from overtaking its signed metadata across pages.
            parents.push(parent);
            parents.sort_unstable();
            parents.dedup();
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
        if event.kind == EventKind::ChannelJoinHistory && !super::session::event_fits_frame(&event)
        {
            return Err(NodeError::Channel(
                "history control exceeds transport frame".into(),
            ));
        }
        let id = event.id;
        log.append(event).map_err(NodeError::Log)?;
        Ok((seq, id))
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

#[cfg(test)]
#[path = "participant_storage_tests.rs"]
mod participant_storage_tests;
