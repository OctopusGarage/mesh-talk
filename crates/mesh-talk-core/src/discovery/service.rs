//! The discovery service: a broadcast loop (advertise our announce) and a listen
//! loop (record peers). The datagram handler is pure so it can be unit-tested;
//! the loops are thin wrappers over a `tokio` UDP socket.

use crate::discovery::DiscoveryVisibility;
use crate::discovery::announce::{Announce, decode, encode};
use crate::discovery::query::{self, Nonce, QueryTracker};
use crate::discovery::roster::{Roster, UpdateOutcome};
use crate::transport::net::{ipv4_interface_addrs, join_discovery_group_all_ifaces};
use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// A shared, swappable encoded announce. The discovery loops read the current bytes
/// on every send, so swapping in a freshly re-signed announce (e.g. on a display-name
/// change) propagates live without restarting discovery. Cheap to read: each send
/// clones the inner `Arc`, never the bytes.
pub type SharedAnnounce = Arc<Mutex<Arc<Vec<u8>>>>;

/// Encode `announce` once and wrap it as a [`SharedAnnounce`].
pub fn shared_announce(announce: &Announce) -> SharedAnnounce {
    Arc::new(Mutex::new(Arc::new(encode(announce))))
}

/// Re-encode `announce` and swap it into `shared` so the discovery loops advertise it
/// from their next send onward — the live-rename path (no discovery restart needed).
pub fn swap_announce(shared: &SharedAnnounce, announce: &Announce) {
    *shared.lock().expect("announce mutex not poisoned") = Arc::new(encode(announce));
}

/// Snapshot the current encoded announce (an `Arc` clone — no byte copy).
fn current_announce(shared: &SharedAnnounce) -> Arc<Vec<u8>> {
    Arc::clone(&shared.lock().expect("announce mutex not poisoned"))
}

type ResponseSigner = Arc<dyn Fn(Nonce, &Announce) -> Option<Vec<u8>> + Send + Sync>;

/// Generic unicast discovery state. Candidate enumeration belongs to the host;
/// this component only sends identity-free queries and processes their replies.
pub struct RoutedDiscovery {
    pending: Mutex<QueryTracker>,
    signer: ResponseSigner,
    replies: Mutex<ResponseBudget>,
}

#[derive(Default)]
struct ResponseBudget {
    window: Option<Instant>,
    sent: u8,
    recent: HashMap<IpAddr, Instant>,
}

impl ResponseBudget {
    fn allow(&mut self, source: IpAddr, now: Instant) -> bool {
        if self
            .window
            .is_none_or(|start| now.saturating_duration_since(start) >= Duration::from_secs(1))
        {
            self.window = Some(now);
            self.sent = 0;
        }
        self.recent
            .retain(|_, sent| now.saturating_duration_since(*sent) < Duration::from_secs(1));
        if self.sent >= 8 || self.recent.contains_key(&source) || self.recent.len() >= 256 {
            return false;
        }
        self.sent += 1;
        self.recent.insert(source, now);
        true
    }
}

impl RoutedDiscovery {
    pub fn new(signer: ResponseSigner) -> Self {
        Self {
            pending: Mutex::new(QueryTracker::default()),
            signer,
            replies: Mutex::new(ResponseBudget::default()),
        }
    }

    /// One bounded query to an explicitly supplied candidate endpoint.
    pub async fn probe(&self, socket: &UdpSocket, target: SocketAddr) -> std::io::Result<bool> {
        self.probe_if(socket, target, |commit| commit()).await
    }

    pub(crate) async fn probe_if(
        &self,
        socket: &UdpSocket,
        target: SocketAddr,
        mut authorize: impl FnMut(&mut dyn FnMut() -> std::io::Result<()>) -> std::io::Result<()> + Send,
    ) -> std::io::Result<bool> {
        let nonce = uuid::Uuid::new_v4().into_bytes();
        loop {
            socket.writable().await?;
            let mut sent = false;
            let result = authorize(&mut || {
                if !self
                    .pending
                    .lock()
                    .expect("query lock not poisoned")
                    .record(target.ip(), nonce, Instant::now())
                {
                    return Ok(());
                }
                socket.try_send_to(&query::encode_query(nonce), target)?;
                sent = true;
                Ok(())
            });
            match result {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error),
                Ok(()) => return Ok(sent),
            }
        }
    }
}

/// Decode + verify + record one received datagram. Returns the [`UpdateOutcome`]
/// (`New` on first sight of a peer — used by the listener to fire a unicast reply
/// exactly once). Pure (apart from locking the roster) — no I/O.
pub fn handle_datagram(
    roster: &Mutex<Roster>,
    data: &[u8],
    source: SocketAddr,
    self_user_id: &str,
) -> UpdateOutcome {
    handle_datagram_with_scope(roster, data, source, self_user_id, true)
}

fn handle_datagram_with_scope(
    roster: &Mutex<Roster>,
    data: &[u8],
    source: SocketAddr,
    self_user_id: &str,
    local_scope: bool,
) -> UpdateOutcome {
    let Some(announce) = decode(data) else {
        return UpdateOutcome::Rejected;
    };
    let mut roster = roster.lock().expect("roster mutex not poisoned");
    if local_scope {
        roster.update(&announce, source.ip(), self_user_id)
    } else {
        roster.update_unscoped(&announce, source.ip(), self_user_id)
    }
}

/// Peers not heard from within this window are evicted from the roster. Broadcast
/// interval is ~2s, so this tolerates ~15 missed announces before dropping a peer —
/// long enough to ride out brief loss, short enough that stale addresses (a peer that
/// went offline or changed IP) don't linger and misdirect fan-out / post-office picks.
const PEER_TTL: Duration = Duration::from_secs(30);

/// Listen for announces on `socket`, feeding the roster, until a fatal socket
/// error or task cancellation. Peer reset/refusal errors are transient and do not
/// end discovery. Also evicts stale peers each iteration —
/// after every datagram (busy network) and on a `PEER_TTL` recv timeout (silent
/// network) — so a roster never grows unbounded or retains dead endpoints.
///
/// Announce/response (LocalSend's mechanism): on FIRST sight of a peer
/// ([`UpdateOutcome::New`]) we unicast our own announce straight back to the
/// datagram's source IP at the discovery port (`discovery_port`, NOT the peer's
/// TCP port). This makes discovery converge in <1s and survive one-directional
/// multicast loss. Replying only on first sight — not on every periodic
/// re-announce — avoids a 2-second reply storm. Our own announce is self-filtered
/// by `user_id`, so we never reply to ourselves.
pub async fn run_listen(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    self_user_id: String,
    self_announce: SharedAnnounce,
    discovery_port: u16,
) {
    run_listen_with_visibility(
        socket,
        roster,
        self_user_id,
        self_announce,
        discovery_port,
        Arc::new(DiscoveryVisibility::default()),
    )
    .await;
}

async fn run_listen_with_visibility(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    self_user_id: String,
    self_announce: SharedAnnounce,
    discovery_port: u16,
    visibility: Arc<DiscoveryVisibility>,
) {
    run_listen_with_routed(
        socket,
        roster,
        self_user_id,
        self_announce,
        discovery_port,
        visibility,
        None,
    )
    .await;
}

fn retryable_udp_receive_error(error: &std::io::Error) -> bool {
    // Unconnected UDP sockets may surface ICMP errors from an earlier send as
    // receive errors (notably WSAECONNRESET 10054 on Windows). A departed peer
    // does not invalidate the local discovery socket or its other peers.
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused
    )
}

async fn run_listen_with_routed(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    self_user_id: String,
    self_announce: SharedAnnounce,
    discovery_port: u16,
    visibility: Arc<DiscoveryVisibility>,
    routed: Option<Arc<RoutedDiscovery>>,
) {
    let mut buf = vec![0u8; 2048];
    let mut physical_scope = visibility.discovery_scope();
    let mut scope_refreshed = Instant::now();
    loop {
        match tokio::time::timeout(PEER_TTL, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => {
                if !visibility.accepts_source(source) {
                    continue;
                }
                if let Some(routed) = &routed {
                    if let Some(nonce) = query::decode_query(&buf[..n]) {
                        let allowed = routed
                            .replies
                            .lock()
                            .expect("reply lock not poisoned")
                            .allow(source.ip(), Instant::now());
                        if allowed
                            && let Some(announce) = decode(&current_announce(&self_announce))
                            && let Some(reply) = (routed.signer)(nonce, &announce)
                        {
                            let _ = visibility.send_announce(&socket, &reply, source).await;
                        }
                        continue;
                    }
                    if n >= 4 && &buf[..4] == b"MTDR" {
                        let answer = routed
                            .pending
                            .lock()
                            .expect("query lock not poisoned")
                            .accept(source.ip(), &buf[..n], Instant::now());
                        if let Some(announce) = answer {
                            let outcome = roster
                                .lock()
                                .expect("roster mutex not poisoned")
                                .update_routed(&announce, source.ip(), &self_user_id);
                            if outcome == UpdateOutcome::New {
                                let bytes = current_announce(&self_announce);
                                let _ = visibility.send_announce(&socket, &bytes, source).await;
                            }
                        }
                        continue;
                    }
                }
                // Reuse a short-lived interface snapshot, not one OS inventory per peer.
                if scope_refreshed.elapsed() >= Duration::from_secs(2) {
                    physical_scope = visibility.discovery_scope();
                    scope_refreshed = Instant::now();
                }
                let outcome = handle_datagram_with_scope(
                    &roster,
                    &buf[..n],
                    source,
                    &self_user_id,
                    crate::discovery::scope::permits(&physical_scope, source),
                );
                if outcome == UpdateOutcome::New {
                    // First sight → unicast our (current) announce back to the peer's
                    // discovery port so the link is recorded symmetrically.
                    let reply_target = SocketAddr::new(source.ip(), discovery_port);
                    let bytes = current_announce(&self_announce);
                    let _ = visibility
                        .send_announce(&socket, &bytes, reply_target)
                        .await;
                }
            }
            Ok(Err(error)) if retryable_udp_receive_error(&error) => {
                log::debug!("discovery ignored a transient UDP peer error: {error}");
            }
            Ok(Err(error)) => {
                log::warn!(
                    "discovery UDP listener stopped: {error}; kind={:?}, os_error={:?}",
                    error.kind(),
                    error.raw_os_error()
                );
                break;
            }
            Err(_) => {} // recv timed out → fall through to evict
        }
        roster
            .lock()
            .expect("roster mutex not poisoned")
            .evict_stale(PEER_TTL);
    }
}

/// A staggered startup burst of announces (offsets from start), the way LocalSend
/// fires several quick multicast announces at launch to beat UDP packet loss and
/// converge first-contact fast. Each is idempotent — peers self-filter dupes by
/// `user_id` — so an extra few packets are harmless. Sent before the steady loop.
const BROADCAST_STARTUP_BURST_MS: [u64; 3] = [100, 500, 2000];

/// Periodically send our `announce` to `target` (the multicast group in
/// production) over `socket`, every `interval`. `socket` must already be
/// configured for the target (e.g. broadcast-enabled if `target` is a broadcast
/// address); this function only sends.
///
/// At startup it first fires a staggered burst ([`BROADCAST_STARTUP_BURST_MS`]) so
/// first-contact converges fast even if an early multicast packet is dropped, then
/// settles into the steady `interval` loop. If `trigger` is supplied, a notify on it
/// fires an immediate extra announce (used by the manual "announce now" control).
///
/// A transient `send_to` error (e.g. WiFi not yet up at launch, or the network
/// dropping out) is ignored — the loop keeps ticking and retries next interval,
/// so discovery self-heals when the network returns rather than dying silently.
pub async fn run_broadcast(
    socket: Arc<UdpSocket>,
    announce: SharedAnnounce,
    target: SocketAddr,
    interval: Duration,
    trigger: Option<Arc<Notify>>,
) {
    run_broadcast_with_visibility(
        socket,
        announce,
        target,
        interval,
        trigger,
        Arc::new(DiscoveryVisibility::default()),
    )
    .await;
}

async fn run_broadcast_with_visibility(
    socket: Arc<UdpSocket>,
    announce: SharedAnnounce,
    target: SocketAddr,
    interval: Duration,
    trigger: Option<Arc<Notify>>,
    visibility: Arc<DiscoveryVisibility>,
) {
    // Startup offsets are measured from launch, not added to one another. Manual
    // requests remain responsive even while the burst is in progress.
    let started = tokio::time::Instant::now();
    for offset_ms in BROADCAST_STARTUP_BURST_MS {
        let deadline = started + Duration::from_millis(offset_ms);
        loop {
            if let Some(notify) = &trigger {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    _ = notify.notified() => {}
                }
            } else {
                tokio::time::sleep_until(deadline).await;
                break;
            }
            let bytes = current_announce(&announce);
            let _ = visibility.send_announce(&socket, &bytes, target).await;
        }
        let bytes = current_announce(&announce);
        let _ = visibility.send_announce(&socket, &bytes, target).await;
    }
    let mut tick = tokio::time::interval(interval);
    tick.tick().await; // consume the immediate first tick (the burst just sent)
    loop {
        // Steady re-announce on the timer, OR an immediate one when triggered.
        match &trigger {
            Some(notify) => tokio::select! {
                _ = tick.tick() => {}
                _ = notify.notified() => {}
            },
            None => {
                tick.tick().await;
            }
        }
        // Read the current announce each send so a runtime rename takes effect here.
        // Ignore send errors: the network may be transiently down. Retry next tick.
        let bytes = current_announce(&announce);
        let _ = visibility.send_announce(&socket, &bytes, target).await;
    }
}

/// The discovery port unicast-scan cadence: offsets from launch for quick startup
/// sweeps so a peer that came up just before us is found fast, then a steady
/// slow interval (multicast/broadcast carry the steady-state load; the scan is
/// the fallback for when those are blocked but unicast is allowed).
const SCAN_STARTUP_DELAYS_SECS: [u64; 3] = [1, 3, 7];
const SCAN_STEADY_INTERVAL: Duration = Duration::from_secs(5);

/// Unicast-UDP /24 scan fallback (the analog of LocalSend's HTTP /24 scan, adapted
/// to our fixed UDP discovery port). For each local non-loopback IPv4 interface,
/// derive its /24 and send our `announce` to `x.y.z.1..=254` at `discovery_port`,
/// deduplicating shared subnets and skipping all of our addresses. The receiver's
/// normal listen loop records us directly (and, on first sight, replies), so two
/// scanning sides converge symmetrically even when multicast/broadcast is dropped.
/// Send errors are ignored per-target.
pub async fn run_scan(
    socket: Arc<UdpSocket>,
    announce: SharedAnnounce,
    discovery_port: u16,
    trigger: Option<Arc<Notify>>,
) {
    run_scan_with_visibility(
        socket,
        announce,
        discovery_port,
        trigger,
        Arc::new(DiscoveryVisibility::default()),
    )
    .await;
}

async fn run_scan_with_visibility(
    socket: Arc<UdpSocket>,
    announce: SharedAnnounce,
    discovery_port: u16,
    trigger: Option<Arc<Notify>>,
    visibility: Arc<DiscoveryVisibility>,
) {
    let started = tokio::time::Instant::now();
    for delay in SCAN_STARTUP_DELAYS_SECS {
        let deadline = started + Duration::from_secs(delay);
        loop {
            if let Some(notify) = &trigger {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    _ = notify.notified() => {}
                }
            } else {
                tokio::time::sleep_until(deadline).await;
                break;
            }
            let bytes = current_announce(&announce);
            scan_once_with_visibility(&socket, &bytes, discovery_port, &visibility).await;
        }
        let bytes = current_announce(&announce);
        scan_once_with_visibility(&socket, &bytes, discovery_port, &visibility).await;
    }
    let mut tick = tokio::time::interval(SCAN_STEADY_INTERVAL);
    tick.tick().await; // consume the immediate first tick (we just scanned)
    loop {
        // Steady sweep on the timer, OR an immediate one when triggered.
        match &trigger {
            Some(notify) => tokio::select! {
                _ = tick.tick() => {}
                _ = notify.notified() => {}
            },
            None => {
                tick.tick().await;
            }
        }
        // Re-read the current announce each sweep so a runtime rename takes effect.
        let bytes = current_announce(&announce);
        scan_once_with_visibility(&socket, &bytes, discovery_port, &visibility).await;
    }
}

/// The set of /24 hosts to scan for a given own address: `x.y.z.1..=254` minus
/// our own address. Pure (no I/O) so the target-derivation logic can be unit
/// tested without real interfaces or sockets.
fn scan_targets(own: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, _] = own.octets();
    (1u8..=254)
        .map(|host| Ipv4Addr::new(a, b, c, host))
        .filter(|target| *target != own)
        .collect()
}

fn scan_targets_for_interfaces(own: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    let local: BTreeSet<_> = own.iter().copied().collect();
    own.iter()
        // The Tailscale interface is routed, not a LAN. Scanning its /24 would
        // send our signed identity announce to unrelated tailnet machines.
        .filter(|addr| {
            let octets = addr.octets();
            !(octets[0] == 100 && (64..128).contains(&octets[1]))
        })
        .flat_map(|addr| scan_targets(*addr))
        .filter(|target| !local.contains(target))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// One sweep: enumerate interfaces fresh (they may change) and unicast `bytes` to
/// each unique /24 host except our own addresses. Fire-and-forget; errors ignored.
#[cfg(test)]
async fn scan_once(socket: &UdpSocket, bytes: &[u8], discovery_port: u16) {
    scan_once_with_visibility(
        socket,
        bytes,
        discovery_port,
        &DiscoveryVisibility::default(),
    )
    .await;
}

async fn scan_once_with_visibility(
    socket: &UdpSocket,
    bytes: &[u8],
    discovery_port: u16,
    visibility: &DiscoveryVisibility,
) {
    let interfaces = if visibility.physical_lan_only() {
        visibility.physical_ipv4_addrs()
    } else {
        ipv4_interface_addrs()
    };
    let targets = scan_targets_for_interfaces(&interfaces)
        .into_iter()
        .map(|ip| SocketAddr::new(IpAddr::V4(ip), discovery_port))
        // A /24 fallback scan can exceed the real interface netmask. Skip
        // disallowed targets instead of ending the scan before another LAN.
        .filter(|target| visibility.accepts_source(*target));
    let _ = scan_addresses(socket, bytes, targets, visibility).await;
}

async fn scan_addresses(
    socket: &UdpSocket,
    bytes: &[u8],
    targets: impl IntoIterator<Item = SocketAddr>,
    visibility: &DiscoveryVisibility,
) -> bool {
    for target in targets {
        if matches!(
            visibility.send_announce(socket, bytes, target).await,
            Ok(false)
        ) {
            return false;
        }
    }
    true
}

/// Periodic multicast re-join: every `REJOIN_INTERVAL`, re-enumerate interfaces and
/// re-join the discovery group on each. Picks up interfaces that come up after
/// launch (WiFi associating late, network changes). Joining an already-joined or
/// down interface just errors harmlessly (ignored inside the helper).
const REJOIN_INTERVAL: Duration = Duration::from_secs(30);

async fn run_rejoin(socket: Arc<UdpSocket>) {
    let mut tick = tokio::time::interval(REJOIN_INTERVAL);
    tick.tick().await; // the bind already joined once; skip the immediate tick
    loop {
        tick.tick().await;
        join_discovery_group_all_ifaces(&socket);
    }
}

/// Spawn the full discovery task set on one shared `socket` and return their
/// handles: listen (with announce/response reply), multicast broadcast (2s),
/// unicast /24 scan fallback, and periodic multicast re-join. The three call sites
/// (desktop runtime, CLI node, post office) all use this — they only differ in the
/// prebuilt `announce` they pass.
pub fn spawn_discovery(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    announce: Announce,
    self_user_id: String,
    discovery_port: u16,
) -> Vec<JoinHandle<()>> {
    spawn_discovery_with_trigger(
        socket,
        roster,
        shared_announce(&announce),
        self_user_id,
        discovery_port,
        None,
    )
}

/// As [`spawn_discovery`], but the broadcast and scan loops also watch `trigger`:
/// a `notify_waiters()` on it fires an immediate re-announce AND an immediate /24
/// sweep (in addition to their timers). The desktop runtime wires this to a manual
/// "announce now / rescan" control so a user can force first-contact when LAN
/// discovery is flaky. Pass `None` (or use [`spawn_discovery`]) when no manual
/// trigger is needed.
pub fn spawn_discovery_with_trigger(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    announce: SharedAnnounce,
    self_user_id: String,
    discovery_port: u16,
    trigger: Option<Arc<Notify>>,
) -> Vec<JoinHandle<()>> {
    spawn_discovery_with_visibility(
        socket,
        roster,
        announce,
        self_user_id,
        discovery_port,
        trigger,
        Arc::new(DiscoveryVisibility::default()),
    )
}

/// Spawn discovery with a shared disclosure gate. Private visibility suppresses
/// all UDP identity advertisements, including replies and triggered scans, but
/// retains passive discovery and multicast membership. This is NOT application
/// admission: the host must enforce account/author permissions separately.
/// Set the initial state before spawning to avoid a public startup burst.
pub fn spawn_discovery_with_visibility(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    announce: SharedAnnounce,
    self_user_id: String,
    discovery_port: u16,
    trigger: Option<Arc<Notify>>,
    visibility: Arc<DiscoveryVisibility>,
) -> Vec<JoinHandle<()>> {
    spawn_discovery_with_routed(
        socket,
        roster,
        announce,
        self_user_id,
        discovery_port,
        trigger,
        visibility,
        None,
    )
}

/// Launch LAN discovery plus optional routed-query handling on the same UDP socket.
pub fn spawn_discovery_with_routed(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    announce: SharedAnnounce,
    self_user_id: String,
    discovery_port: u16,
    trigger: Option<Arc<Notify>>,
    visibility: Arc<DiscoveryVisibility>,
    routed: Option<Arc<RoutedDiscovery>>,
) -> Vec<JoinHandle<()>> {
    let target: SocketAddr = (
        crate::transport::net::DISCOVERY_MULTICAST_GROUP,
        discovery_port,
    )
        .into();
    vec![
        tokio::spawn(run_listen_with_routed(
            Arc::clone(&socket),
            roster,
            self_user_id,
            Arc::clone(&announce),
            discovery_port,
            Arc::clone(&visibility),
            routed,
        )),
        tokio::spawn(run_broadcast_with_visibility(
            Arc::clone(&socket),
            Arc::clone(&announce),
            target,
            Duration::from_secs(2),
            trigger.clone(),
            Arc::clone(&visibility),
        )),
        tokio::spawn(run_scan_with_visibility(
            Arc::clone(&socket),
            announce,
            discovery_port,
            trigger,
            visibility,
        )),
        tokio::spawn(run_rejoin(socket)),
    ]
}

/// IPv6 uses unicast only; the existing IPv4 LAN multicast loop remains separate.
pub fn spawn_routed_listener(
    socket: Arc<UdpSocket>,
    roster: Arc<Mutex<Roster>>,
    announce: SharedAnnounce,
    self_user_id: String,
    discovery_port: u16,
    visibility: Arc<DiscoveryVisibility>,
    routed: Arc<RoutedDiscovery>,
) -> JoinHandle<()> {
    tokio::spawn(run_listen_with_routed(
        socket,
        roster,
        self_user_id,
        announce,
        discovery_port,
        visibility,
        Some(routed),
    ))
}

#[cfg(test)]
#[path = "privacy_tests.rs"]
mod privacy_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn udp_peer_reset_and_refusal_are_retryable_but_local_socket_failures_are_fatal() {
        use std::io::{Error, ErrorKind};
        for kind in [ErrorKind::ConnectionReset, ErrorKind::ConnectionRefused] {
            assert!(
                retryable_udp_receive_error(&Error::from(kind)),
                "peer error {kind:?} must not stop discovery"
            );
        }
        for kind in [
            ErrorKind::InvalidInput,
            ErrorKind::PermissionDenied,
            ErrorKind::NotConnected,
        ] {
            assert!(
                !retryable_udp_receive_error(&Error::from(kind)),
                "local socket failure {kind:?} must remain fatal"
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_listener_survives_icmp_reset_from_a_stopped_udp_peer() {
        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listen_addr = listener.local_addr().unwrap();
        let departed = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let departed_addr = departed.local_addr().unwrap();
        drop(departed);
        // Windows reports the ICMP port-unreachable reply as WSAECONNRESET on
        // this same unconnected socket's next recv_from, not as a send failure.
        listener
            .send_to(b"departed-peer", departed_addr)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), listener.readable())
            .await
            .expect("Windows should deliver the closed peer's ICMP error")
            .unwrap();

        let roster = Arc::new(Mutex::new(Roster::default()));
        let own = DeviceIdentity::generate();
        let peer = DeviceIdentity::generate();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let task = tokio::spawn(run_listen(
            Arc::clone(&listener),
            Arc::clone(&roster),
            own.user_id(),
            shared_announce(&Announce::new(&own, "local", 1)),
            sender.local_addr().unwrap().port(),
        ));
        let received = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                sender
                    .send_to(&encode(&Announce::new(&peer, "live", 4000)), listen_addr)
                    .await
                    .unwrap();
                if roster.lock().unwrap().get(&peer.user_id()).is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let exited = task.is_finished();
        task.abort();
        let _ = task.await;
        assert!(
            received.is_ok(),
            "fresh signed announces were lost after the UDP peer reset"
        );
        assert!(
            !exited,
            "discovery listener exited after a transient peer reset"
        );
    }

    use super::*;
    use crate::identity::device::DeviceIdentity;
    use std::net::{IpAddr, Ipv4Addr};

    fn source() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345)
    }

    #[tokio::test]
    async fn routed_query_discovers_a_peer_without_sending_our_identity_first() {
        let alice = Arc::new(DeviceIdentity::generate());
        let bob = Arc::new(DeviceIdentity::generate());
        let a_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let b_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let a_roster = Arc::new(Mutex::new(Roster::default()));
        let b_roster = Arc::new(Mutex::new(Roster::default()));
        let a_queries = Arc::new(RoutedDiscovery::new({
            let alice = Arc::clone(&alice);
            Arc::new(move |nonce, announce| query::encode_response(nonce, announce, &alice))
        }));
        let b_queries = Arc::new(RoutedDiscovery::new({
            let bob = Arc::clone(&bob);
            Arc::new(move |nonce, announce| query::encode_response(nonce, announce, &bob))
        }));
        let a_task = tokio::spawn(run_listen_with_routed(
            Arc::clone(&a_socket),
            Arc::clone(&a_roster),
            alice.user_id(),
            shared_announce(&Announce::new(&alice, "Alice", 41001)),
            47474,
            Arc::new(DiscoveryVisibility::default()),
            Some(Arc::clone(&a_queries)),
        ));
        let b_task = tokio::spawn(run_listen_with_routed(
            Arc::clone(&b_socket),
            Arc::clone(&b_roster),
            bob.user_id(),
            shared_announce(&Announce::new(&bob, "Bob", 41002)),
            47474,
            Arc::new(DiscoveryVisibility::default()),
            Some(b_queries),
        ));
        a_queries
            .probe(&a_socket, b_socket.local_addr().unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while a_roster.lock().unwrap().get(&bob.user_id()).is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("query discovered Bob");
        a_task.abort();
        b_task.abort();
    }

    #[tokio::test]
    async fn invisible_node_does_not_answer_an_unknown_routed_query() {
        let bob = Arc::new(DeviceIdentity::generate());
        let asker = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let responder = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let query_state = Arc::new(RoutedDiscovery::new({
            let bob = Arc::clone(&bob);
            Arc::new(move |nonce, announce| query::encode_response(nonce, announce, &bob))
        }));
        let task = tokio::spawn(run_listen_with_routed(
            Arc::clone(&responder),
            Arc::new(Mutex::new(Roster::default())),
            bob.user_id(),
            shared_announce(&Announce::new(&bob, "Bob", 41002)),
            47474,
            Arc::new(DiscoveryVisibility::new(false)),
            Some(query_state),
        ));
        asker
            .send_to(
                &query::encode_query([7; 16]),
                responder.local_addr().unwrap(),
            )
            .await
            .unwrap();
        let mut buf = [0u8; 2048];
        assert!(
            tokio::time::timeout(Duration::from_millis(150), asker.recv_from(&mut buf))
                .await
                .is_err()
        );
        task.abort();
    }

    #[test]
    fn signed_udp_without_established_scope_is_online_without_a_lan_claim() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new(&device, "Peer", 1234);
        let roster = Mutex::new(Roster::default());
        let address = "192.168.1.20:47474".parse().unwrap();
        assert_eq!(
            handle_datagram_with_scope(&roster, &encode(&proof), address, "self", false),
            UpdateOutcome::New
        );
        let guard = roster.lock().unwrap();
        let peer = guard.get(&device.user_id()).unwrap();
        assert!(!peer.has_lan_route());
        assert!(peer.has_direct_route());
        assert_eq!(
            peer.presence_evidence(Instant::now())[0].0,
            crate::discovery::roster::RouteSource::Discovery
        );
    }

    #[test]
    fn handle_datagram_records_a_valid_announce() {
        let alice = DeviceIdentity::generate();
        let roster = Mutex::new(Roster::default());
        let bytes = encode(&Announce::new(&alice, "Alice", 4000));
        assert_eq!(
            handle_datagram(&roster, &bytes, source(), "self"),
            UpdateOutcome::New
        );
        assert!(
            roster
                .lock()
                .unwrap()
                .get(&alice.public().user_id())
                .is_some()
        );
    }

    #[test]
    fn handle_datagram_self_filters_own_announce() {
        // A datagram carrying our OWN user_id is dropped (not recorded), even
        // though it is a valid announce.
        let me = DeviceIdentity::generate();
        let roster = Mutex::new(Roster::default());
        let bytes = encode(&Announce::new(&me, "Me", 4000));
        assert_eq!(
            handle_datagram(&roster, &bytes, source(), &me.public().user_id()),
            UpdateOutcome::Rejected
        );
        assert!(roster.lock().unwrap().get(&me.public().user_id()).is_none());
    }

    #[test]
    fn handle_datagram_reports_refreshed_on_repeat_sight() {
        // First sight → New; the same peer re-announcing → Refreshed (this is what
        // gates the listener's reply: a reply fires only on New, not on Refreshed).
        let alice = DeviceIdentity::generate();
        let roster = Mutex::new(Roster::default());
        let bytes = encode(&Announce::new(&alice, "Alice", 4000));
        assert_eq!(
            handle_datagram(&roster, &bytes, source(), "self"),
            UpdateOutcome::New
        );
        assert_eq!(
            handle_datagram(&roster, &bytes, source(), "self"),
            UpdateOutcome::Refreshed
        );
    }

    #[test]
    fn scan_targets_covers_full_subnet_excluding_own() {
        // A /24 sweep targets x.y.z.1..=254 minus our own address: 253 targets,
        // .1 and .254 included (range endpoints), .0 and .255 excluded, and the
        // own address never appears.
        let own = Ipv4Addr::new(192, 168, 1, 50);
        let targets = scan_targets(own);
        assert_eq!(targets.len(), 253); // 254 hosts minus our own
        assert!(!targets.contains(&own), "must not scan our own address");
        assert!(targets.contains(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(targets.contains(&Ipv4Addr::new(192, 168, 1, 254)));
        assert!(!targets.contains(&Ipv4Addr::new(192, 168, 1, 0)));
        assert!(!targets.contains(&Ipv4Addr::new(192, 168, 1, 255)));
    }

    #[test]
    fn scan_targets_excludes_own_at_range_edge() {
        // Own address at a range endpoint (.1) is still excluded; the rest of the
        // /24 (252 + .254 = ...) is intact at 253 targets.
        let own = Ipv4Addr::new(10, 0, 0, 1);
        let targets = scan_targets(own);
        assert_eq!(targets.len(), 253);
        assert!(!targets.contains(&own));
        assert!(targets.contains(&Ipv4Addr::new(10, 0, 0, 2)));
        assert!(targets.contains(&Ipv4Addr::new(10, 0, 0, 254)));
    }

    #[test]
    fn lan_scan_never_sweeps_tailscale_address_space() {
        let tailnet: Ipv4Addr = "100.101.1.20".parse().unwrap();
        assert!(scan_targets_for_interfaces(&[tailnet]).is_empty());
    }

    #[test]
    fn handle_datagram_rejects_garbage() {
        let roster = Mutex::new(Roster::default());
        assert_eq!(
            handle_datagram(&roster, b"not a datagram", source(), "self"),
            UpdateOutcome::Rejected
        );
    }

    #[tokio::test]
    async fn listen_loop_records_a_received_announce() {
        let alice = DeviceIdentity::generate();
        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listen_addr = listener.local_addr().unwrap();
        let roster = Arc::new(Mutex::new(Roster::default()));
        let self_id = DeviceIdentity::generate();
        let self_announce = shared_announce(&Announce::new(&self_id, "Self", 1));
        tokio::spawn(run_listen(
            listener.clone(),
            roster.clone(),
            self_id.public().user_id(),
            self_announce,
            47474,
        ));

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let announce = Announce::new(&alice, "Alice", 4000);
        sender
            .send_to(&encode(&announce), listen_addr)
            .await
            .unwrap();

        let mut recorded = false;
        for _ in 0..100 {
            if roster
                .lock()
                .unwrap()
                .get(&alice.public().user_id())
                .is_some()
            {
                recorded = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(recorded, "listener did not record the announce");
        let r = roster.lock().unwrap();
        assert_eq!(r.get(&alice.public().user_id()).unwrap().addr.port(), 4000);
    }

    #[tokio::test]
    async fn broadcast_loop_sends_decodable_announces() {
        let alice = DeviceIdentity::generate();
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let recv_addr = receiver.local_addr().unwrap();
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let announce = Announce::new(&alice, "Alice", 4000);

        tokio::spawn(run_broadcast(
            sender,
            shared_announce(&announce),
            recv_addr,
            Duration::from_millis(10),
            None,
        ));

        let mut buf = vec![0u8; 2048];
        let (n, _src) = tokio::time::timeout(Duration::from_secs(2), receiver.recv_from(&mut buf))
            .await
            .expect("received an announce within 2s")
            .unwrap();
        let decoded = decode(&buf[..n]).expect("decodes");
        assert_eq!(decoded, announce);
        assert!(decoded.verify());
    }

    #[tokio::test]
    async fn broadcast_picks_up_a_swapped_announce_live() {
        // The broadcast loop reads the shared announce on each send, so swapping in a
        // freshly-built announce (the runtime-rename path) changes what peers receive
        // without restarting discovery.
        let alice = DeviceIdentity::generate();
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let recv_addr = receiver.local_addr().unwrap();
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let shared = shared_announce(&Announce::new(&alice, "Alice", 4000));

        tokio::spawn(run_broadcast(
            sender,
            Arc::clone(&shared),
            recv_addr,
            Duration::from_millis(10),
            None,
        ));

        // Swap in a new name; subsequent sends must carry it (and still verify).
        swap_announce(&shared, &Announce::new(&alice, "Alice Renamed", 4000));

        let mut buf = vec![0u8; 2048];
        let mut saw_renamed = false;
        for _ in 0..50 {
            let (n, _src) =
                tokio::time::timeout(Duration::from_secs(2), receiver.recv_from(&mut buf))
                    .await
                    .expect("an announce within 2s")
                    .unwrap();
            let decoded = decode(&buf[..n]).expect("decodes");
            assert!(decoded.verify());
            if decoded.name == "Alice Renamed" {
                saw_renamed = true;
                break;
            }
        }
        assert!(saw_renamed, "swapped announce never reached the receiver");
    }

    #[tokio::test]
    async fn broadcast_burst_fires_several_announces_before_the_steady_loop() {
        // The startup burst sends multiple announces within the first ~2s, all
        // decodable and BEFORE the long steady interval would have ticked. Use a
        // very long steady interval so any announce we receive must be a burst one.
        let alice = DeviceIdentity::generate();
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let recv_addr = receiver.local_addr().unwrap();
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let announce = Announce::new(&alice, "Alice", 4000);

        tokio::spawn(run_broadcast(
            sender,
            shared_announce(&announce),
            recv_addr,
            Duration::from_secs(3600), // steady loop won't tick during the test
            None,
        ));

        // Expect at least the first two burst announces (100ms, 500ms) inside 2s.
        let mut buf = vec![0u8; 2048];
        for _ in 0..2 {
            let (n, _src) =
                tokio::time::timeout(Duration::from_secs(2), receiver.recv_from(&mut buf))
                    .await
                    .expect("a burst announce within 2s")
                    .unwrap();
            assert_eq!(decode(&buf[..n]).expect("decodes"), announce);
        }
    }

    #[tokio::test]
    async fn broadcast_finishes_startup_burst_within_its_documented_offsets() {
        let id = DeviceIdentity::generate();
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let target = receiver.local_addr().unwrap();
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(run_broadcast(
            sender,
            shared_announce(&Announce::new(&id, "Alice", 4000)),
            target,
            Duration::from_secs(3600),
            None,
        ));
        let mut buf = [0; 2048];
        for _ in 0..BROADCAST_STARTUP_BURST_MS.len() {
            tokio::time::timeout_at(
                started + Duration::from_millis(2350),
                receiver.recv_from(&mut buf),
            )
            .await
            .expect("all startup announces arrive close to 100, 500, and 2000ms")
            .unwrap();
        }
        task.abort();
    }

    #[tokio::test]
    async fn manual_broadcast_trigger_works_during_startup_burst() {
        let id = DeviceIdentity::generate();
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let target = receiver.local_addr().unwrap();
        let trigger = Arc::new(Notify::new());
        let task = tokio::spawn(run_broadcast(
            sender,
            shared_announce(&Announce::new(&id, "Alice", 4000)),
            target,
            Duration::from_secs(3600),
            Some(Arc::clone(&trigger)),
        ));
        let mut buf = [0; 2048];
        receiver.recv_from(&mut buf).await.unwrap(); // first scheduled send at 100ms
        trigger.notify_waiters();
        tokio::time::timeout(Duration::from_millis(250), receiver.recv_from(&mut buf))
            .await
            .expect("manual trigger should not wait for the next startup send")
            .unwrap();
        task.abort();
    }

    #[test]
    fn scan_targets_for_interfaces_deduplicates_subnets_and_excludes_local_addresses() {
        let own = [
            Ipv4Addr::new(192, 168, 1, 10),
            Ipv4Addr::new(192, 168, 1, 11),
            Ipv4Addr::new(10, 0, 0, 5),
        ];
        let targets = scan_targets_for_interfaces(&own);
        assert_eq!(targets.len(), 505);
        assert!(!targets.contains(&own[0]));
        assert!(!targets.contains(&own[1]));
        assert!(!targets.contains(&own[2]));
        assert_eq!(
            targets
                .iter()
                .filter(|ip| **ip == Ipv4Addr::new(192, 168, 1, 9))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn broadcast_trigger_fires_an_immediate_announce() {
        // With a long steady interval (and after the burst), a notify on the trigger
        // produces an extra announce promptly — the manual "announce now" path.
        let alice = DeviceIdentity::generate();
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let recv_addr = receiver.local_addr().unwrap();
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let announce = Announce::new(&alice, "Alice", 4000);
        let trigger = Arc::new(Notify::new());

        tokio::spawn(run_broadcast(
            sender,
            shared_announce(&announce),
            recv_addr,
            Duration::from_secs(3600),
            Some(Arc::clone(&trigger)),
        ));

        // Drain the startup burst (3 announces) so the next one is trigger-driven.
        let mut buf = vec![0u8; 2048];
        for _ in 0..BROADCAST_STARTUP_BURST_MS.len() {
            let _ = tokio::time::timeout(Duration::from_secs(3), receiver.recv_from(&mut buf))
                .await
                .expect("burst announce")
                .unwrap();
        }
        // No further announce without a trigger (steady interval is an hour).
        assert!(
            tokio::time::timeout(Duration::from_millis(300), receiver.recv_from(&mut buf))
                .await
                .is_err(),
            "no announce should arrive between the burst and a trigger"
        );
        // Fire the trigger → an immediate announce arrives.
        trigger.notify_waiters();
        let (n, _src) = tokio::time::timeout(Duration::from_secs(2), receiver.recv_from(&mut buf))
            .await
            .expect("triggered announce within 2s")
            .unwrap();
        assert_eq!(decode(&buf[..n]).expect("decodes"), announce);
    }

    #[tokio::test]
    async fn listen_replies_on_first_sight_but_not_on_refresh() {
        // The peer binds a known port and sends from it, so the listener's reply
        // (aimed at source-ip:discovery_port) lands back on the peer's socket.
        let peer_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let peer_port = peer_sock.local_addr().unwrap().port();
        let peer = DeviceIdentity::generate();
        let peer_announce = Announce::new(&peer, "Peer", 4000);

        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listen_addr = listener.local_addr().unwrap();
        let roster = Arc::new(Mutex::new(Roster::default()));
        let self_id = DeviceIdentity::generate();
        let self_announce = shared_announce(&Announce::new(&self_id, "Self", 1));
        tokio::spawn(run_listen(
            listener,
            roster,
            self_id.public().user_id(),
            self_announce,
            peer_port, // reply to the peer's bound port (its "discovery port")
        ));

        // First sight → expect a unicast reply (our self-announce) back.
        peer_sock
            .send_to(&encode(&peer_announce), listen_addr)
            .await
            .unwrap();
        let mut buf = vec![0u8; 2048];
        let (n, _src) = tokio::time::timeout(Duration::from_secs(2), peer_sock.recv_from(&mut buf))
            .await
            .expect("reply on first sight")
            .unwrap();
        let reply = decode(&buf[..n]).expect("reply decodes");
        assert_eq!(reply.user_id, self_id.public().user_id());

        // Re-announce (refresh) → NO reply (avoids the 2s reply storm).
        peer_sock
            .send_to(&encode(&peer_announce), listen_addr)
            .await
            .unwrap();
        let second =
            tokio::time::timeout(Duration::from_millis(300), peer_sock.recv_from(&mut buf)).await;
        assert!(second.is_err(), "refresh must not trigger a reply");
    }

    #[tokio::test]
    async fn scan_does_not_send_to_own_address() {
        // scan_once must skip our own interface address. Bind a socket on a local
        // interface address and confirm a sweep over its /24 never targets it.
        let own_addrs = ipv4_interface_addrs();
        let Some(own) = own_addrs.into_iter().next() else {
            return; // no non-loopback iface in this env — nothing to assert
        };
        // Listen on our own address at a chosen port; a scan over the /24 must not
        // deliver anything to it (the own-address skip).
        let port = {
            let probe = UdpSocket::bind(SocketAddr::new(IpAddr::V4(own), 0))
                .await
                .unwrap();
            probe.local_addr().unwrap().port()
        };
        let listener = UdpSocket::bind(SocketAddr::new(IpAddr::V4(own), port))
            .await
            .unwrap();
        let scanner = Arc::new(UdpSocket::bind("0.0.0.0:0").await.unwrap());
        let id = DeviceIdentity::generate();
        let bytes = encode(&Announce::new(&id, "Scanner", 4000));
        scan_once(&scanner, &bytes, port).await;

        let mut buf = vec![0u8; 2048];
        let got =
            tokio::time::timeout(Duration::from_millis(300), listener.recv_from(&mut buf)).await;
        assert!(got.is_err(), "scan delivered a datagram to our own address");
    }
}
