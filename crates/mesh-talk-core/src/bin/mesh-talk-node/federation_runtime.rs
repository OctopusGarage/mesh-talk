//! Bounded relay reconciliation driven by explicit public trust pins.
use super::federation_config::FederationSeed;
use futures_util::{StreamExt, stream::FuturesUnordered};
use mesh_talk_core::discovery::Roster;
use mesh_talk_core::identity::device::{DeviceIdentity, PublicIdentity};
use mesh_talk_core::postoffice::federation::{
    FederationDirectory, IndexedRelayEndpoint, IndexedRelayError, MAX_MAILBOXES_PER_EXCHANGE,
};
use mesh_talk_core::postoffice::{IndexedPostOffice, network::RelayNetwork};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Independent operation budgets, not a process-wide socket cap.
const MAX_SEED_PROBES: usize = 8;
const MAX_RELAY_SYNCS: usize = 8;
const MAX_KNOWN_EDGES: usize = 2048;
const MAX_CONTINUATION_DELAY: Duration = Duration::from_secs(5);
type EdgeKey = (SocketAddr, [u8; 32], [u8; 32]);

struct Edge {
    peer: PublicIdentity,
    pending: bool,
    active: bool,
    failures: u32,
    continuation_batches: u32,
    retry_at: Instant,
    observed: Instant,
}

impl Edge {
    fn succeeded(&mut self, count: usize, now: Instant) {
        self.failures = 0;
        if count == MAX_MAILBOXES_PER_EXCHANGE {
            // A full batch is not proof that inventories are converging: a peer
            // may reject data or retain less than its neighbor. Pace successive
            // batches independently per edge instead of reconnecting forever.
            self.continuation_batches = self.continuation_batches.saturating_add(1);
            let delay =
                Duration::from_millis(100u64 << self.continuation_batches.saturating_sub(1).min(6))
                    .min(MAX_CONTINUATION_DELAY);
            self.retry_at = now + delay;
            self.pending = true;
        } else {
            self.continuation_batches = 0;
            self.retry_at = now;
        }
    }
}

struct Probe {
    seed: FederationSeed,
    pending: bool,
    active: bool,
    failures: u32,
    retry_at: Instant,
}

fn jittered_delay(ceiling_secs: u64) -> Duration {
    let max_ms = ceiling_secs * 1000;
    let floor_ms = max_ms * 4 / 5;
    Duration::from_millis(
        floor_ms + uuid::Uuid::new_v4().as_u128() as u64 % (max_ms - floor_ms + 1),
    )
}

fn retry_delay(failures: u32) -> Duration {
    jittered_delay((1u64 << failures.min(6)).min(60))
}

fn observe_edge(edges: &mut HashMap<EdgeKey, Edge>, addr: SocketAddr, peer: PublicIdentity) {
    let key = (addr, peer.ed25519_pub, peer.x25519_pub);
    if let Some(edge) = edges.get_mut(&key) {
        edge.observed = Instant::now();
        return;
    }
    if edges.len() >= MAX_KNOWN_EDGES {
        if let Some(oldest) = edges
            .iter()
            .filter(|(_, edge)| !edge.active)
            .min_by_key(|(_, edge)| edge.observed)
            .map(|(key, _)| *key)
        {
            edges.remove(&oldest);
        } else {
            return;
        }
    }
    edges.insert(
        key,
        Edge {
            peer,
            pending: true,
            active: false,
            failures: 0,
            continuation_batches: 0,
            retry_at: Instant::now(),
            observed: Instant::now(),
        },
    );
}

fn observe_roster(
    roster: &Mutex<Roster>,
    trusted: &HashSet<String>,
    edges: &mut HashMap<EdgeKey, Edge>,
    network_enabled: bool,
) {
    let known = roster
        .lock()
        .expect("roster lock not poisoned")
        .post_offices();
    for peer in known {
        if network_enabled || trusted.contains(&peer.public.user_id()) {
            observe_edge(edges, peer.addr, peer.public);
        }
    }
}

/// Each edge has one coalesced pending bit and at most one active exchange.
/// A slow edge never delays a new generation on another completed edge.
pub(super) async fn run_federation_loop(
    roster: Arc<Mutex<Roster>>,
    seeds: Vec<FederationSeed>,
    identity: DeviceIdentity,
    store: IndexedPostOffice,
    directory: FederationDirectory,
    network: Option<RelayNetwork>,
    endpoint: IndexedRelayEndpoint,
) -> Result<(), IndexedRelayError> {
    let mut mail = store.subscribe_changes();
    let mut presence = directory.subscribe_changes();
    let mut routes = roster
        .lock()
        .expect("roster lock not poisoned")
        .subscribe_changes();
    let trusted = seeds
        .iter()
        .filter(|seed| seed.id != identity.user_id())
        .map(|seed| seed.id.clone())
        .collect::<HashSet<_>>();
    let mut probes = seeds
        .into_iter()
        .filter(|seed| trusted.contains(&seed.id))
        .map(|seed| {
            (
                (seed.id.clone(), seed.discovery),
                Probe {
                    seed,
                    pending: true,
                    active: false,
                    failures: 0,
                    retry_at: Instant::now(),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut edges = HashMap::new();
    observe_roster(&roster, &trusted, &mut edges, network.is_some());
    let mut exchanges = FuturesUnordered::new();
    let mut queries = FuturesUnordered::new();
    let mut edge_cursor = None;
    let mut probe_cursor = None;
    let mut dirty_at = None;
    let mut idle_rounds = 0u32;
    let mut fallback_at = tokio::time::Instant::now() + jittered_delay(5);
    let identity = Arc::new(identity);
    loop {
        // Snapshot fair due work before spawning. Pending/active state is owned
        // exclusively here; notifications cannot create duplicate workers.
        let now = Instant::now();
        let mut due = edges
            .iter()
            .filter(|(_, edge)| edge.pending && !edge.active && edge.retry_at <= now)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        due.sort_unstable();
        if let Some(cursor) = edge_cursor {
            let offset = due.partition_point(|key| *key <= cursor);
            due.rotate_left(offset);
        }
        for key in due.into_iter().take(MAX_RELAY_SYNCS - exchanges.len()) {
            edge_cursor = Some(key);
            let edge = edges.get_mut(&key).unwrap();
            edge.pending = false;
            edge.active = true;
            let peer = edge.peer.clone();
            let identity = identity.clone();
            let endpoint = endpoint.clone();
            exchanges.push(async move {
                (
                    key,
                    endpoint
                        .sync_with_relay_checked(key.0, &identity, &peer)
                        .await,
                )
            });
        }
        let mut due = probes
            .iter()
            .filter(|(_, probe)| probe.pending && !probe.active && probe.retry_at <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        due.sort_unstable();
        if let Some(cursor) = &probe_cursor {
            let offset = due.partition_point(|key| key <= cursor);
            due.rotate_left(offset);
        }
        for key in due.into_iter().take(MAX_SEED_PROBES - queries.len()) {
            probe_cursor = Some(key.clone());
            let probe = probes.get_mut(&key).unwrap();
            probe.pending = false;
            probe.active = true;
            let seed = probe.seed.clone();
            queries.push(async move {
                let proof = mesh_talk_core::postoffice::gateway::query_pinned_relay(
                    seed.discovery,
                    &seed.id,
                )
                .await;
                (
                    key,
                    proof.map(|proof| {
                        (
                            SocketAddr::new(seed.discovery.ip(), proof.tcp_port),
                            proof.public(),
                        )
                    }),
                )
            });
        }
        let next_retry = edges
            .values()
            .filter(|edge| edge.pending && !edge.active && edge.retry_at > now)
            .map(|edge| edge.retry_at)
            .chain(
                probes
                    .values()
                    .filter(|probe| probe.pending && !probe.active && probe.retry_at > now)
                    .map(|probe| probe.retry_at),
            )
            .min();
        let retry_at = next_retry.map(|next| {
            tokio::time::Instant::now() + next.saturating_duration_since(Instant::now())
        });
        tokio::select! {
            result = exchanges.next(), if !exchanges.is_empty() => {
                let Some((key, result)) = result else { return Ok(()); };
                let edge = edges.get_mut(&key).expect("active edge remains installed");
                edge.active = false;
                match result {
                    Ok(count) => {
                        edge.succeeded(count, Instant::now());
                    }
                    Err(error) => {
                        if store.requires_reopen() { return Err(error); }
                        log::debug!("federation sync at {}: {error}", key.0);
                        edge.continuation_batches = 0;
                        edge.failures = edge.failures.saturating_add(1);
                        let local = error.is_local_retry();
                        edge.retry_at = Instant::now() + if local { Duration::from_millis(100u64 << edge.failures.saturating_sub(1).min(6)).min(Duration::from_secs(5)) } else { retry_delay(edge.failures) };
                        edge.pending = true;
                    }
                }
            }
            result = queries.next(), if !queries.is_empty() => {
                let Some((key, result)) = result else { return Ok(()); };
                let probe = probes.get_mut(&key).unwrap();
                probe.active = false;
                match result {
                    Some((addr, peer)) => {
                        probe.failures = 0;
                        probe.retry_at = Instant::now();
                        observe_edge(&mut edges, addr, peer);
                    }
                    None => {
                        probe.failures = probe.failures.saturating_add(1);
                        probe.retry_at = Instant::now() + retry_delay(probe.failures);
                        probe.pending = true;
                    }
                }
            }
            _ = mail.changed() => {
                dirty_at.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_millis(100));
            }
            _ = presence.changed() => {
                dirty_at.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_millis(100));
            }
            _ = routes.changed() => { observe_roster(&roster, &trusted, &mut edges, network.is_some()); }
            _ = tokio::time::sleep_until(dirty_at.unwrap_or_else(tokio::time::Instant::now)), if dirty_at.is_some() => {
                dirty_at = None;
                idle_rounds = 0;
                // A new presence registration must shorten an old idle timer,
                // otherwise its first propagated lease could expire before renewal.
                fallback_at = fallback_at.min(tokio::time::Instant::now() + jittered_delay(5));
                for edge in edges.values_mut() { edge.pending = true; }
            }
            _ = tokio::time::sleep_until(retry_at.unwrap_or_else(tokio::time::Instant::now)), if retry_at.is_some() => {}
            _ = tokio::time::sleep_until(fallback_at) => {
                for edge in edges.values_mut() { edge.pending = true; }
                for probe in probes.values_mut() { probe.pending = true; }
                let ceiling = if directory.has_live_presence() { 5 } else { 5u64.saturating_mul(1u64 << idle_rounds.min(4)).min(60) };
                fallback_at = tokio::time::Instant::now() + jittered_delay(ceiling);
                idle_rounds = idle_rounds.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::{
        eventlog::sync::network::NetworkSyncStore,
        postoffice::{PostOffice, federation::IndexedRelayTrust},
        storage::profile::ProfileStorageLease,
    };
    async fn run_federation_loop(
        roster: Arc<Mutex<Roster>>,
        seeds: Vec<FederationSeed>,
        identity: DeviceIdentity,
        store: IndexedPostOffice,
        directory: FederationDirectory,
        network: Option<RelayNetwork>,
    ) {
        let trust = network.clone().map(IndexedRelayTrust::Network).or_else(|| {
            Some(IndexedRelayTrust::Pinned(Arc::new(
                seeds.iter().map(|s| s.id.clone()).collect(),
            )))
        });
        let endpoint = IndexedRelayEndpoint::new(store.clone(), Some(directory.clone()), trust);
        let (_stop, rx) = tokio::sync::watch::channel(false);
        tokio::select! {
            result=super::run_federation_loop(roster,seeds,identity,store.clone(),directory,network,endpoint)=>{result.unwrap();}
            result=store.run_maintenance(rx)=>{result.unwrap();}
        }
    }
    use mesh_talk_core::discovery::Announce;
    use tokio::net::{TcpListener, UdpSocket};

    fn local_store(temp: &tempfile::TempDir) -> IndexedPostOffice {
        IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(temp.path()).unwrap(),
            &temp.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap()
    }

    #[test]
    fn full_batch_continuation_backs_off_without_delaying_a_drained_edge() {
        let now = Instant::now();
        let mut edges = HashMap::new();
        let saturated = DeviceIdentity::generate().public();
        let healthy = DeviceIdentity::generate().public();
        let saturated_addr = "127.0.0.1:47479".parse().unwrap();
        let healthy_addr = "127.0.0.1:47480".parse().unwrap();
        observe_edge(&mut edges, saturated_addr, saturated.clone());
        observe_edge(&mut edges, healthy_addr, healthy.clone());
        let saturated = edges
            .get_mut(&(saturated_addr, saturated.ed25519_pub, saturated.x25519_pub))
            .unwrap();
        let mut completed_at = now;
        for delay_ms in [100, 200, 400, 800, 1600, 3200, 5000, 5000] {
            saturated.pending = false; // The exchange consumes the pending turn.
            saturated.succeeded(MAX_MAILBOXES_PER_EXCHANGE, completed_at);
            assert!(saturated.pending);
            assert_eq!(
                saturated.retry_at.duration_since(completed_at),
                Duration::from_millis(delay_ms),
                "a full batch alone must not authorize an immediate endless continuation"
            );
            // Mail accepted by this same exchange publishes a dirty wake. Merely
            // making it pending again must not erase its continuation deadline.
            saturated.pending = true;
            completed_at = saturated.retry_at;
        }
        let saturated_retry = saturated.retry_at;
        let healthy = edges
            .get_mut(&(healthy_addr, healthy.ed25519_pub, healthy.x25519_pub))
            .unwrap();
        healthy.pending = false;
        healthy.succeeded(0, now);
        healthy.pending = true; // New mail after an idle complete exchange.
        assert!(healthy.retry_at <= now);
        assert!(saturated_retry > now);
    }

    #[test]
    fn draining_a_full_batch_backlog_resets_its_continuation_delay() {
        let peer = DeviceIdentity::generate().public();
        let addr = "127.0.0.1:47479".parse().unwrap();
        let mut edges = HashMap::new();
        observe_edge(&mut edges, addr, peer.clone());
        let edge = edges
            .get_mut(&(addr, peer.ed25519_pub, peer.x25519_pub))
            .unwrap();
        let mut now = Instant::now();
        for _ in 0..8 {
            edge.pending = false;
            edge.succeeded(MAX_MAILBOXES_PER_EXCHANGE, now);
            now = edge.retry_at;
        }
        edge.pending = false;
        edge.succeeded(MAX_MAILBOXES_PER_EXCHANGE - 1, now);
        assert!(!edge.pending);
        assert_eq!(edge.retry_at, now);
        edge.pending = false;
        edge.succeeded(MAX_MAILBOXES_PER_EXCHANGE, now);
        assert_eq!(
            edge.retry_at.duration_since(now),
            Duration::from_millis(100)
        );
    }

    #[tokio::test]
    async fn healthy_edge_reacts_to_new_mail_while_an_authenticated_edge_is_silent() {
        use mesh_talk_core::eventlog::{ConversationId, Event, EventKind};
        use mesh_talk_core::postoffice::opaque::RelayMailbox;
        let temp = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let healthy = DeviceIdentity::generate();
        let silent = DeviceIdentity::generate();
        let healthy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let healthy_addr = healthy_listener.local_addr().unwrap();
        let silent_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent_addr = silent_listener.local_addr().unwrap();
        let mut roster = Roster::default();
        let query_sink = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let seeds = [(&healthy, healthy_addr), (&silent, silent_addr)]
            .into_iter()
            .map(|(peer, addr)| {
                roster.update(
                    &Announce::new_post_office(peer, "relay", addr.port()),
                    addr.ip(),
                    &identity.user_id(),
                );
                FederationSeed {
                    id: peer.user_id(),
                    discovery: query_sink.local_addr().unwrap(),
                }
            })
            .collect();
        let store = local_store(&temp);
        let target = Arc::new(Mutex::new(
            PostOffice::open(
                &temp.path().join("target"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap(),
        ));
        let sender = DeviceIdentity::generate();
        let mailbox =
            RelayMailbox::federated(&sender, &DeviceIdentity::generate().public()).unwrap();
        let warmup = mailbox
            .wrap(&Event::new(
                &sender,
                ConversationId::new([7; 32]),
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                b"initial".to_vec(),
            ))
            .unwrap();
        store.clone().ingest(warmup.clone()).unwrap();
        let server = tokio::spawn(
            mesh_talk_core::node::run_federated_relay_accept_loop_with_directory(
                healthy,
                healthy_listener,
                target.clone(),
                Arc::new(HashSet::from([identity.user_id()])),
                FederationDirectory::new(),
            ),
        );
        let (accepted, ready) = tokio::sync::oneshot::channel();
        let stalled = tokio::spawn(async move {
            let (stream, _) = silent_listener.accept().await.unwrap();
            let mut channel = mesh_talk_core::transport::SecureChannel::accept(stream, &silent)
                .await
                .unwrap();
            let _request = channel.recv().await.unwrap();
            accepted.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(roster)),
            seeds,
            identity,
            store.clone(),
            FederationDirectory::new(),
            None,
        ));
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !target.lock().unwrap().has(&warmup.id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let next = mailbox
            .wrap(&Event::new(
                &sender,
                ConversationId::new([7; 32]),
                2,
                vec![],
                2,
                0,
                EventKind::Message,
                b"new generation".to_vec(),
            ))
            .unwrap();
        let id = next.id;
        store.clone().ingest(next).unwrap();
        let delivered = tokio::time::timeout(Duration::from_secs(2), async {
            while !target.lock().unwrap().has(&id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let silent_still_active = !stalled.is_finished();
        runner.abort();
        server.abort();
        stalled.abort();
        assert!(
            delivered.is_ok(),
            "a healthy edge must process new mail before the silent edge's thirty-second deadline"
        );
        assert!(silent_still_active);
    }

    #[tokio::test]
    async fn durable_ingest_pushes_federation_before_its_idle_fallback() {
        use mesh_talk_core::eventlog::{ConversationId, Event, EventKind};
        use mesh_talk_core::postoffice::opaque::RelayMailbox;
        let temp = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let remote = DeviceIdentity::generate();
        let remote_id = remote.user_id();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut roster = Roster::default();
        roster.update(
            &Announce::new_post_office(&remote, "relay", addr.port()),
            addr.ip(),
            &identity.user_id(),
        );
        let store = local_store(&temp);
        let target = Arc::new(Mutex::new(
            PostOffice::open(
                &temp.path().join("target"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap(),
        ));
        let server = tokio::spawn(
            mesh_talk_core::node::run_federated_relay_accept_loop_with_directory(
                remote,
                listener,
                target.clone(),
                Arc::new(HashSet::from([identity.user_id()])),
                FederationDirectory::new(),
            ),
        );
        let unreachable = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // Reply with a rejected proof immediately, so UDP reachability does not
        // hide the push scheduling boundary behind its four-second timeout.
        let discovery = unreachable.local_addr().unwrap();
        let reject = tokio::spawn(async move {
            let mut bytes = [0; 2048];
            loop {
                let (_, source) = unreachable.recv_from(&mut bytes).await.unwrap();
                unreachable.send_to(b"invalid", source).await.unwrap();
            }
        });
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(roster)),
            vec![FederationSeed {
                id: remote_id,
                discovery,
            }],
            identity,
            store.clone(),
            FederationDirectory::new(),
            None,
        ));
        // Give the initial empty round time to enter its idle wait.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let sender = DeviceIdentity::generate();
        let inner = Event::new(
            &sender,
            ConversationId::new([9; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"pushed".to_vec(),
        );
        let events = (0..20)
            .map(|_| {
                RelayMailbox::federated(&sender, &DeviceIdentity::generate().public())
                    .unwrap()
                    .wrap(&inner)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let ids = events.iter().map(|event| event.id).collect::<Vec<_>>();
        {
            let mut office = store.clone();
            for event in events {
                office.ingest(event).unwrap();
            }
        }
        let delivered = tokio::time::timeout(Duration::from_secs(2), async {
            while !ids.iter().all(|id| target.lock().unwrap().has(id)) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        runner.abort();
        server.abort();
        reject.abort();
        assert!(
            delivered.is_ok(),
            "durable ingestion and full-batch continuation must precede the five-second fallback"
        );
    }

    #[tokio::test]
    async fn reconciliation_opens_at_most_eight_noise_handshakes() {
        let temp = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let mut roster = Roster::default();
        let stalled_queries = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (accepted_tx, mut accepted_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut listeners = Vec::new();
        let mut seeds = Vec::new();
        for _ in 0..9 {
            let peer = DeviceIdentity::generate();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            roster.update(
                &Announce::new_post_office(&peer, "relay", addr.port()),
                addr.ip(),
                &identity.user_id(),
            );
            seeds.push(FederationSeed {
                id: peer.user_id(),
                discovery: stalled_queries.local_addr().unwrap(),
            });
            let accepted = accepted_tx.clone();
            listeners.push(tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                accepted.send(stream).unwrap();
            }));
        }
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(roster)),
            seeds,
            identity,
            local_store(&temp),
            FederationDirectory::default(),
            None,
        ));
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(
                tokio::time::timeout(Duration::from_secs(6), accepted_rx.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let ninth = tokio::time::timeout(Duration::from_millis(200), accepted_rx.recv()).await;
        runner.abort();
        let _ = runner.await;
        for listener in listeners {
            listener.abort();
        }
        assert!(
            ninth.is_err(),
            "ninth Noise handshake started while eight remain stalled"
        );
    }

    #[tokio::test]
    async fn known_lan_route_starts_before_stalled_seed_query_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let peer = DeviceIdentity::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stalled_query = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut roster = Roster::default();
        roster.update(
            &Announce::new_post_office(&peer, "relay", addr.port()),
            addr.ip(),
            &identity.user_id(),
        );
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(roster)),
            vec![FederationSeed {
                id: peer.user_id(),
                discovery: stalled_query.local_addr().unwrap(),
            }],
            identity,
            local_store(&temp),
            FederationDirectory::default(),
            None,
        ));
        let connected = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
        runner.abort();
        let _ = runner.await;
        assert!(
            connected.is_ok(),
            "known LAN route waited for the stalled UDP seed"
        );
    }

    #[tokio::test]
    async fn routed_seed_probes_have_at_most_eight_in_flight_queries() {
        let temp = tempfile::tempdir().unwrap();
        let queries = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let seeds = (0..9)
            .map(|_| FederationSeed {
                id: DeviceIdentity::generate().user_id(),
                discovery: queries.local_addr().unwrap(),
            })
            .collect();
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(Roster::default())),
            seeds,
            DeviceIdentity::generate(),
            local_store(&temp),
            FederationDirectory::default(),
            None,
        ));
        let mut bytes = [0u8; 2048];
        for _ in 0..8 {
            tokio::time::timeout(Duration::from_secs(1), queries.recv_from(&mut bytes))
                .await
                .unwrap()
                .unwrap();
        }
        let ninth =
            tokio::time::timeout(Duration::from_millis(200), queries.recv_from(&mut bytes)).await;
        runner.abort();
        let _ = runner.await;
        assert!(
            ninth.is_err(),
            "ninth UDP query started while eight remain pending"
        );
    }
}
