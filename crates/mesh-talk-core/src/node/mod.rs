//! The node: the explicit app context that wires identity, discovery, transport, the
//! event log, and DM/group crypto into a runnable peer.
//!
//! The submodules are crate-internal implementation detail; the crate's public SDK
//! surface is exactly the curated re-export block below. Keeping the modules
//! `pub(crate)` (rather than `pub`) means an external consumer's semver contract is the
//! re-exports, not every internal type.

pub(crate) mod account_devices;
pub(crate) mod account_file_proofs;
pub(crate) mod account_preferences;
pub(crate) mod account_suppression;
pub(crate) mod account_sync;
pub(crate) mod account_sync_policy;
pub(crate) mod account_sync_wire;
pub(crate) mod attachment_consent;
mod bridge_client;
pub(crate) mod bridge_directory;
pub(crate) mod call;
pub(crate) mod channel;
pub(crate) mod channel_custody;
mod channel_projection_cache;
pub(crate) mod channel_senders;
pub(crate) mod channels;
pub(crate) mod conversation;
pub(crate) mod delivery_receipt;
mod delivery_runtime;
#[cfg(test)]
mod delivery_runtime_tests;
pub(crate) mod delivery_store;
pub(crate) mod dm;
pub(crate) mod dm_envelope;
pub(crate) mod dm_ratchet;
pub(crate) mod expiry_policy;
mod file_completion;
mod file_consent;
mod file_consent_store;
#[cfg(test)]
mod file_consent_tests;
pub(crate) mod filebook;
pub(crate) mod files;
#[cfg(test)]
mod group_chain_tests;
pub(crate) mod lifecycle;
pub(crate) mod linking;
pub(crate) mod media_store;
pub(crate) mod message;
pub(crate) mod name_directory;
pub(crate) mod node;
mod opaque_relay;
pub(crate) mod pairing;
pub(crate) mod peer_directory;
pub(crate) mod postbox;
pub(crate) mod privacy;
pub(crate) mod privacy_routes;
mod privacy_routing;
pub(crate) mod privacy_runtime;
pub(crate) mod privacy_sync;
pub(crate) mod profile;
pub(crate) mod profile_io;
pub(crate) mod profile_store;
pub(crate) mod queries;
pub(crate) mod ratchet_sessions;
pub(crate) mod reaction;
pub(crate) mod reactions;
pub(crate) mod recall;
pub(crate) mod received_log;
pub(crate) mod recovered_sent_ids;
pub(crate) mod recovery;
mod relay_policy;
pub(crate) mod runtime;
mod runtime_work;
mod search;
pub(crate) mod sentlog;
pub(crate) mod serving;
pub(crate) mod session;
mod session_pool;
#[cfg(test)]
mod test_support;
pub(crate) mod transport;
pub(crate) mod wire;

pub use crate::discovery::service::spawn_discovery;
pub use crate::transport::net::{
    DEFAULT_DISCOVERY_PORT, DISCOVERY_MULTICAST_GROUP, bind_dual_stack_listener, discovery_socket,
    ipv4_interface_addrs,
};
pub use account_devices::AccountDevice;
pub use account_preferences::AccountPreference;
pub use account_sync::{DeviceSyncStatus, SyncPhase};
pub use account_sync_policy::{DeviceSyncPolicy, SyncCategory};
pub use attachment_consent::ReceiveDecision;
pub use call::{CallSignal, ReceivedCallSignal};
pub use channel::{ChannelActivity, ReceivedChannelMessage};
pub use delivery_store::DeliveryStatus;
pub use dm_envelope::{DmEnvelope, DmRoute, ReactionEnvelope, RecallEnvelope};
pub use dm_ratchet::DmRatchet;
pub use filebook::{FileBook, ReceivedFile};
pub use files::{FileProgress, OutgoingFileProgress};
pub use message::MessageBody;
pub use node::{
    ChannelSummary, HistoryEntry, LinkedAccount, Node, NodeError, ReceivedDm, ReceivedProfile,
    SearchHit,
};
pub use pairing::{BackfillRecord, PairingCode, PairingRequest, PairingResponse};
pub use postbox::{
    elected_post_office, run_bridge_relay_accept_loop, run_bridge_relay_accept_loop_with_gateway,
    run_federated_relay_accept_loop_with_directory, run_relay_accept_loop,
};
pub use privacy::{AllowedAccount, PermissionSource, PrivacyPolicy, PrivacySnapshot};
pub use profile::{MAX_AVATAR_BYTES, ProfilePayload};
pub use ratchet_sessions::RatchetSessions;
pub use reaction::{ReactionPayload, ReactionView, aggregate};
pub use received_log::{ReceivedEntry, ReceivedLog};
pub use runtime::{NodeRuntime, RuntimeConfig, RuntimeError, RuntimeEvents, RuntimeNetworkOptions};
