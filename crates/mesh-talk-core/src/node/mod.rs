//! The node: the explicit app context that wires identity, discovery, transport, the
//! event log, and DM/group crypto into a runnable peer.
//!
//! The submodules are crate-internal implementation detail; the crate's public SDK
//! surface is exactly the curated re-export block below. Keeping the modules
//! `pub(crate)` (rather than `pub`) means an external consumer's semver contract is the
//! re-exports, not every internal type.

pub(crate) mod channel;
pub(crate) mod channel_senders;
pub(crate) mod channels;
pub(crate) mod conversation;
pub(crate) mod dm;
pub(crate) mod dm_envelope;
pub(crate) mod dm_ratchet;
pub(crate) mod filebook;
pub(crate) mod files;
pub(crate) mod lifecycle;
pub(crate) mod linking;
pub(crate) mod media_store;
pub(crate) mod message;
pub(crate) mod name_directory;
pub(crate) mod node;
pub(crate) mod pairing;
pub(crate) mod postbox;
pub(crate) mod profile;
pub(crate) mod profile_io;
pub(crate) mod profile_store;
pub(crate) mod queries;
pub(crate) mod ratchet_sessions;
pub(crate) mod reaction;
pub(crate) mod reactions;
pub(crate) mod recall;
pub(crate) mod received_log;
pub(crate) mod runtime;
pub(crate) mod sentlog;
pub(crate) mod serving;
pub(crate) mod session;
pub(crate) mod transport;

pub use crate::discovery::service::spawn_discovery;
pub use crate::transport::net::{
    bind_dual_stack_listener, discovery_socket, ipv4_interface_addrs, DEFAULT_DISCOVERY_PORT,
    DISCOVERY_MULTICAST_GROUP,
};
pub use channel::ReceivedChannelMessage;
pub use dm_envelope::{DmEnvelope, DmRoute, ReactionEnvelope, RecallEnvelope};
pub use dm_ratchet::DmRatchet;
pub use filebook::{FileBook, ReceivedFile};
pub use files::FileProgress;
pub use message::MessageBody;
pub use node::{
    ChannelSummary, HistoryEntry, LinkedAccount, Node, NodeError, ReceivedDm, ReceivedProfile,
    SearchHit,
};
pub use pairing::{BackfillRecord, PairingCode, PairingRequest, PairingResponse};
pub use postbox::{elected_post_office, run_relay_accept_loop};
pub use profile::{ProfilePayload, MAX_AVATAR_BYTES};
pub use ratchet_sessions::RatchetSessions;
pub use reaction::{aggregate, ReactionPayload, ReactionView};
pub use received_log::{ReceivedEntry, ReceivedLog};
pub use runtime::{NodeRuntime, RuntimeError};
