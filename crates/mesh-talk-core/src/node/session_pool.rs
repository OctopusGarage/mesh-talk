//! Node-owned TCP capacity and explicitly reusable sequential-sync sessions.
//!
//! One-shot call/link/directory protocols always dial fresh. A session enters
//! this pool only after its caller completes every response in a sync exchange.

use super::Node;
use crate::identity::device::PublicIdentity;
use crate::transport::{SecureChannel, TransportError};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore},
};

pub(super) const MAX_TCP_CONNECTIONS: usize = 8;
pub(super) const MAX_DIRECTION_CONNECTIONS: usize = MAX_TCP_CONNECTIONS - 1;
const IDLE_TTL: Duration = Duration::from_secs(15);
const CAPACITY_WAIT: Duration = Duration::from_secs(2);

struct Idle {
    address: SocketAddr,
    generation: u64,
    returned: Instant,
    channel: SecureChannel<TcpStream>,
    _expiry: ExpiryTask,
}

struct ExpiryTask(tokio::task::AbortHandle);

impl Drop for ExpiryTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(in crate::node) struct SessionPool {
    total: Arc<Semaphore>,
    outbound: Arc<Semaphore>,
    inbound: Arc<Semaphore>,
    idle: Arc<Mutex<Vec<Idle>>>,
    timings: Mutex<super::route_timing::RouteTimings>,
}

impl Default for SessionPool {
    fn default() -> Self {
        Self {
            total: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
            outbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            inbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            idle: Arc::new(Mutex::new(Vec::new())),
            timings: Mutex::new(super::route_timing::RouteTimings::default()),
        }
    }
}

impl SessionPool {
    fn prune(&self) {
        self.idle
            .lock()
            .unwrap()
            .retain(|entry| entry.returned.elapsed() < IDLE_TTL && entry.channel.io_admitted());
    }

    pub(in crate::node) fn clear(&self) {
        self.idle.lock().unwrap().clear();
    }

    pub(in crate::node) fn network_changed(&self) {
        self.clear();
        self.timings.lock().unwrap().clear();
    }

    pub(in crate::node) fn authentication_timeout(
        &self,
        peer: &PublicIdentity,
        address: SocketAddr,
    ) -> Duration {
        self.timings
            .lock()
            .unwrap()
            .deadline(&peer.user_id(), address)
    }

    pub(in crate::node) fn authentication_succeeded(
        &self,
        peer: &PublicIdentity,
        address: SocketAddr,
        elapsed: Duration,
    ) {
        self.timings
            .lock()
            .unwrap()
            .succeeded(&peer.user_id(), address, elapsed);
    }

    pub(in crate::node) fn authentication_timed_out(
        &self,
        peer: &PublicIdentity,
        address: SocketAddr,
    ) {
        self.timings
            .lock()
            .unwrap()
            .timed_out(&peer.user_id(), address);
    }

    pub(in crate::node) fn control_timeout(
        &self,
        peer: &PublicIdentity,
        address: SocketAddr,
    ) -> Duration {
        self.timings
            .lock()
            .unwrap()
            .control_deadline(&peer.user_id(), address)
    }

    pub(in crate::node) fn control_succeeded(
        &self,
        peer: &PublicIdentity,
        address: SocketAddr,
        elapsed: Duration,
    ) {
        self.timings
            .lock()
            .unwrap()
            .control_succeeded(&peer.user_id(), address, elapsed);
    }

    pub(in crate::node) fn control_timed_out(&self, peer: &PublicIdentity, address: SocketAddr) {
        self.timings
            .lock()
            .unwrap()
            .control_timed_out(&peer.user_id(), address);
    }

    fn prepare_capacity(&self, direction: &Arc<Semaphore>) {
        self.prune();
        if direction.available_permits() == 0 || self.total.available_permits() == 0 {
            // Idle connections must never prevent a new identity/route or an
            // incoming peer from making progress. Active sessions are untouched.
            self.clear();
        }
    }

    async fn acquire(
        &self,
        direction: &Arc<Semaphore>,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        self.prepare_capacity(direction);
        tokio::time::timeout(CAPACITY_WAIT, async {
            let directional = direction
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| TransportError::AdmissionDenied)?;
            let shared = self
                .total
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| TransportError::AdmissionDenied)?;
            Ok(vec![directional, shared])
        })
        .await
        .map_err(|_| TransportError::Noise("TCP connection capacity busy".into()))?
    }

    pub(in crate::node) async fn outbound(
        &self,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        self.acquire(&self.outbound).await
    }

    /// An alternate route gets its own socket permits. Racing addresses must
    /// not turn one logical dial into unaccounted TCP connections.
    pub(in crate::node) fn try_outbound(&self) -> Option<Vec<OwnedSemaphorePermit>> {
        self.prepare_capacity(&self.outbound);
        let directional = self.outbound.clone().try_acquire_owned().ok()?;
        let shared = self.total.clone().try_acquire_owned().ok()?;
        Some(vec![directional, shared])
    }

    pub(in crate::node) async fn inbound(
        &self,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        self.acquire(&self.inbound).await
    }

    fn checkout(
        &self,
        address: SocketAddr,
        expected: &PublicIdentity,
        generation: u64,
    ) -> Option<SecureChannel<TcpStream>> {
        self.prune();
        let mut idle = self.idle.lock().unwrap();
        while let Some(index) = idle.iter().position(|entry| {
            entry.address == address
                && entry.generation == generation
                && entry.channel.peer_identity() == expected
        }) {
            let entry = idle.swap_remove(index);
            if entry.channel.idle_tcp_alive() {
                return Some(entry.channel);
            }
        }
        None
    }
}

impl Node {
    fn pooled_presence_current(&self, channel: &SecureChannel<TcpStream>) -> bool {
        let public = channel.peer_identity();
        let matches = |latest: &crate::discovery::Announce| {
            channel.peer_announcement().is_some_and(|authenticated| {
                authenticated.public() == latest.public()
                    && authenticated.account_id() == latest.account_id()
                    && authenticated.post_office == latest.post_office
            })
        };
        let roster = self.roster.lock().expect("roster mutex not poisoned");
        if roster
            .announcement(public)
            .is_some_and(|latest| !matches(latest))
        {
            return false;
        }
        drop(roster);
        let state = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        state.as_ref().is_none_or(|state| {
            channel
                .peer_announcement()
                .is_none_or(|authenticated| state.proofs.accepts_binding(authenticated))
                && state
                    .proofs
                    .by_author(&public.ed25519_pub)
                    .is_none_or(|latest| matches(&latest))
        })
    }

    /// Only sequential-sync protocols may check out a retained channel. The
    /// ordinary privacy_dial intentionally stays fresh for first-frame protocols.
    pub(in crate::node) async fn privacy_sync_dial(
        &self,
        mut addr: SocketAddr,
        expected: &PublicIdentity,
    ) -> Result<SecureChannel<TcpStream>, TransportError> {
        addr.set_ip(crate::transport::net::canonical_peer_ip(addr.ip()));
        {
            let _operation = self.privacy.gate.read().await;
            let generation = self
                .privacy
                .generation
                .load(std::sync::atomic::Ordering::SeqCst);
            // A route race can authenticate an endpoint other than the caller's
            // roster snapshot. Reuse that winner only when both exact endpoints
            // remain routes of this verified device. An unadvertised one-off
            // endpoint still gets its own connection, including pairing ports.
            let preferred = self
                .preferred_routes
                .lock()
                .expect("routes lock not poisoned")
                .get(&expected.user_id())
                .copied()
                .filter(|preferred| *preferred != addr);
            let preferred = preferred.filter(|preferred| {
                let roster = self.roster.lock().expect("roster mutex not poisoned");
                roster.get(&expected.user_id()).is_some_and(|peer| {
                    let direct = |endpoint| {
                        peer.routes.iter().any(|route| {
                            route.addr == endpoint
                                && route.source != crate::discovery::roster::RouteSource::PostOffice
                        })
                    };
                    peer.public == *expected && direct(addr) && direct(*preferred)
                })
            });
            for endpoint in std::iter::once(addr).chain(preferred) {
                let Some(channel) = self.sessions.checkout(endpoint, expected, generation) else {
                    continue;
                };
                if !self.pooled_presence_current(&channel) {
                    continue;
                }
                if !self.account_allowed(expected, channel.peer_announcement())
                    && !self.group_member(expected)
                    && !self.group_departure_peer(expected)
                {
                    return Err(TransportError::AdmissionDenied);
                }
                return Ok(channel);
            }
            // A valid new certificate or role announcement invalidates the old
            // auth snapshot. Drop it and require a fresh, verified handshake.
        }
        self.privacy_dial(addr, expected).await
    }

    /// The caller must have consumed a successful complete sync exchange. On
    /// cancellation, malformed replies or I/O errors, simply drop the channel.
    pub(in crate::node) fn recycle_sync_channel(&self, channel: SecureChannel<TcpStream>) {
        let Some(generation) = channel.host_generation() else {
            return;
        };
        if !channel.io_admitted() || !channel.idle_tcp_alive() {
            return;
        }
        let Ok(address) = channel.tcp_peer_addr() else {
            return;
        };
        let mut idle = self.sessions.idle.lock().unwrap();
        // Keep at most one idle channel per exact identity/route. Do not keep
        // the final outbound slot idle while other work is awaiting capacity.
        if self.sessions.outbound.available_permits() == 0
            || self.sessions.total.available_permits() == 0
        {
            return;
        }
        idle.retain(|entry| {
            !(entry.address == address && entry.channel.peer_identity() == channel.peer_identity())
        });
        let returned = Instant::now();
        let weak = Arc::downgrade(&self.sessions.idle);
        let expiry = tokio::spawn(async move {
            tokio::time::sleep(IDLE_TTL).await;
            if let Some(idle) = weak.upgrade() {
                idle.lock()
                    .unwrap()
                    .retain(|entry| entry.returned.elapsed() < IDLE_TTL);
            }
        });
        idle.push(Idle {
            address,
            generation,
            returned,
            channel,
            _expiry: ExpiryTask(expiry.abort_handle()),
        });
    }
}
