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
    sync::{OwnedSemaphorePermit, Semaphore},
};

pub(super) const MAX_TCP_CONNECTIONS: usize = 8;
pub(super) const MAX_WATCH_CONNECTIONS: usize = 4;
pub(super) const MAX_DIRECTION_CONNECTIONS: usize = MAX_TCP_CONNECTIONS - 1;
const IDLE_TTL: Duration = Duration::from_secs(15);
const CAPACITY_WAIT: Duration = Duration::from_secs(2);
const CAPACITY_REPORT_INTERVAL: Duration = Duration::from_secs(30);
const ACCOUNT_CAPABILITY_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_ACCOUNT_CAPABILITIES: usize = 256;
const MAX_ROUTE_AFFINITIES: usize = 256;
const MAX_AUTHENTICATION_GATES: usize = 256;
#[derive(Default)]
struct AuthenticationGate {
    state: Mutex<AuthenticationState>,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct AuthenticationState {
    active: bool,
    next_ticket: u64,
    ordinary_due: bool,
    ordinary: std::collections::VecDeque<u64>,
    control: std::collections::VecDeque<u64>,
}

impl AuthenticationState {
    fn queue(&mut self, priority: AuthenticationPriority) -> &mut std::collections::VecDeque<u64> {
        match priority {
            AuthenticationPriority::Ordinary => &mut self.ordinary,
            AuthenticationPriority::Control => &mut self.control,
        }
    }
    fn next(&self) -> Option<(AuthenticationPriority, u64)> {
        if self.ordinary_due
            && let Some(ticket) = self.ordinary.front()
        {
            Some((AuthenticationPriority::Ordinary, *ticket))
        } else if let Some(ticket) = self.control.front() {
            Some((AuthenticationPriority::Control, *ticket))
        } else {
            self.ordinary
                .front()
                .map(|ticket| (AuthenticationPriority::Ordinary, *ticket))
        }
    }
}

pub(in crate::node) struct AuthenticationLease {
    gate: Arc<AuthenticationGate>,
}
impl Drop for AuthenticationLease {
    fn drop(&mut self) {
        self.gate.state.lock().unwrap().active = false;
        self.gate.changed.notify_waiters();
    }
}

struct AuthenticationWaiter {
    gate: Arc<AuthenticationGate>,
    priority: AuthenticationPriority,
    ticket: u64,
    queued: bool,
}
impl Drop for AuthenticationWaiter {
    fn drop(&mut self) {
        if self.queued {
            self.gate
                .state
                .lock()
                .unwrap()
                .queue(self.priority)
                .retain(|ticket| *ticket != self.ticket);
            self.gate.changed.notify_waiters();
        }
    }
}
impl AuthenticationWaiter {
    async fn wait(mut self) -> AuthenticationLease {
        loop {
            // Register before inspecting state: release/cancellation between the
            // check and await must not lose the sole owner's handoff.
            let changed = self.gate.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let admitted = {
                let mut state = self.gate.state.lock().unwrap();
                if !state.active && state.next() == Some((self.priority, self.ticket)) {
                    state.queue(self.priority).pop_front();
                    state.active = true;
                    state.ordinary_due = self.priority == AuthenticationPriority::Control;
                    true
                } else {
                    false
                }
            };
            if admitted {
                self.queued = false;
                return AuthenticationLease {
                    gate: self.gate.clone(),
                };
            }
            changed.await;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::node) enum AuthenticationPriority {
    Ordinary,
    Control,
}

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

type HistoricalRouteKey = ([u8; 32], [u8; 32], SocketAddr);
type HistoricalAuthenticationRetries =
    super::delivery_retry::RetrySchedule<HistoricalRouteKey, (u64, u64)>;

fn historical_route_key(key: &PeerEndpointKey) -> HistoricalRouteKey {
    let mut address = key.address;
    address.set_ip(crate::transport::net::canonical_peer_ip(address.ip()));
    (key.identity.ed25519_pub, key.identity.x25519_pub, address)
}

/// Work pacing only. Cancellation and local capacity retain the earlier learned
/// failures without manufacturing a remote failure or holding any socket lease.
pub(in crate::node) struct HistoricalAuthenticationAttempt<'a> {
    retries: &'a Mutex<HistoricalAuthenticationRetries>,
    key: HistoricalRouteKey,
    token: u64,
    completed: bool,
}
impl HistoricalAuthenticationAttempt<'_> {
    pub(in crate::node) fn complete(&mut self, authenticated: bool, now: Instant) {
        self.retries
            .lock()
            .unwrap()
            .complete(&self.key, self.token, authenticated, now);
        self.completed = true;
    }
}
impl Drop for HistoricalAuthenticationAttempt<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.retries.lock().unwrap().defer(
                &self.key,
                self.token,
                Duration::from_secs(5),
                Instant::now(),
            );
        }
    }
}

const SLOW_AUTHENTICATION_REPORT_INTERVAL: Duration = Duration::from_secs(300);
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::node) struct SlowAuthenticationSummary {
    pub attempts: u64,
    pub failures: u64,
    pub historical: u64,
    pub longest: Duration,
}
#[derive(Default)]
struct SlowAuthenticationDiagnostics {
    next_report: Option<Instant>,
    pending: SlowAuthenticationSummary,
}
impl SlowAuthenticationDiagnostics {
    fn observe(
        &mut self,
        now: Instant,
        elapsed: Duration,
        authenticated: bool,
        historical: bool,
        interactive: bool,
    ) -> Option<SlowAuthenticationSummary> {
        if elapsed < Duration::from_secs(2) {
            return None;
        }
        let one = SlowAuthenticationSummary {
            attempts: 1,
            failures: u64::from(!authenticated),
            historical: u64::from(historical),
            longest: elapsed,
        };
        // Explicit user actions retain their own diagnostic; they cannot reset
        // the background window or turn background noise into repeated reports.
        if interactive {
            return Some(one);
        }
        self.pending.attempts = self.pending.attempts.saturating_add(1);
        self.pending.failures = self.pending.failures.saturating_add(one.failures);
        self.pending.historical = self.pending.historical.saturating_add(one.historical);
        self.pending.longest = self.pending.longest.max(elapsed);
        if self.next_report.is_some_and(|next| now < next) {
            return None;
        }
        self.next_report = Some(now + SLOW_AUTHENTICATION_REPORT_INTERVAL);
        Some(std::mem::take(&mut self.pending))
    }
}

#[derive(Default)]
struct InboundIdleState {
    next: u64,
    retiring: Option<u64>,
    entries: std::collections::VecDeque<(u64, tokio::sync::oneshot::Sender<()>)>,
}
#[derive(Default)]
struct InboundIdleRegistry {
    state: Mutex<InboundIdleState>,
    changed: tokio::sync::Notify,
}
pub(in crate::node) struct InboundIdleRegistration {
    registry: Arc<InboundIdleRegistry>,
    token: u64,
}
impl Drop for InboundIdleRegistration {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock().unwrap();
        state.entries.retain(|(token, _)| *token != self.token);
        if state.retiring == Some(self.token) {
            state.retiring = None;
        }
        drop(state);
        self.registry.changed.notify_waiters();
    }
}

pub(in crate::node) enum InboundReuse {
    Frame(Vec<u8>),
    Retired(InboundIdleRegistration),
}
async fn wait_registered_inbound(
    channel: &SecureChannel<TcpStream>,
    registration: InboundIdleRegistration,
    mut retire: tokio::sync::oneshot::Receiver<()>,
) -> Result<Option<InboundIdleRegistration>, TransportError> {
    tokio::select! {
        biased;
        ready = channel.inbound_frame_ready() => ready.map(|_| None).map_err(TransportError::Io),
        _ = &mut retire => Ok(Some(registration)),
    }
}

pub(in crate::node) struct SessionPool {
    authentication_gates: Mutex<Vec<(PeerEndpointKey, Weak<AuthenticationGate>)>>,
    capacity_diagnostics: Mutex<CapacityDiagnostics>,
    historical_authentication_retries: Mutex<HistoricalAuthenticationRetries>,
    slow_authentication_diagnostics: Mutex<SlowAuthenticationDiagnostics>,
    total: Arc<Semaphore>,
    outbound: Arc<Semaphore>,
    watches: Arc<Semaphore>,
    historical: Arc<Semaphore>,
    inbound: Arc<Semaphore>,
    idle: Arc<Mutex<Vec<Idle>>>,
    inbound_idle: Arc<InboundIdleRegistry>,
    timings: Mutex<super::route_timing::RouteTimings>,
    account_unsupported: Mutex<std::collections::VecDeque<(PeerEndpointKey, Instant)>>,
    file_identity_unsupported: Mutex<std::collections::VecDeque<(PeerEndpointKey, Instant)>>,
    file_identity_scans: Mutex<std::collections::VecDeque<(PublicIdentity, u8, usize)>>,
    route_affinities: Mutex<std::collections::VecDeque<(PeerEndpointKey, SocketAddr, Instant)>>,
}

impl Default for SessionPool {
    fn default() -> Self {
        Self {
            authentication_gates: Mutex::new(Vec::new()),
            capacity_diagnostics: Mutex::new(CapacityDiagnostics::default()),
            historical_authentication_retries: Mutex::new(
                HistoricalAuthenticationRetries::with_failure_bounds(
                    Duration::from_secs(300),
                    Duration::from_secs(300),
                ),
            ),
            slow_authentication_diagnostics: Mutex::new(SlowAuthenticationDiagnostics::default()),
            total: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
            outbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            watches: Arc::new(Semaphore::new(MAX_WATCH_CONNECTIONS)),
            historical: Arc::new(Semaphore::new(2)),
            inbound: Arc::new(Semaphore::new(MAX_DIRECTION_CONNECTIONS)),
            idle: Arc::new(Mutex::new(Vec::new())),
            inbound_idle: Arc::new(InboundIdleRegistry::default()),
            timings: Mutex::new(super::route_timing::RouteTimings::default()),
            account_unsupported: Mutex::new(Default::default()),
            file_identity_unsupported: Mutex::new(Default::default()),
            file_identity_scans: Mutex::new(Default::default()),
            route_affinities: Mutex::new(Default::default()),
        }
    }
}

impl SessionPool {
    pub(in crate::node) fn historical_authentication(
        &self,
        key: &PeerEndpointKey,
        network: u64,
        now: Instant,
    ) -> Result<HistoricalAuthenticationAttempt<'_>, TransportError> {
        let route = historical_route_key(key);
        let (token, _) = self
            .historical_authentication_retries
            .lock()
            .unwrap()
            .reserve(route, (key.generation, network), now)
            .ok_or_else(|| TransportError::Noise("historical route retry deferred".into()))?;
        Ok(HistoricalAuthenticationAttempt {
            retries: &self.historical_authentication_retries,
            key: route,
            token,
            completed: false,
        })
    }

    pub(in crate::node) fn observe_slow_authentication(
        &self,
        elapsed: Duration,
        authenticated: bool,
        historical: bool,
        interactive: bool,
    ) -> Option<SlowAuthenticationSummary> {
        self.slow_authentication_diagnostics
            .lock()
            .unwrap()
            .observe(
                Instant::now(),
                elapsed,
                authenticated,
                historical,
                interactive,
            )
    }
    /// All background producers share this speculative dial budget. The permit
    /// covers the actual TCP/Noise attempt, never an authenticated session.
    pub(in crate::node) fn historical_dial(&self) -> Result<OwnedSemaphorePermit, TransportError> {
        self.historical
            .clone()
            .try_acquire_owned()
            .map_err(|_| TransportError::Noise("historical route capacity busy".into()))
    }

    pub(in crate::node) fn historical_capacity_available(&self) -> bool {
        self.historical.available_permits() > 0
    }

    fn remember_authenticated_route(
        &self,
        key: PeerEndpointKey,
        mut actual: SocketAddr,
        now: Instant,
    ) {
        actual.set_ip(crate::transport::net::canonical_peer_ip(actual.ip()));
        let mut entries = self.route_affinities.lock().unwrap();
        entries.retain(|(candidate, _, _)| candidate != &key);
        if entries.len() >= MAX_ROUTE_AFFINITIES {
            entries.pop_front();
        }
        entries.push_back((key, actual, now));
    }

    pub(in crate::node) fn recently_authenticated_route(
        &self,
        key: &PeerEndpointKey,
        now: Instant,
    ) -> bool {
        self.route_affinities
            .lock()
            .unwrap()
            .iter()
            .any(|(candidate, actual, observed)| {
                candidate.identity == key.identity
                    && candidate.generation == key.generation
                    && *actual == key.address
                    && now.saturating_duration_since(*observed) < Duration::from_secs(15)
            })
    }

    /// Serialize unauthenticated sockets for one exact route before they acquire
    /// TCP capacity. Weak entries disappear after the last owner/waiter drops;
    /// active gates must never be evicted, including during a policy reset.
    pub(in crate::node) async fn authentication_admission(
        &self,
        key: PeerEndpointKey,
    ) -> Result<AuthenticationLease, TransportError> {
        self.authentication_admission_priority(key, AuthenticationPriority::Ordinary)
            .await
    }

    pub(in crate::node) async fn authentication_admission_priority(
        &self,
        key: PeerEndpointKey,
        priority: AuthenticationPriority,
    ) -> Result<AuthenticationLease, TransportError> {
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
                let gate = Arc::new(AuthenticationGate::default());
                gates.push((key, Arc::downgrade(&gate)));
                gate
            }
        };
        let ticket = {
            let mut state = gate.state.lock().unwrap();
            let ticket = state.next_ticket;
            state.next_ticket = ticket
                .checked_add(1)
                .ok_or_else(|| TransportError::Noise("peer authentication capacity busy".into()))?;
            state.queue(priority).push_back(ticket);
            ticket
        };
        let waiter = AuthenticationWaiter {
            gate,
            priority,
            ticket,
            queued: true,
        };
        tokio::time::timeout(CAPACITY_WAIT, waiter.wait())
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
        self.historical_authentication_retries
            .lock()
            .unwrap()
            .clear();
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
        let mut address = address;
        address.set_ip(crate::transport::net::canonical_peer_ip(address.ip()));
        self.historical_authentication_retries
            .lock()
            .unwrap()
            .forget(&(peer.ed25519_pub, peer.x25519_pub, address));
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

    fn register_inbound_idle(
        &self,
    ) -> Option<(InboundIdleRegistration, tokio::sync::oneshot::Receiver<()>)> {
        let mut state = self.inbound_idle.state.lock().unwrap();
        if state.entries.len() + usize::from(state.retiring.is_some()) >= MAX_DIRECTION_CONNECTIONS
        {
            return None;
        }
        let token = state.next.checked_add(1)?;
        state.next = token;
        let (send, receive) = tokio::sync::oneshot::channel();
        state.entries.push_back((token, send));
        drop(state);
        self.inbound_idle.changed.notify_waiters();
        Some((
            InboundIdleRegistration {
                registry: self.inbound_idle.clone(),
                token,
            },
            receive,
        ))
    }
    fn retire_oldest_inbound_idle(&self) {
        loop {
            let entry = {
                let mut state = self.inbound_idle.state.lock().unwrap();
                if state.retiring.is_some() {
                    return;
                }
                let entry = state.entries.pop_front();
                state.retiring = entry.as_ref().map(|(token, _)| *token);
                entry
            };
            let Some((token, retire)) = entry else {
                return;
            };
            if retire.send(()).is_ok() {
                return;
            }
            let mut state = self.inbound_idle.state.lock().unwrap();
            if state.retiring == Some(token) {
                state.retiring = None;
            }
        }
    }
    /// Retirement only signals a completed idle connection. Its owning future
    /// must drop the channel and release the real permits before admission wins.
    pub(in crate::node) async fn accept_connection_slot(
        &self,
        slots: &Arc<Semaphore>,
    ) -> Result<OwnedSemaphorePermit, TransportError> {
        loop {
            let changed = self.inbound_idle.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Ok(permit) = slots.clone().try_acquire_owned() {
                return Ok(permit);
            }
            // The channel can release its TCP permits just before the outer
            // accept task drops its connection permit. Wait for that owner;
            // do not retire another idle channel during this handoff.
            if self.inbound.available_permits() == 0 {
                self.retire_oldest_inbound_idle();
            }
            tokio::select! {
                result = slots.clone().acquire_owned() => return result.map_err(|_| TransportError::AdmissionDenied),
                _ = changed => {}
            }
        }
    }

    fn prepare_capacity(&self, direction: &Arc<Semaphore>) {
        self.prune();
        if direction.available_permits() == 0 || self.total.available_permits() == 0 {
            // Idle connections must never prevent a new identity/route or an
            // incoming peer from making progress. Active sessions are untouched.
            self.idle.lock().unwrap().clear();
            if self.total.available_permits() == 0
                || Arc::ptr_eq(direction, &self.inbound) && direction.available_permits() == 0
            {
                self.retire_oldest_inbound_idle();
            }
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
        let waiting_since = Instant::now();
        tokio::time::timeout(CAPACITY_WAIT, async {
            // Keep the same semaphore futures queued while eligibility changes.
            // An active connection can become idle after the initial capacity
            // check; only its actual owner can release the TCP permits.
            let acquisition = async {
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
            };
            tokio::pin!(acquisition);
            loop {
                let changed = self.inbound_idle.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                self.prepare_capacity(direction);
                tokio::select! {
                    result = &mut acquisition => return result,
                    _ = changed => {}
                }
            }
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

    /// Parked mailbox notification sockets have their own fixed budget. They
    /// never enter the sequential exchange pool or consume ordinary TCP slots.
    /// Each raced route retains its actual permit until its socket is dropped.
    pub(in crate::node) async fn watch_outbound(
        &self,
    ) -> Result<Vec<OwnedSemaphorePermit>, TransportError> {
        let permit = tokio::time::timeout(CAPACITY_WAIT, self.watches.clone().acquire_owned())
            .await
            .map_err(|_| TransportError::Noise("mailbox watch capacity busy".into()))?
            .map_err(|_| TransportError::AdmissionDenied)?;
        Ok(vec![permit])
    }

    /// An alternate route gets its own socket permits. Racing addresses must
    /// not turn one logical dial into unaccounted TCP connections.
    pub(in crate::node) fn try_outbound(&self) -> Option<Vec<OwnedSemaphorePermit>> {
        self.prepare_capacity(&self.outbound);
        let directional = self.outbound.clone().try_acquire_owned().ok()?;
        let shared = self.total.clone().try_acquire_owned().ok()?;
        Some(vec![directional, shared])
    }

    #[cfg(test)]
    pub(in crate::node) fn test_watch_available(&self) -> usize {
        self.watches.available_permits()
    }

    #[cfg(test)]
    pub(in crate::node) fn test_capacity_snapshot(&self) -> (usize, usize, usize, Option<usize>) {
        (
            self.total.available_permits(),
            self.inbound.available_permits(),
            self.outbound.available_permits(),
            self.idle.try_lock().ok().map(|idle| idle.len()),
        )
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

    pub(in crate::node) fn file_identity_scan_after(&self, peer: &PublicIdentity) -> Option<i64> {
        self.file_identity_scans
            .lock()
            .unwrap()
            .iter()
            .find(|(public, kind, _)| public == peer && *kind == 0)
            .and_then(|(_, _, value)| i64::try_from(*value).ok())
            .filter(|value| *value != 0)
    }

    pub(in crate::node) fn advance_file_identity_scan(
        &self,
        peer: &PublicIdentity,
        after: Option<i64>,
    ) {
        let mut scans = self.file_identity_scans.lock().unwrap();
        if let Some(index) = scans
            .iter()
            .position(|(public, kind, _)| public == peer && *kind == 0)
        {
            scans.remove(index);
        }
        if scans.len() >= MAX_ACCOUNT_CAPABILITIES {
            scans.pop_front();
        }
        scans.push_back((
            peer.clone(),
            0,
            after
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0),
        ));
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
        "peer authentication capacity busy" | "TCP connection capacity busy" | "historical route capacity busy" | "historical route retry deferred"))
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
    fn pooled_presence_current(
        &self,
        protocol: SessionProtocol,
        channel: &SecureChannel<TcpStream>,
    ) -> bool {
        let public = channel.peer_identity();
        let identity_only = channel.peer_announcement().is_none();
        // Resolve durable membership before taking roster/privacy locks.
        let certified = identity_only && self.certified_sibling(public);
        if protocol == SessionProtocol::Sync && certified {
            return false;
        }
        let matches = |latest: &crate::discovery::Announce| {
            channel.peer_announcement().map_or_else(
                || {
                    latest.public() == *public
                        && !latest.post_office
                        && match protocol {
                            SessionProtocol::Sync => latest.account_id().is_none(),
                            SessionProtocol::Account => {
                                certified
                                    && latest.valid_presence()
                                    && latest.account_id().as_deref()
                                        == Some(self.account_id().as_str())
                            }
                        }
                },
                |authenticated| {
                    authenticated.public() == latest.public()
                        && authenticated.account_id() == latest.account_id()
                        && authenticated.post_office == latest.post_office
                },
            )
        };
        let latest = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .announcement(public)
            .cloned();
        if latest.as_ref().is_some_and(|latest| !matches(latest)) {
            return false;
        }
        let state = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        state.as_ref().is_none_or(|state| {
            // Generic identity-only Noise cannot stand in for a retained account pin,
            // including a pairing certificate with no cached announcement.
            if protocol == SessionProtocol::Sync
                && channel.peer_announcement().is_none()
                && (state.proofs.account_for(public).is_some()
                    || latest
                        .as_ref()
                        .is_some_and(|latest| !state.proofs.accepts_binding(latest)))
            {
                return false;
            }
            if protocol == SessionProtocol::Account
                && identity_only
                && (latest
                    .as_ref()
                    .is_some_and(|latest| !state.proofs.accepts_binding(latest))
                    || state
                        .proofs
                        .account_for(public)
                        .is_some_and(|account| account != self.account_id()))
            {
                return false;
            }
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
        self.sessions
            .remember_authenticated_route(key, actual, Instant::now());
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
                .find(|(candidate, _, _)| candidate == &key)
                .map(|(_, actual, _)| *actual)
                .filter(|actual| *actual != addr && permitted.contains(actual));
            for addr in std::iter::once(addr).chain(fallback) {
                if let Some(channel) = self.sessions.checkout(protocol, addr, expected, generation)
                    && self.pooled_presence_current(protocol, &channel)
                {
                    if !self.account_allowed(expected, channel.peer_announcement())
                        && !self
                            .group_member(expected)
                            .map_err(|error| TransportError::Io(std::io::Error::other(error)))?
                        && !self.group_departure_peer(expected)
                    {
                        return Err(TransportError::AdmissionDenied);
                    }
                    self.note_dialed_route(addr, &channel);
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
    /// Only call after a successful terminal response, before reading a new
    /// frame. Ready bytes win a retirement race; registration ends before recv.
    pub(in crate::node) async fn recv_inbound_reuse(
        &self,
        channel: &mut SecureChannel<TcpStream>,
    ) -> Result<InboundReuse, TransportError> {
        if let Some((registration, retire)) = self.sessions.register_inbound_idle()
            && let Some(retired) = wait_registered_inbound(channel, registration, retire).await?
        {
            return Ok(InboundReuse::Retired(retired));
        }
        channel.recv().await.map(InboundReuse::Frame)
    }

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

    #[tokio::test]
    async fn parked_mailbox_watches_preserve_normal_capacity_and_release_on_drop() {
        let pool = SessionPool::default();
        let mut watches = Vec::new();
        for _ in 0..4 {
            watches.push(pool.watch_outbound().await.unwrap());
        }
        assert_eq!(pool.test_capacity_snapshot().0, MAX_TCP_CONNECTIONS);
        let mut ordinary = Vec::new();
        for _ in 0..MAX_DIRECTION_CONNECTIONS {
            ordinary.push(pool.outbound().await.unwrap());
        }
        let inbound = pool.inbound().await.unwrap();
        assert_eq!(pool.test_capacity_snapshot().0, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), pool.watch_outbound())
                .await
                .is_err()
        );
        drop(watches.pop());
        watches.push(pool.watch_outbound().await.unwrap());
        drop(watches);
        drop(ordinary);
        drop(inbound);
        assert_eq!(pool.test_capacity_snapshot().0, MAX_TCP_CONNECTIONS);
        let mut restored = Vec::new();
        for _ in 0..4 {
            restored.push(pool.watch_outbound().await.unwrap());
        }
    }

    fn historical_key() -> PeerEndpointKey {
        PeerEndpointKey {
            identity: crate::identity::device::DeviceIdentity::generate().public(),
            address: "127.0.0.1:47479".parse().unwrap(),
            generation: 1,
        }
    }

    #[test]
    fn failed_unchanged_historical_authentication_waits_for_sparse_repair() {
        let pool = SessionPool::default();
        let key = historical_key();
        let now = Instant::now();
        let mut attempt = pool.historical_authentication(&key, 1, now).unwrap();
        attempt.complete(false, now);
        for elapsed in [31, 60, 120, 299] {
            assert!(
                pool.historical_authentication(&key, 1, now + Duration::from_secs(elapsed))
                    .is_err(),
                "unchanged failed historical endpoint retried after {elapsed}s"
            );
        }
        assert!(
            pool.historical_authentication(&key, 1, now + Duration::from_secs(301))
                .is_ok(),
            "sparse repair must still recover an endpoint without new discovery"
        );
    }

    #[test]
    fn historical_authentication_retains_cancelled_work_without_learning_failure() {
        let pool = SessionPool::default();
        let key = historical_key();
        let now = Instant::now();
        drop(pool.historical_authentication(&key, 1, now).unwrap());
        assert!(pool.historical_authentication(&key, 1, now).is_err());
        let mut attempt = pool
            .historical_authentication(&key, 1, now + Duration::from_secs(6))
            .unwrap();
        attempt.complete(false, now + Duration::from_secs(6));
        assert!(
            pool.historical_authentication(&key, 1, now + Duration::from_secs(35))
                .is_err()
        );
        assert!(
            pool.historical_authentication(&key, 1, now + Duration::from_secs(307))
                .is_ok()
        );
    }

    #[test]
    fn historical_authentication_scope_and_late_cancellation_preserve_recovery() {
        let pool = SessionPool::default();
        let key = historical_key();
        let now = Instant::now();
        let mut first = pool.historical_authentication(&key, 1, now).unwrap();
        first.complete(false, now);
        let mapped = PeerEndpointKey {
            address: "[::ffff:127.0.0.1]:47479".parse().unwrap(),
            ..key.clone()
        };
        assert!(
            pool.historical_authentication(&mapped, 1, now).is_err(),
            "mapped addresses share the same failed endpoint"
        );
        for changed in [
            PeerEndpointKey {
                address: "127.0.0.1:47480".parse().unwrap(),
                ..key.clone()
            },
            PeerEndpointKey {
                generation: 2,
                ..key.clone()
            },
            PeerEndpointKey {
                identity: crate::identity::device::DeviceIdentity::generate().public(),
                ..key.clone()
            },
        ] {
            assert!(pool.historical_authentication(&changed, 1, now).is_ok());
        }
        // A changed network replaces this route's reservation. The old guard
        // must not defer that replacement when it later retires.
        let old = pool.historical_authentication(&key, 2, now).unwrap();
        let mut newer = pool.historical_authentication(&key, 3, now).unwrap();
        drop(old);
        newer.complete(true, now);
        assert!(pool.historical_authentication(&key, 3, now).is_ok());
        pool.authentication_succeeded(&key.identity, key.address, Duration::from_millis(5));
        assert!(
            pool.historical_authentication(&key, 3, now).is_ok(),
            "a real authenticated recovery clears earlier pacing"
        );
    }

    #[test]
    fn historical_authentication_retry_inventory_is_bounded_and_reclaims_expired_slots() {
        let pool = SessionPool::default();
        let now = Instant::now();
        let mut key = historical_key();
        let mut held = Vec::new();
        for index in 0..256u64 {
            key.identity.ed25519_pub[..8].copy_from_slice(&index.to_le_bytes());
            held.push(pool.historical_authentication(&key, 1, now).unwrap());
        }
        key.identity.ed25519_pub[..8].copy_from_slice(&256u64.to_le_bytes());
        let error = pool.historical_authentication(&key, 1, now).err().unwrap();
        assert!(
            local_capacity_busy(&error),
            "hint capacity is not a remote failure"
        );
        drop(held);
        assert!(
            pool.historical_authentication(&key, 1, now + Duration::from_secs(6))
                .is_ok()
        );
    }

    #[test]
    fn slow_authentication_diagnostics_coalesce_background_and_preserve_interactive_reports() {
        let now = Instant::now();
        let mut diagnostics = SlowAuthenticationDiagnostics::default();
        assert!(
            diagnostics
                .observe(now, Duration::from_secs(1), false, true, false)
                .is_none()
        );
        assert_eq!(
            diagnostics.observe(now, Duration::from_secs(10), false, true, false),
            Some(SlowAuthenticationSummary {
                attempts: 1,
                failures: 1,
                historical: 1,
                longest: Duration::from_secs(10)
            })
        );
        assert!(
            diagnostics
                .observe(
                    now + Duration::from_secs(1),
                    Duration::from_secs(10),
                    false,
                    true,
                    false
                )
                .is_none()
        );
        assert!(
            diagnostics
                .observe(
                    now + Duration::from_secs(2),
                    Duration::from_secs(3),
                    true,
                    false,
                    false
                )
                .is_none()
        );
        assert_eq!(
            diagnostics
                .observe(
                    now + Duration::from_secs(299),
                    Duration::from_secs(4),
                    false,
                    false,
                    true
                )
                .unwrap()
                .attempts,
            1
        );
        assert_eq!(
            diagnostics.observe(
                now + Duration::from_secs(300),
                Duration::from_secs(8),
                false,
                true,
                false
            ),
            Some(SlowAuthenticationSummary {
                attempts: 3,
                failures: 2,
                historical: 2,
                longest: Duration::from_secs(10)
            })
        );
        assert!(
            diagnostics
                .observe(
                    now + Duration::from_secs(301),
                    Duration::from_secs(10),
                    false,
                    true,
                    false
                )
                .is_none(),
            "interactive reports never reset the background limit"
        );
        let mut independent = SlowAuthenticationDiagnostics::default();
        assert!(
            independent
                .observe(now, Duration::from_secs(3), false, false, false)
                .is_some()
        );
    }

    #[test]
    fn slow_authentication_diagnostics_bound_pending_counters_without_allocating_routes() {
        let now = Instant::now();
        let mut diagnostics = SlowAuthenticationDiagnostics {
            next_report: Some(now + Duration::from_secs(300)),
            pending: SlowAuthenticationSummary {
                attempts: u64::MAX,
                failures: u64::MAX,
                historical: u64::MAX,
                longest: Duration::from_secs(10),
            },
        };
        assert!(
            diagnostics
                .observe(now, Duration::from_secs(3), false, true, false)
                .is_none()
        );
        assert_eq!(
            diagnostics
                .observe(
                    now + Duration::from_secs(300),
                    Duration::from_secs(3),
                    false,
                    true,
                    false
                )
                .unwrap()
                .attempts,
            u64::MAX
        );
    }

    #[tokio::test]
    async fn historical_budget_is_shared_and_cancellation_releases_actual_owner() {
        let pool = Arc::new(SessionPool::default());
        let first = pool.historical_dial().unwrap();
        let second = pool.historical_dial().unwrap();
        assert!(!pool.historical_capacity_available());
        assert!(local_capacity_busy(&pool.historical_dial().unwrap_err()));
        // Historical pressure cannot consume the independent global/directional
        // capacity needed by fresh peers and explicit interactive operations.
        let live = pool.outbound().await.unwrap();
        let task = tokio::spawn(async move {
            let _owner = second;
            std::future::pending::<()>().await;
        });
        task.abort();
        let _ = task.await;
        assert!(pool.historical_capacity_available());
        assert!(pool.historical_dial().is_ok());
        drop(first);
        drop(live);
        assert_eq!(pool.historical.available_permits(), 2);
    }

    #[test]
    fn authenticated_resource_hint_is_exact_expiring_bounded_and_resettable() {
        let pool = SessionPool::default();
        let key = authentication_key(1);
        let now = Instant::now();
        pool.remember_authenticated_route(key.clone(), key.address, now);
        assert!(pool.recently_authenticated_route(&key, now));
        assert!(!pool.recently_authenticated_route(&key, now + Duration::from_secs(15)));
        let mut different = key.clone();
        different.generation += 1;
        assert!(!pool.recently_authenticated_route(&different, now));
        different = key.clone();
        different.address.set_port(2);
        assert!(!pool.recently_authenticated_route(&different, now));
        different = key.clone();
        different.identity = authentication_key(1).identity;
        assert!(!pool.recently_authenticated_route(&different, now));
        for port in 2..=MAX_ROUTE_AFFINITIES as u16 + 1 {
            pool.remember_authenticated_route(authentication_key(port), key.address, now);
        }
        assert_eq!(
            pool.route_affinities.lock().unwrap().len(),
            MAX_ROUTE_AFFINITIES
        );
        assert!(!pool.recently_authenticated_route(&key, now));
        pool.clear();
        assert!(pool.route_affinities.lock().unwrap().is_empty());
    }

    #[test]
    fn local_capacity_backpressure_never_classifies_identity_or_protocol_failures_as_busy() {
        for message in [
            "peer authentication capacity busy",
            "TCP connection capacity busy",
            "historical route capacity busy",
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

    #[tokio::test]
    async fn ready_encrypted_frame_wins_idle_retirement_and_cancellation_removes_its_token() {
        use crate::identity::device::DeviceIdentity;
        use tokio::net::TcpListener;
        let first = DeviceIdentity::generate();
        let second = DeviceIdentity::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sending, receiving) = tokio::join!(
            async {
                let stream = TcpStream::connect(address).await.unwrap();
                SecureChannel::connect(stream, &first, Some(&second.public()))
                    .await
                    .unwrap()
            },
            async {
                let (stream, _) = listener.accept().await.unwrap();
                SecureChannel::accept(stream, &second).await.unwrap()
            }
        );
        let (mut sending, mut receiving) = (sending, receiving);
        let pool = SessionPool::default();
        let (registration, retire) = pool.register_inbound_idle().unwrap();
        let mut waiting = Box::pin(wait_registered_inbound(&receiving, registration, retire));
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(waiting.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(waiting);
        assert!(
            pool.inbound_idle.state.lock().unwrap().entries.is_empty(),
            "cancelled idle reception cannot retain a stale eviction token"
        );
        sending
            .send(b"already arrived framed request")
            .await
            .unwrap();
        receiving.inbound_frame_ready().await.unwrap();
        let (registration, retire) = pool.register_inbound_idle().unwrap();
        pool.retire_oldest_inbound_idle();
        assert!(
            wait_registered_inbound(&receiving, registration, retire)
                .await
                .unwrap()
                .is_none(),
            "already-ready bytes must win simultaneous retirement"
        );
        assert!(pool.inbound_idle.state.lock().unwrap().entries.is_empty());
        assert_eq!(
            receiving.recv().await.unwrap(),
            b"already arrived framed request",
            "readiness and retirement must never consume or corrupt a framed request"
        );
    }

    #[tokio::test]
    async fn pending_total_admission_retires_newly_idle_connection_only_after_owner_drops() {
        use crate::identity::device::DeviceIdentity;
        use tokio::net::TcpListener;
        let first = DeviceIdentity::generate();
        let second = DeviceIdentity::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (mut sending, mut receiving) = tokio::join!(
            async {
                let stream = TcpStream::connect(address).await.unwrap();
                SecureChannel::connect(stream, &first, Some(&second.public()))
                    .await
                    .unwrap()
            },
            async {
                let (stream, _) = listener.accept().await.unwrap();
                SecureChannel::accept(stream, &second).await.unwrap()
            }
        );
        let pool = SessionPool::default();
        receiving.retain_connection_permits(pool.inbound().await.unwrap());
        let outbound: Vec<_> = (0..MAX_DIRECTION_CONNECTIONS)
            .map(|_| pool.try_outbound().unwrap())
            .collect();
        sending.send(b"request").await.unwrap();
        assert_eq!(receiving.recv().await.unwrap(), b"request");
        receiving.send(b"complete").await.unwrap();
        assert_eq!(sending.recv().await.unwrap(), b"complete");
        assert_eq!(pool.total.available_permits(), 0);
        let admission = pool.inbound();
        tokio::pin!(admission);
        assert!(futures_util::poll!(&mut admission).is_pending());
        // The completed real connection becomes eligible only AFTER admission
        // is already queued. No new dial or unrelated capacity check wakes it.
        let (registration, retire) = pool.register_inbound_idle().unwrap();
        let mut waiting = Box::pin(wait_registered_inbound(&receiving, registration, retire));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        assert!(futures_util::poll!(&mut admission).is_pending());
        let registration = match futures_util::poll!(&mut waiting) {
            std::task::Poll::Ready(Ok(Some(registration))) => registration,
            _ => panic!("queued total admission must retire the newly idle connection"),
        };
        assert_eq!(pool.total.available_permits(), 0);
        assert!(
            futures_util::poll!(&mut admission).is_pending(),
            "retirement notification must not manufacture capacity before the real channel drops"
        );
        drop(waiting);
        drop(receiving);
        let admitted = admission.await.unwrap();
        drop(registration);
        drop(admitted);
        drop(outbound);
        assert_eq!(pool.total.available_permits(), MAX_TCP_CONNECTIONS);
    }

    #[tokio::test]
    async fn inbound_idle_retirement_is_bounded_and_old_guards_cannot_remove_new_entries() {
        let pool = SessionPool::default();
        let (first, mut retired_first) = pool.register_inbound_idle().unwrap();
        let (second, mut retired_second) = pool.register_inbound_idle().unwrap();
        for _ in 0..MAX_DIRECTION_CONNECTIONS {
            pool.retire_oldest_inbound_idle();
        }
        assert!(retired_first.try_recv().is_ok());
        assert!(
            matches!(
                retired_second.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "repeated pressure must wait for the first owner to release its retirement guard"
        );
        drop(first);
        assert_eq!(pool.inbound_idle.state.lock().unwrap().entries.len(), 1);
        pool.retire_oldest_inbound_idle();
        assert!(retired_second.try_recv().is_ok());
        drop(second);
        let registrations: Vec<_> = (0..MAX_DIRECTION_CONNECTIONS)
            .map(|_| pool.register_inbound_idle().unwrap())
            .collect();
        assert!(pool.register_inbound_idle().is_none());
        drop(registrations);
        assert!(pool.inbound_idle.state.lock().unwrap().entries.is_empty());
        assert_eq!(
            pool.total.available_permits(),
            MAX_TCP_CONNECTIONS,
            "idle registration and cancellation never manufacture or release real TCP permits"
        );
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
    async fn control_authentication_overtakes_only_queued_ordinary_handshakes() {
        let pool = SessionPool::default();
        let key = authentication_key(1);
        let active = pool.authentication_admission(key.clone()).await.unwrap();
        let ordinary = pool.authentication_admission(key.clone());
        tokio::pin!(ordinary);
        assert!(futures_util::poll!(&mut ordinary).is_pending());
        let control =
            pool.authentication_admission_priority(key.clone(), AuthenticationPriority::Control);
        tokio::pin!(control);
        assert!(futures_util::poll!(&mut control).is_pending());
        assert_eq!(pool.outbound.available_permits(), MAX_DIRECTION_CONNECTIONS);
        drop(active);
        let first = tokio::time::timeout(CAPACITY_WAIT, async {
            tokio::select! {
                result = &mut control => { drop(result.unwrap()); AuthenticationPriority::Control }
                result = &mut ordinary => { drop(result.unwrap()); AuthenticationPriority::Ordinary }
            }
        }).await.unwrap();
        assert_eq!(
            first,
            AuthenticationPriority::Control,
            "an explicit Control handshake must overtake an earlier queued ordinary Wake without cancelling the active handshake"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn authentication_priority_preserves_class_fifo_and_one_control_then_ordinary_fairness() {
        let pool = SessionPool::default();
        let key = authentication_key(1);
        let active = pool.authentication_admission(key.clone()).await.unwrap();
        let ordinary_first = pool.authentication_admission(key.clone());
        let ordinary_second = pool.authentication_admission(key.clone());
        let control_first =
            pool.authentication_admission_priority(key.clone(), AuthenticationPriority::Control);
        let control_second =
            pool.authentication_admission_priority(key, AuthenticationPriority::Control);
        tokio::pin!(
            ordinary_first,
            ordinary_second,
            control_first,
            control_second
        );
        assert!(futures_util::poll!(&mut ordinary_first).is_pending());
        assert!(futures_util::poll!(&mut ordinary_second).is_pending());
        assert!(futures_util::poll!(&mut control_first).is_pending());
        assert!(futures_util::poll!(&mut control_second).is_pending());
        drop(active);
        assert!(futures_util::poll!(&mut control_second).is_pending());
        let first = control_first.await.unwrap();
        assert!(futures_util::poll!(&mut ordinary_first).is_pending());
        drop(first);
        assert!(futures_util::poll!(&mut control_second).is_pending());
        assert!(futures_util::poll!(&mut ordinary_second).is_pending());
        let ordinary = ordinary_first.await.unwrap();
        drop(ordinary);
        let control = control_second.await.unwrap();
        drop(control);
        assert!(ordinary_second.await.is_ok());
        assert_eq!(pool.total.available_permits(), MAX_TCP_CONNECTIONS);
    }

    #[tokio::test(start_paused = true)]
    async fn authentication_priority_cancellation_and_timeouts_release_waiter_turns() {
        let pool = SessionPool::default();
        let key = authentication_key(1);
        let active = pool.authentication_admission(key.clone()).await.unwrap();
        {
            let cancelled = pool
                .authentication_admission_priority(key.clone(), AuthenticationPriority::Control);
            tokio::pin!(cancelled);
            assert!(futures_util::poll!(&mut cancelled).is_pending());
        }
        assert!(
            pool.authentication_admission_priority(key.clone(), AuthenticationPriority::Control)
                .await
                .is_err()
        );
        drop(active);
        let control = pool
            .authentication_admission_priority(key.clone(), AuthenticationPriority::Control)
            .await
            .unwrap();
        {
            let cancelled = pool.authentication_admission(key.clone());
            tokio::pin!(cancelled);
            assert!(futures_util::poll!(&mut cancelled).is_pending());
        }
        drop(control);
        assert!(
            pool.authentication_admission_priority(key.clone(), AuthenticationPriority::Control)
                .await
                .is_ok()
        );
        assert!(pool.authentication_admission(key).await.is_ok());
        assert_eq!(pool.authentication_gates.lock().unwrap().len(), 1);
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
            let _guard = pool
                .authentication_admission_priority(independent, AuthenticationPriority::Control)
                .await
                .unwrap();
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
            pool.authentication_admission_priority(
                authentication_key(300),
                AuthenticationPriority::Control
            )
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
