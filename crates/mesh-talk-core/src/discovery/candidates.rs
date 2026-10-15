//! Host-owned address inventories; core does not know their network provider.

use futures_util::future::BoxFuture;
use std::net::SocketAddr;

/// A snapshot source. LAN discovery remains an event stream; this interface is
/// for optional routed address inventories supplied by the desktop host.
pub trait CandidateSource: Send + Sync {
    fn snapshot(&self) -> BoxFuture<'_, Vec<SocketAddr>>;
}
