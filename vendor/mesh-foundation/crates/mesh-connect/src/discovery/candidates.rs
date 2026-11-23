//! Host-owned address inventories; core does not know their network provider.

use futures_util::future::BoxFuture;
use std::net::SocketAddr;

/// A snapshot source. LAN discovery remains an event stream; this interface is
/// for optional routed address inventories supplied by the desktop host.
pub trait CandidateSource: Send + Sync {
    fn snapshot(&self) -> BoxFuture<'_, Vec<SocketAddr>>;

    /// Optional topology invalidation. Values are scheduling hints only; callers
    /// still obtain a fresh bounded snapshot and authenticate routed responses.
    /// CLI-only sources keep sparse repair and explicit host/network wakes.
    fn subscribe_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
}
