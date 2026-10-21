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
const ACCOUNT_CAPABILITY_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_ACCOUNT_CAPABILITIES: usize = 256;
const MAX_ROUTE_AFFINITIES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::node) enum SessionProtocol {
    Sync,
    Account,
}

#[derive(Clone, PartialEq)]
pub(in crate::node) struct PeerEndpointKey {
    identity: PublicIdentity,
    address: SocketAddr,
    generation: u64,
}

struct Idle {
    protocol: SessionProtocol,
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
    account_unsupported: Mutex<std::collections::VecDeque<(PeerEndpointKey, Instant)>>,
    route_affinities: Mutex<std::collections::VecDeque<(PeerEndpointKey, SocketAddr)>>,
}

impl Default for SessionPool {
    fn default() -> Self {
        Self {
            total: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
            outbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            inbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            idle: Arc::new(Mutex::new(Vec::new())),
            account_unsupported: Mutex::new(Default::default()),
            route_affinities: Mutex::new(Default::default()),
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
        self.account_unsupported.lock().unwrap().clear();
        self.route_affinities.lock().unwrap().clear();
    }

    async fn acquire(
        &self,
        direction: &Arc<Semaphore>,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        self.prune();
        if direction.available_permits() == 0 || self.total.available_permits() == 0 {
            // Idle connections must never prevent a new identity/route or an
            // incoming peer from making progress. Active sessions are untouched.
            self.idle.lock().unwrap().clear();
        }
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

    pub(in crate::node) async fn inbound(
        &self,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        self.acquire(&self.inbound).await
    }

    fn checkout(
        &self,
        protocol: SessionProtocol,
        address: SocketAddr,
        expected: &PublicIdentity,
        generation: u64,
    ) -> Option<SecureChannel<TcpStream>> {
        self.prune();
        let mut idle = self.idle.lock().unwrap();
        while let Some(index) = idle.iter().position(|entry| {
            entry.protocol == protocol
                && entry.address == address
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

    pub(in crate::node) fn account_capability_due(&self, key: &PeerEndpointKey) -> bool {
        let mut entries = self.account_unsupported.lock().unwrap();
        entries.retain(|(_, at)| at.elapsed() < ACCOUNT_CAPABILITY_COOLDOWN);
        !entries.iter().any(|(candidate, _)| candidate == key)
    }

    pub(in crate::node) fn note_account_unsupported(&self, key: PeerEndpointKey) {
        let mut entries = self.account_unsupported.lock().unwrap();
        entries.retain(|(candidate, at)| {
            candidate != &key && at.elapsed() < ACCOUNT_CAPABILITY_COOLDOWN
        });
        if entries.len() >= MAX_ACCOUNT_CAPABILITIES {
            entries.pop_front();
        }
        entries.push_back((key, Instant::now()));
    }

    pub(in crate::node) fn note_account_supported(&self, key: &PeerEndpointKey) {
        self.account_unsupported
            .lock()
            .unwrap()
            .retain(|(candidate, _)| candidate != key);
    }
}

pub(in crate::node) fn retryable_transport_error(error: &TransportError) -> bool {
    matches!(error, TransportError::Io(error) if matches!(error.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset |
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected |
        std::io::ErrorKind::ConnectionAborted))
}

pub(in crate::node) fn retryable_sync_error(error: &super::session::SessionError) -> bool {
    matches!(error, super::session::SessionError::Transport(error) if retryable_transport_error(error))
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
        addr: SocketAddr,
        expected: &PublicIdentity,
    ) -> Result<SecureChannel<TcpStream>, TransportError> {
        self.privacy_sync_dial_with_reuse(addr, expected)
            .await
            .map(|(channel, _)| channel)
    }

    pub(in crate::node) async fn privacy_sync_dial_with_reuse(
        &self,
        addr: SocketAddr,
        expected: &PublicIdentity,
    ) -> Result<(SecureChannel<TcpStream>, bool), TransportError> {
        if let Some(channel) = self
            .checkout_reusable_channel(SessionProtocol::Sync, addr, expected)
            .await?
        {
            return Ok((channel, true));
        }
        self.privacy_dial(addr, expected)
            .await
            .map(|channel| (channel, false))
    }

    pub(in crate::node) fn peer_endpoint_key(
        &self,
        mut addr: SocketAddr,
        expected: &PublicIdentity,
    ) -> PeerEndpointKey {
        addr.set_ip(crate::transport::net::canonical_peer_ip(addr.ip()));
        PeerEndpointKey {
            identity: expected.clone(),
            address: addr,
            generation: self
                .privacy
                .generation
                .load(std::sync::atomic::Ordering::SeqCst),
        }
    }

    /// Remember an actually authenticated fallback for this precise dial input.
    /// A different explicit listener must not inherit another endpoint's affinity.
    pub(in crate::node) fn note_dialed_route(
        &self,
        requested: SocketAddr,
        channel: &SecureChannel<TcpStream>,
    ) {
        let (Ok(actual), Some(generation)) = (channel.tcp_peer_addr(), channel.host_generation())
        else {
            return;
        };
        let mut key = self.peer_endpoint_key(requested, channel.peer_identity());
        key.generation = generation;
        let mut entries = self.sessions.route_affinities.lock().unwrap();
        entries.retain(|(candidate, _)| candidate != &key);
        if key.address != actual {
            if entries.len() >= MAX_ROUTE_AFFINITIES {
                entries.pop_front();
            }
            entries.push_back((key, actual));
        }
    }

    pub(in crate::node) async fn checkout_reusable_channel(
        &self,
        protocol: SessionProtocol,
        mut addr: SocketAddr,
        expected: &PublicIdentity,
    ) -> Result<Option<SecureChannel<TcpStream>>, TransportError> {
        addr.set_ip(crate::transport::net::canonical_peer_ip(addr.ip()));
        {
            let _operation = self.privacy.gate.read().await;
            let generation = self
                .privacy
                .generation
                .load(std::sync::atomic::Ordering::SeqCst);
            let announce = self.roster.lock().unwrap().announcement(expected).cloned();
            let permitted = self.privacy_route_candidates(addr, expected, announce.as_ref());
            let key = PeerEndpointKey {
                identity: expected.clone(),
                address: addr,
                generation,
            };
            let fallback = self
                .sessions
                .route_affinities
                .lock()
                .unwrap()
                .iter()
                .find(|(candidate, _)| candidate == &key)
                .map(|(_, actual)| *actual)
                .filter(|actual| permitted.contains(actual));
            for addr in std::iter::once(addr).chain(fallback) {
                if let Some(channel) = self.sessions.checkout(protocol, addr, expected, generation)
                    && self.pooled_presence_current(&channel)
                {
                    if !self.account_allowed(expected, channel.peer_announcement())
                        && !self.group_member(expected)
                        && !self.group_departure_peer(expected)
                    {
                        return Err(TransportError::AdmissionDenied);
                    }
                    return Ok(Some(channel));
                }
            }
            // A valid new certificate or role announcement invalidates the old
            // auth snapshot. Drop it and require a fresh, verified handshake.
        }
        Ok(None)
    }

    /// The caller must have consumed a successful complete sync exchange. On
    /// cancellation, malformed replies or I/O errors, simply drop the channel.
    pub(in crate::node) fn recycle_sync_channel(&self, channel: SecureChannel<TcpStream>) {
        self.recycle_channel(SessionProtocol::Sync, channel);
    }

    pub(in crate::node) fn recycle_account_channel(&self, channel: SecureChannel<TcpStream>) {
        self.recycle_channel(SessionProtocol::Account, channel);
    }

    fn recycle_channel(&self, protocol: SessionProtocol, channel: SecureChannel<TcpStream>) {
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
            !(entry.protocol == protocol
                && entry.address == address
                && entry.channel.peer_identity() == channel.peer_identity())
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
            protocol,
            address,
            generation,
            returned,
            channel,
            _expiry: ExpiryTask(expiry.abort_handle()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_account_capabilities_are_bounded_and_scope_specific() {
        let pool = SessionPool::default();
        let identity = crate::identity::device::DeviceIdentity::generate().public();
        let key = |port| PeerEndpointKey {
            identity: identity.clone(),
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            generation: 1,
        };
        for port in 1..=300 {
            pool.note_account_unsupported(key(port));
        }
        assert!(
            pool.account_capability_due(&key(1)),
            "old entries must be evicted at the capacity bound"
        );
        assert!(!pool.account_capability_due(&key(300)));
        assert_eq!(
            pool.account_unsupported.lock().unwrap().len(),
            MAX_ACCOUNT_CAPABILITIES
        );
        let mut changed = key(300);
        changed.generation += 1;
        assert!(pool.account_capability_due(&changed));
        changed = key(300);
        changed.identity = crate::identity::device::DeviceIdentity::generate().public();
        assert!(pool.account_capability_due(&changed));
        assert!(pool.account_capability_due(&key(301)));
        pool.note_account_supported(&key(300));
        assert!(pool.account_capability_due(&key(300)));
        pool.clear();
        assert!(
            pool.account_capability_due(&key(299)),
            "privacy/network reset must clear unsupported state"
        );
    }

    #[test]
    fn unsupported_account_capabilities_expire_without_background_polling() {
        let pool = SessionPool::default();
        let key = PeerEndpointKey {
            identity: crate::identity::device::DeviceIdentity::generate().public(),
            address: SocketAddr::from(([127, 0, 0, 1], 9)),
            generation: 1,
        };
        pool.note_account_unsupported(key.clone());
        pool.account_unsupported
            .lock()
            .unwrap()
            .back_mut()
            .unwrap()
            .1 = Instant::now() - ACCOUNT_CAPABILITY_COOLDOWN;
        assert!(pool.account_capability_due(&key));
    }
}
