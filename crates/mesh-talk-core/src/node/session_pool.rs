//! Node-owned TCP capacity and explicitly reusable sequential-sync sessions.
//!
//! One-shot call/link/directory protocols always dial fresh. A session enters
//! this pool only after its caller completes every response in a sync exchange.

use super::Node;
use crate::identity::device::PublicIdentity;
use crate::transport::{SecureChannel, TransportError};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{OwnedMutexGuard, OwnedSemaphorePermit, Semaphore},
};

pub(super) const MAX_TCP_CONNECTIONS: usize = 8;
pub(super) const MAX_DIRECTION_CONNECTIONS: usize = MAX_TCP_CONNECTIONS - 1;
const IDLE_TTL: Duration = Duration::from_secs(15);
const CAPACITY_WAIT: Duration = Duration::from_secs(2);
const CAPACITY_REPORT_INTERVAL: Duration = Duration::from_secs(30);
const ACCOUNT_CAPABILITY_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_ACCOUNT_CAPABILITIES: usize = 256;
const MAX_ROUTE_AFFINITIES: usize = 256;
const MAX_AUTHENTICATION_GATES: usize = 256;
type AuthenticationGate = tokio::sync::Mutex<()>;

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

#[derive(Default)]
struct CapacityDiagnostics {
    next_report: Option<Instant>,
    timeouts: u64,
}

impl CapacityDiagnostics {
    fn timed_out(&mut self, now: Instant) -> Option<u64> {
        self.timeouts = self.timeouts.saturating_add(1);
        if self.next_report.is_some_and(|next| now < next) {
            return None;
        }
        self.next_report = Some(now + CAPACITY_REPORT_INTERVAL);
        Some(std::mem::take(&mut self.timeouts))
    }
}

pub(in crate::node) struct SessionPool {
    authentication_gates: Mutex<Vec<(PeerEndpointKey, Weak<AuthenticationGate>)>>,
    capacity_diagnostics: Mutex<CapacityDiagnostics>,
    total: Arc<Semaphore>,
    outbound: Arc<Semaphore>,
    inbound: Arc<Semaphore>,
    idle: Arc<Mutex<Vec<Idle>>>,
    timings: Mutex<super::route_timing::RouteTimings>,
    account_unsupported: Mutex<std::collections::VecDeque<(PeerEndpointKey, Instant)>>,
    file_identity_unsupported: Mutex<std::collections::VecDeque<(PeerEndpointKey, Instant)>>,
    file_identity_scans: Mutex<std::collections::VecDeque<(PublicIdentity, u8, usize)>>,
    route_affinities: Mutex<std::collections::VecDeque<(PeerEndpointKey, SocketAddr)>>,
}

impl Default for SessionPool {
    fn default() -> Self {
        Self {
            authentication_gates: Mutex::new(Vec::new()),
            capacity_diagnostics: Mutex::new(CapacityDiagnostics::default()),
            total: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
            outbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            inbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            idle: Arc::new(Mutex::new(Vec::new())),
            timings: Mutex::new(super::route_timing::RouteTimings::default()),
            account_unsupported: Mutex::new(Default::default()),
            file_identity_unsupported: Mutex::new(Default::default()),
            file_identity_scans: Mutex::new(Default::default()),
            route_affinities: Mutex::new(Default::default()),
        }
    }
}

impl SessionPool {
    /// Serialize unauthenticated sockets for one exact route before they acquire
    /// TCP capacity. Weak entries disappear after the last owner/waiter drops;
    /// active gates must never be evicted, including during a policy reset.
    pub(in crate::node) async fn authentication_admission(
        &self,
        key: PeerEndpointKey,
    ) -> Result<OwnedMutexGuard<()>, TransportError> {
        let gate = {
            let mut gates = self.authentication_gates.lock().unwrap();
            gates.retain(|(_, gate)| gate.strong_count() > 0);
            if let Some(gate) = gates
                .iter()
                .find_map(|(candidate, gate)| (candidate == &key).then(|| gate.upgrade()).flatten())
            {
                gate
            } else {
                if gates.len() >= MAX_AUTHENTICATION_GATES {
                    return Err(TransportError::Noise(
                        "peer authentication capacity busy".into(),
                    ));
                }
                let gate = Arc::new(AuthenticationGate::new(()));
                gates.push((key, Arc::downgrade(&gate)));
                gate
            }
        };
        tokio::time::timeout(CAPACITY_WAIT, gate.lock_owned())
            .await
            .map_err(|_| TransportError::Noise("peer authentication capacity busy".into()))
    }

    fn prune(&self) {
        self.idle
            .lock()
            .unwrap()
            .retain(|entry| entry.returned.elapsed() < IDLE_TTL && entry.channel.io_admitted());
    }

    pub(in crate::node) fn clear(&self) {
        self.idle.lock().unwrap().clear();
        self.account_unsupported.lock().unwrap().clear();
        self.file_identity_unsupported.lock().unwrap().clear();
        self.file_identity_scans.lock().unwrap().clear();
        self.route_affinities.lock().unwrap().clear();
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

    pub(in crate::node) fn routing_cost(
        &self,
        peer: &PublicIdentity,
        address: SocketAddr,
    ) -> Option<Duration> {
        self.timings
            .lock()
            .unwrap()
            .routing_cost(&peer.user_id(), address, Instant::now())
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
            self.idle.lock().unwrap().clear();
        }
    }

    fn report_outbound_capacity_timeout(&self, waiting_since: Instant) {
        let count = self
            .capacity_diagnostics
            .lock()
            .unwrap()
            .timed_out(Instant::now());
        if let Some(count) = count {
            log::warn!(target: "mesh_talk::delivery",
                "outbound TCP capacity wait exhausted wait_ms={} shared_available={} outbound_available={} timeouts_since_report={}",
                waiting_since.elapsed().as_millis(), self.total.available_permits(),
                self.outbound.available_permits(), count);
        }
    }

    async fn acquire(
        &self,
        direction: &Arc<Semaphore>,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        self.prepare_capacity(direction);
        let waiting_since = Instant::now();
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
        .map_err(|_| {
            if Arc::ptr_eq(direction, &self.outbound) {
                self.report_outbound_capacity_timeout(waiting_since);
            }
            TransportError::Noise("TCP connection capacity busy".into())
        })?
    }

    pub(in crate::node) async fn outbound(
        &self,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        if let Some(permits) = self.try_outbound() {
            return Ok(permits);
        }
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

    /// Independent bounded cursors keep one peer's scan from skipping another's rows.
    pub(in crate::node) fn file_identity_scan_offset(
        &self,
        peer: &PublicIdentity,
        scan_kind: u8,
        total: usize,
        width: usize,
    ) -> usize {
        if total == 0 {
            return 0;
        }
        let mut scans = self.file_identity_scans.lock().unwrap();
        let old = scans
            .iter()
            .position(|(public, kind, _)| public == peer && *kind == scan_kind)
            .and_then(|index| scans.remove(index))
            .map_or(0, |(_, _, offset)| offset);
        if scans.len() >= MAX_ACCOUNT_CAPABILITIES {
            scans.pop_front();
        }
        let offset = old % total;
        scans.push_back((peer.clone(), scan_kind, (offset + width.min(total)) % total));
        offset
    }

    pub(in crate::node) fn file_identity_capability_due(&self, key: &PeerEndpointKey) -> bool {
        let mut entries = self.file_identity_unsupported.lock().unwrap();
        entries.retain(|(_, at)| at.elapsed() < ACCOUNT_CAPABILITY_COOLDOWN);
        !entries.iter().any(|(candidate, _)| candidate == key)
    }

    pub(in crate::node) fn note_file_identity_unsupported(&self, key: PeerEndpointKey) {
        let mut entries = self.file_identity_unsupported.lock().unwrap();
        entries.retain(|(candidate, at)| {
            candidate != &key && at.elapsed() < ACCOUNT_CAPABILITY_COOLDOWN
        });
        if entries.len() >= MAX_ACCOUNT_CAPABILITIES {
            entries.pop_front();
        }
        entries.push_back((key, Instant::now()));
    }

    pub(in crate::node) fn note_file_identity_supported(&self, key: &PeerEndpointKey) {
        self.file_identity_unsupported
            .lock()
            .unwrap()
            .retain(|(candidate, _)| candidate != key);
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

/// Admission backpressure occurs locally, before any peer exchange. It provides
/// no negative evidence about the peer's identity or protocol capabilities.
pub(in crate::node) fn local_capacity_busy(error: &TransportError) -> bool {
    matches!(error, TransportError::Noise(message) if matches!(message.as_str(),
        "peer authentication capacity busy" | "TCP connection capacity busy"))
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
    fn local_capacity_backpressure_never_classifies_identity_or_protocol_failures_as_busy() {
        for message in [
            "peer authentication capacity busy",
            "TCP connection capacity busy",
        ] {
            assert!(local_capacity_busy(&TransportError::Noise(message.into())));
        }
        for error in [
            TransportError::IdentityMismatch,
            TransportError::AdmissionDenied,
            TransportError::Noise("invalid Noise handshake".into()),
            TransportError::Io(std::io::ErrorKind::UnexpectedEof.into()),
        ] {
            assert!(!local_capacity_busy(&error));
        }
    }

    #[test]
    fn capacity_diagnostic_throttle_counts_timeouts_without_sliding_its_deadline() {
        let now = Instant::now();
        let mut diagnostics = CapacityDiagnostics::default();
        assert_eq!(diagnostics.timed_out(now), Some(1));
        for seconds in [0, 1, 15, 29] {
            assert_eq!(
                diagnostics.timed_out(now + Duration::from_secs(seconds)),
                None
            );
        }
        assert_eq!(
            diagnostics.timed_out(now + CAPACITY_REPORT_INTERVAL),
            Some(5)
        );
        assert_eq!(diagnostics.timed_out(now + Duration::from_secs(59)), None);
        assert_eq!(
            diagnostics.timed_out(now + CAPACITY_REPORT_INTERVAL * 2),
            Some(2)
        );
        let mut independent_pool = CapacityDiagnostics::default();
        assert_eq!(independent_pool.timed_out(now), Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn capacity_diagnostic_records_only_actual_outbound_capacity_timeouts() {
        let pool = SessionPool::default();
        let outbound = (0..MAX_DIRECTION_CONNECTIONS)
            .map(|_| pool.try_outbound().unwrap())
            .collect::<Vec<_>>();
        assert!(
            pool.capacity_diagnostics
                .lock()
                .unwrap()
                .next_report
                .is_none()
        );
        assert!(pool.outbound().await.is_err());
        assert!(
            pool.capacity_diagnostics
                .lock()
                .unwrap()
                .next_report
                .is_some()
        );
        assert_eq!(pool.capacity_diagnostics.lock().unwrap().timeouts, 0);
        assert!(pool.outbound().await.is_err());
        assert_eq!(pool.capacity_diagnostics.lock().unwrap().timeouts, 1);
        let inbound = pool.inbound().await.unwrap();
        assert!(pool.inbound().await.is_err());
        assert_eq!(
            pool.capacity_diagnostics.lock().unwrap().timeouts,
            1,
            "inbound timeouts must not pollute the outbound diagnostic"
        );
        drop(inbound);
        drop(outbound);
        assert!(pool.outbound().await.is_ok());
        assert_eq!(
            pool.capacity_diagnostics.lock().unwrap().timeouts,
            1,
            "successful capacity acquisition must not create a timeout record"
        );
    }

    fn authentication_key(port: u16) -> PeerEndpointKey {
        PeerEndpointKey {
            identity: crate::identity::device::DeviceIdentity::generate().public(),
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            generation: 1,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn authentication_gates_scope_both_identity_keys_endpoint_and_generation() {
        let pool = SessionPool::default();
        let key = authentication_key(1);
        let owner = pool.authentication_admission(key.clone()).await.unwrap();
        for changed in 0..4 {
            let mut independent = key.clone();
            match changed {
                0 => independent.identity.ed25519_pub[0] ^= 1,
                1 => independent.identity.x25519_pub[0] ^= 1,
                2 => independent.address.set_port(2),
                _ => independent.generation += 1,
            }
            let _guard = pool.authentication_admission(independent).await.unwrap();
        }
        assert!(
            pool.authentication_admission(key.clone()).await.is_err(),
            "a duplicate waits for bounded admission without another owner"
        );
        assert_eq!(pool.outbound.available_permits(), MAX_DIRECTION_CONNECTIONS);
        drop(owner);
        assert!(pool.authentication_admission(key).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn authentication_gates_never_evict_live_owners_and_release_cancelled_waiters() {
        let pool = SessionPool::default();
        let key = authentication_key(1);
        let owner = pool.authentication_admission(key.clone()).await.unwrap();
        let mut owners = Vec::new();
        for port in 2..=MAX_AUTHENTICATION_GATES {
            let mut independent = key.clone();
            independent.address.set_port(port as u16);
            owners.push(pool.authentication_admission(independent).await.unwrap());
        }
        assert!(
            pool.authentication_admission(authentication_key(300))
                .await
                .is_err()
        );
        pool.clear();
        pool.network_changed();
        // Poll then cancel a duplicate waiter; resets must not create a second
        // owner for the still-active old-generation authentication.
        {
            let waiter = pool.authentication_admission(key.clone());
            tokio::pin!(waiter);
            assert!(
                tokio::time::timeout(Duration::from_millis(1), &mut waiter)
                    .await
                    .is_err()
            );
        }
        assert!(pool.authentication_admission(key.clone()).await.is_err());
        drop(owner);
        let recovered = pool.authentication_admission(key).await.unwrap();
        drop(recovered);
        drop(owners);
        assert!(
            pool.authentication_admission(authentication_key(300))
                .await
                .is_ok()
        );
        assert_eq!(pool.authentication_gates.lock().unwrap().len(), 1);
    }

    #[test]
    fn identity_capability_cooldown_is_bounded_and_independent_of_account_sessions() {
        let pool = SessionPool::default();
        let key = authentication_key(41001);
        pool.note_file_identity_unsupported(key.clone());
        assert!(!pool.file_identity_capability_due(&key));
        assert!(pool.account_capability_due(&key));
        let mut changed = key.clone();
        changed.generation += 1;
        assert!(pool.file_identity_capability_due(&changed));
        assert!(pool.file_identity_capability_due(&authentication_key(41002)));
        pool.note_file_identity_supported(&key);
        assert!(pool.file_identity_capability_due(&key));
        for port in 1..=300 {
            pool.note_file_identity_unsupported(authentication_key(port));
        }
        assert_eq!(
            pool.file_identity_unsupported.lock().unwrap().len(),
            MAX_ACCOUNT_CAPABILITIES
        );
        assert!(pool.file_identity_capability_due(&authentication_key(1)));
        pool.clear();
        assert!(pool.file_identity_capability_due(&authentication_key(300)));
    }

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
