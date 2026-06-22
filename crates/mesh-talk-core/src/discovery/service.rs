//! The discovery service: a broadcast loop (advertise our announce) and a listen
//! loop (record peers). The datagram handler is pure so it can be unit-tested;
//! the loops are thin wrappers over a `tokio` UDP socket.

use crate::discovery::announce::{decode, encode, Announce};
use crate::discovery::roster::{Roster, UpdateOutcome};
use crate::transport::net::{ipv4_interface_addrs, join_discovery_group_all_ifaces};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Decode + verify + record one received datagram. Returns the [`UpdateOutcome`]
/// (`New` on first sight of a peer — used by the listener to fire a unicast reply
/// exactly once). Pure (apart from locking the roster) — no I/O.
pub fn handle_datagram(
    roster: &Mutex<Roster>,
    data: &[u8],
    source: SocketAddr,
    self_user_id: &str,
) -> UpdateOutcome {
    let Some(announce) = decode(data) else {
        return UpdateOutcome::Rejected;
    };
    roster
        .lock()
        .expect("roster mutex not poisoned")
        .update(&announce, source.ip(), self_user_id)
}

/// Peers not heard from within this window are evicted from the roster. Broadcast
/// interval is ~2s, so this tolerates ~15 missed announces before dropping a peer —
/// long enough to ride out brief loss, short enough that stale addresses (a peer that
/// went offline or changed IP) don't linger and misdirect fan-out / post-office picks.
const PEER_TTL: Duration = Duration::from_secs(30);

/// Listen for announces on `socket`, feeding the roster, until the socket errors
/// (e.g. the task is aborted on shutdown). Also evicts stale peers each iteration —
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
    self_announce_bytes: Arc<Vec<u8>>,
    discovery_port: u16,
) {
    let mut buf = vec![0u8; 2048];
    loop {
        match tokio::time::timeout(PEER_TTL, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => {
                if handle_datagram(&roster, &buf[..n], source, &self_user_id) == UpdateOutcome::New
                {
                    // First sight → unicast our announce back to the peer's
                    // discovery port so the link is recorded symmetrically.
                    let reply_target = SocketAddr::new(source.ip(), discovery_port);
                    let _ = socket.send_to(&self_announce_bytes, reply_target).await;
                }
            }
            Ok(Err(_)) => break, // socket error — end the loop (shutdown)
            Err(_) => {}         // recv timed out → fall through to evict
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
    announce: Announce,
    target: SocketAddr,
    interval: Duration,
    trigger: Option<Arc<Notify>>,
) {
    let bytes = encode(&announce);
    // Startup burst: a few staggered announces to beat early UDP loss.
    for offset_ms in BROADCAST_STARTUP_BURST_MS {
        tokio::time::sleep(Duration::from_millis(offset_ms)).await;
        let _ = socket.send_to(&bytes, target).await;
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
        // Ignore send errors: the network may be transiently down. Retry next tick.
        let _ = socket.send_to(&bytes, target).await;
    }
}

/// The discovery port unicast-scan cadence: a few quick bursts at startup so a
/// peer that came up just before us is found fast, then a steady slow interval
/// (multicast/broadcast carry the steady-state load; the scan is the fallback for
/// when those are blocked but unicast is allowed).
const SCAN_STARTUP_DELAYS_SECS: [u64; 3] = [1, 3, 7];
const SCAN_STEADY_INTERVAL: Duration = Duration::from_secs(20);

/// Unicast-UDP /24 scan fallback (the analog of LocalSend's HTTP /24 scan, adapted
/// to our fixed UDP discovery port). For each local non-loopback IPv4 interface,
/// derive its /24 and send our `announce` to `x.y.z.1..=254` at `discovery_port`,
/// skipping our own address. The receiver's normal listen loop records us directly
/// (and, on first sight, replies), so two scanning sides converge symmetrically
/// even when multicast/broadcast is dropped. Send errors are ignored per-target.
pub async fn run_scan(
    socket: Arc<UdpSocket>,
    announce: Announce,
    discovery_port: u16,
    trigger: Option<Arc<Notify>>,
) {
    let bytes = encode(&announce);
    for delay in SCAN_STARTUP_DELAYS_SECS {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        scan_once(&socket, &bytes, discovery_port).await;
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
        scan_once(&socket, &bytes, discovery_port).await;
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

/// One sweep: enumerate interfaces fresh (they may change) and unicast `bytes` to
/// every host in each /24 except our own address. Fire-and-forget; errors ignored.
async fn scan_once(socket: &UdpSocket, bytes: &[u8], discovery_port: u16) {
    for own in ipv4_interface_addrs() {
        for target in scan_targets(own) {
            let _ = socket
                .send_to(bytes, SocketAddr::new(IpAddr::V4(target), discovery_port))
                .await;
        }
    }
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
    spawn_discovery_with_trigger(socket, roster, announce, self_user_id, discovery_port, None)
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
    announce: Announce,
    self_user_id: String,
    discovery_port: u16,
    trigger: Option<Arc<Notify>>,
) -> Vec<JoinHandle<()>> {
    let self_announce_bytes = Arc::new(encode(&announce));
    let target: SocketAddr = (
        crate::transport::net::DISCOVERY_MULTICAST_GROUP,
        discovery_port,
    )
        .into();
    vec![
        tokio::spawn(run_listen(
            Arc::clone(&socket),
            roster,
            self_user_id,
            self_announce_bytes,
            discovery_port,
        )),
        tokio::spawn(run_broadcast(
            Arc::clone(&socket),
            announce.clone(),
            target,
            Duration::from_secs(2),
            trigger.clone(),
        )),
        tokio::spawn(run_scan(
            Arc::clone(&socket),
            announce,
            discovery_port,
            trigger,
        )),
        tokio::spawn(run_rejoin(socket)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    use std::net::{IpAddr, Ipv4Addr};

    fn source() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345)
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
        assert!(roster
            .lock()
            .unwrap()
            .get(&alice.public().user_id())
            .is_some());
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
        let self_announce = Arc::new(encode(&Announce::new(&self_id, "Self", 1)));
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
            announce.clone(),
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
            announce.clone(),
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
            announce.clone(),
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
        let self_announce = Arc::new(encode(&Announce::new(&self_id, "Self", 1)));
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
