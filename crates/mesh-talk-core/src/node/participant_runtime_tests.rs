use super::*;
use crate::discovery::{DiscoveryVisibility, Roster};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::{net::TcpListener, sync::mpsc};

fn open(dir: &std::path::Path) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        DeviceIdentity::generate(),
        Account::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.join("messages.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}

struct Candidates(Arc<AtomicUsize>);
impl crate::discovery::candidates::CandidateSource for Candidates {
    fn snapshot(&self) -> futures_util::future::BoxFuture<'_, Vec<std::net::SocketAddr>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Vec::new() })
    }
}

#[tokio::test]
async fn prepare_preserves_requested_listener_and_starts_no_producers() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let snapshots = Arc::new(AtomicUsize::new(0));
    let prepared = PreparedParticipant::prepare(
        node,
        listener,
        Arc::new(DiscoveryVisibility::new(true)),
        ParticipantConfig {
            discovery_port: 0,
            candidate_source: Some(Arc::new(Candidates(snapshots.clone()))),
        },
    )
    .unwrap();
    let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
    use tokio::io::AsyncReadExt;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), connection.read(&mut [0; 1]))
            .await
            .is_err()
    );
    assert_eq!(snapshots.load(Ordering::SeqCst), 0);
    let running = prepared.launch("Owner");
    assert_eq!(running.listen_tcp_port(), address.port());
    assert!(running.discovery_available());
    running.tasks[0].abort();
    tokio::time::timeout(Duration::from_secs(1), async {
        while running.discovery_available() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !running.discovery_available(),
        "terminated discovery is unavailable, not an empty snapshot"
    );
    running.stop().await;
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn failed_prepare_releases_listener_without_launching_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let occupied = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let snapshots = Arc::new(AtomicUsize::new(0));
    let result = PreparedParticipant::prepare(
        node,
        listener,
        Arc::new(DiscoveryVisibility::new(true)),
        ParticipantConfig {
            discovery_port: occupied.local_addr().unwrap().port(),
            candidate_source: Some(Arc::new(Candidates(snapshots.clone()))),
        },
    );
    assert!(result.is_err());
    tokio::task::yield_now().await;
    assert_eq!(snapshots.load(Ordering::SeqCst), 0);
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn stop_retires_owned_host_task_and_rejects_private_public_probe() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut running = PreparedParticipant::prepare(
        node,
        listener,
        Arc::new(DiscoveryVisibility::new(false)),
        ParticipantConfig {
            discovery_port: 0,
            candidate_source: None,
        },
    )
    .unwrap()
    .launch("Owner");
    let task = tokio::spawn(std::future::pending());
    let aborted = task.abort_handle();
    running.register_host_task(task);
    let error = running
        .probe_endpoint("127.0.0.1:9".parse().unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    running.stop().await;
    assert!(aborted.is_finished());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_waits_for_owned_blocking_work_after_host_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut running = PreparedParticipant::prepare(
        node,
        listener,
        Arc::new(DiscoveryVisibility::new(false)),
        ParticipantConfig {
            discovery_port: 0,
            candidate_source: None,
        },
    )
    .unwrap()
    .launch("Owner");
    let work = running.blocking_work();
    let after_stop = work.clone();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    running.register_host_task(tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        assert!(work.spawn(&mut tasks, move || {
            entered.send(()).unwrap();
            released.recv().unwrap();
        }));
        let _ = tasks.join_next().await;
    }));
    entered_rx.await.unwrap();
    let mut stop = tokio::spawn(running.stop());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut stop)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    stop.await.unwrap();
    assert!(!after_stop.spawn(&mut tokio::task::JoinSet::new(), || ()));
}

#[tokio::test]
async fn graceful_host_task_joins_children_before_stop_returns() {
    struct ChildDropped(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for ChildDropped {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut running = PreparedParticipant::prepare(
        node,
        listener,
        Arc::new(DiscoveryVisibility::new(false)),
        ParticipantConfig {
            discovery_port: 0,
            candidate_source: None,
        },
    )
    .unwrap()
    .launch("Owner");
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let (child_started, started) = tokio::sync::oneshot::channel();
    let (child_dropped, dropped) = tokio::sync::oneshot::channel();
    let (cleanup_started, cleanup) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        children.spawn(async move {
            let _observer = ChildDropped(Some(child_dropped));
            child_started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let _ = stopped.await;
        children.abort_all();
        while children.join_next().await.is_some() {}
        cleanup_started.send(()).unwrap();
        let _ = released.await;
    });
    running.register_graceful_host_task(shutdown, task);
    started.await.unwrap();
    let mut stopping = tokio::spawn(running.stop());
    tokio::time::timeout(Duration::from_secs(1), cleanup)
        .await
        .unwrap()
        .unwrap();
    dropped
        .await
        .expect("child destructors finish during graceful cleanup");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut stopping)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    stopping.await.unwrap();
}

#[test]
fn quiet_file_producer_uses_sparse_repair_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    node.retry_file_receives();
    let generation = node
        .attachment_consent
        .lock()
        .unwrap()
        .generation()
        .unwrap();
    node.files
        .lock()
        .unwrap()
        .discover_downloads(generation)
        .unwrap();
    for _ in 0..128 {
        if !node.media_maintenance_pending().unwrap() {
            break;
        }
        node.maintain_media().unwrap();
    }
    let (wait, pending) = node.file_pull_schedule(Duration::from_secs(3));
    assert!(
        !pending,
        "empty profile has no transfer or maintenance work"
    );
    assert!(
        wait >= Duration::from_secs(300),
        "quiet repair must not poll every few seconds: {wait:?}"
    );
}

#[tokio::test]
async fn file_producer_ignores_presence_renewal_but_wakes_for_new_route() {
    use crate::discovery::Announce;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    // Startup explicitly requests one file turn. This fixture observes the
    // settled wait after that turn, rather than mistaking startup demand for a heartbeat.
    use futures_util::FutureExt;
    let _ = node.file_pull_notify.notified().now_or_never();
    let peer = DeviceIdentity::generate();
    let announce = Announce::new(&peer, "synthetic peer", 12345);
    let update = |announce: &Announce, ip: &str| {
        node.roster
            .lock()
            .unwrap()
            .update(announce, ip.parse().unwrap(), &node.user_id());
    };
    update(&announce, "127.0.0.1");
    update(&announce, "127.0.0.2");
    let mut network = node.network_changes.subscribe();
    let mut roster = node.subscribe_roster_changes();
    let mut available = node.subscribe_sibling_availability();
    let mut channels = node.subscribe_channel_changes();
    let mut peers = node.file_pull_peers();
    let waiting = node.wait_for_file_pull(
        &mut network,
        &mut roster,
        &mut available,
        &mut channels,
        &mut peers,
        Duration::from_secs(300),
    );
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    for index in 0..4 {
        update(
            &announce,
            if index % 2 == 0 {
                "127.0.0.1"
            } else {
                "127.0.0.2"
            },
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err(),
            "unchanged presence must not restart storage and file authorization work"
        );
    }
    update(&Announce::new(&peer, "synthetic peer", 12346), "127.0.0.1");
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut waiting)
            .await
            .unwrap(),
        "new endpoint resumes recovery immediately"
    );
}

#[test]
fn cold_file_work_retains_its_prompt_continuation() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let (wait, pending) = node.file_pull_schedule(Duration::from_secs(300));
    assert!(pending, "startup metadata and media work has not completed");
    assert!(wait <= Duration::from_secs(3));
}

#[tokio::test(start_paused = true)]
async fn ignored_file_heartbeats_do_not_extend_the_original_deadline() {
    use crate::discovery::Announce;
    use futures_util::FutureExt;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let _ = node.file_pull_notify.notified().now_or_never();
    let peer = DeviceIdentity::generate();
    let announce = Announce::new(&peer, "synthetic peer", 12345);
    let update = || {
        node.roster.lock().unwrap().update(
            &announce,
            "127.0.0.1".parse().unwrap(),
            &node.user_id(),
        );
    };
    update();
    let mut network = node.network_changes.subscribe();
    let mut roster = node.subscribe_roster_changes();
    let mut available = node.subscribe_sibling_availability();
    let mut channels = node.subscribe_channel_changes();
    let mut peers = node.file_pull_peers();
    let started = tokio::time::Instant::now();
    let waiting = node.wait_for_file_pull(
        &mut network,
        &mut roster,
        &mut available,
        &mut channels,
        &mut peers,
        Duration::from_millis(100),
    );
    tokio::pin!(waiting);
    for _ in 0..3 {
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        update();
    }
    assert!(
        !tokio::time::timeout(Duration::from_millis(50), &mut waiting)
            .await
            .unwrap()
    );
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_millis(100)
    );
}

#[test]
fn blocked_file_receive_keeps_sparse_repair_instead_of_three_second_polling() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    node.retry_file_receives();
    let generation = node
        .attachment_consent
        .lock()
        .unwrap()
        .generation()
        .unwrap();
    node.files
        .lock()
        .unwrap()
        .discover_downloads(generation)
        .unwrap();
    for _ in 0..128 {
        if !node.media_maintenance_pending().unwrap() {
            break;
        }
        node.maintain_media().unwrap();
    }
    node.storage.database().transaction(|c| {
        c.execute("INSERT INTO file_receive_pending SELECT account,?1,?2,64,next_ticket FROM file_receive_scan", rusqlite::params![[51u8;32].as_slice(),[52u8;32].as_slice()])?;
        Ok(())
    }).unwrap();
    let (wait, pending) = node.file_pull_schedule(Duration::from_secs(300));
    assert!(pending, "blocked work must retain its recovery obligation");
    assert!(
        wait >= Duration::from_secs(300),
        "unchanged blocked work must not override backoff: {wait:?}"
    );
}

#[tokio::test]
async fn group_metadata_change_wakes_file_recovery_before_sparse_deadline() {
    use futures_util::FutureExt;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let _ = node.file_pull_notify.notified().now_or_never();
    let mut network = node.network_changes.subscribe();
    let mut roster = node.subscribe_roster_changes();
    let mut available = node.subscribe_sibling_availability();
    // Subscribe before work: a change during the work turn must remain pending.
    let mut channels = node.subscribe_channel_changes();
    let mut peers = node.file_pull_peers();
    let conversation = crate::eventlog::ConversationId::new([71; 32]);
    node.channel_changes.send(conversation).unwrap();
    let changed = tokio::time::timeout(
        Duration::from_secs(1),
        node.wait_for_file_pull(
            &mut network,
            &mut roster,
            &mut available,
            &mut channels,
            &mut peers,
            Duration::from_secs(300),
        ),
    )
    .await
    .unwrap();
    assert!(
        changed,
        "accepted group information invalidates file recovery immediately"
    );
}

#[tokio::test]
async fn lagged_group_metadata_subscription_repairs_file_recovery() {
    use futures_util::FutureExt;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let _ = node.file_pull_notify.notified().now_or_never();
    let mut network = node.network_changes.subscribe();
    let mut roster = node.subscribe_roster_changes();
    let mut available = node.subscribe_sibling_availability();
    let (sender, mut channels) = tokio::sync::broadcast::channel(1);
    let mut peers = node.file_pull_peers();
    for id in [72, 73] {
        sender
            .send(crate::eventlog::ConversationId::new([id; 32]))
            .unwrap();
    }
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            node.wait_for_file_pull(
                &mut network,
                &mut roster,
                &mut available,
                &mut channels,
                &mut peers,
                Duration::from_secs(300)
            )
        )
        .await
        .unwrap(),
        "lag is missed invalidation, not proof of no work"
    );
}

#[tokio::test]
async fn route_change_during_file_work_survives_until_the_next_wait() {
    use crate::discovery::Announce;
    use futures_util::FutureExt;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path());
    let _ = node.file_pull_notify.notified().now_or_never();
    let peer = DeviceIdentity::generate();
    let update = |port| {
        node.roster.lock().unwrap().update(
            &Announce::new(&peer, "peer", port),
            "127.0.0.1".parse().unwrap(),
            &node.user_id(),
        );
    };
    update(12345);
    let mut network = node.network_changes.subscribe();
    let mut roster = node.subscribe_roster_changes();
    let mut available = node.subscribe_sibling_availability();
    let mut channels = node.subscribe_channel_changes();
    let mut peers = node.file_pull_peers();
    update(12346);
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            node.wait_for_file_pull(
                &mut network,
                &mut roster,
                &mut available,
                &mut channels,
                &mut peers,
                Duration::from_secs(300)
            )
        )
        .await
        .unwrap(),
        "a route update during work must not be recaptured as an unchanged heartbeat"
    );
}

struct RoutedInventorySource {
    calls: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    endpoints: Vec<std::net::SocketAddr>,
    changes: tokio::sync::watch::Sender<u64>,
    first_gate: Option<tokio::sync::Semaphore>,
    held_turn: usize,
}
impl RoutedInventorySource {
    fn new(endpoints: Vec<std::net::SocketAddr>, held: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            endpoints,
            changes: tokio::sync::watch::channel(0).0,
            first_gate: held.then(|| tokio::sync::Semaphore::new(0)),
            held_turn: 0,
        })
    }
    fn changed(&self) {
        self.changes.send_modify(|generation| *generation += 1);
    }
}
struct RoutedInventoryRead<'a>(&'a AtomicUsize);
impl Drop for RoutedInventoryRead<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl crate::discovery::candidates::CandidateSource for RoutedInventorySource {
    fn snapshot(&self) -> futures_util::future::BoxFuture<'_, Vec<std::net::SocketAddr>> {
        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            let _read = RoutedInventoryRead(&self.active);
            if turn == self.held_turn
                && let Some(gate) = &self.first_gate
            {
                gate.acquire().await.unwrap().forget();
            }
            self.endpoints.clone()
        })
    }
    fn subscribe_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        Some(self.changes.subscribe())
    }
}

async fn routed_inventory_participant(
    directory: &std::path::Path,
    source: Arc<RoutedInventorySource>,
) -> (Arc<Node>, ParticipantRuntime) {
    let node = open(directory);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut running = PreparedParticipant::prepare(
        node.clone(),
        listener,
        Arc::new(DiscoveryVisibility::new(true)),
        ParticipantConfig {
            discovery_port: 0,
            candidate_source: Some(source),
        },
    )
    .unwrap()
    .launch("Routed inventory owner");
    let retired = running.retire_ambient_discovery_for_test().await;
    assert!(retired.failures.is_empty());
    (node, running)
}
async fn flush_routed_inventory() {
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn routed_inventory_empty_source_uses_sparse_repair() {
    let directory = tempfile::tempdir().unwrap();
    let source = RoutedInventorySource::new(Vec::new(), false);
    let (_node, running) = routed_inventory_participant(directory.path(), source.clone()).await;
    flush_routed_inventory().await;
    let startup = source.calls.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(299)).await;
    flush_routed_inventory().await;
    let idle = source.calls.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    flush_routed_inventory().await;
    let repaired = source.calls.load(Ordering::SeqCst);
    running.stop().await;
    assert_eq!(startup, 1, "initial discovery must be immediate");
    assert_eq!(
        idle, 1,
        "unchanged/absent provider must not rerun every five seconds"
    );
    assert_eq!(
        repaired, 2,
        "sparse repair must obtain a fresh provider snapshot"
    );
}

#[tokio::test(start_paused = true)]
async fn routed_inventory_cached_hints_keep_five_second_protocol_probes() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let source = RoutedInventorySource::new(vec![receiver.local_addr().unwrap()], false);
    let (_node, running) = routed_inventory_participant(directory.path(), source.clone()).await;
    flush_routed_inventory().await;
    let mut first = [0u8; 128];
    let first_packet = receiver.try_recv(&mut first);
    let startup = source.calls.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(5)).await;
    flush_routed_inventory().await;
    let mut second = [0u8; 128];
    let second_packet = receiver.try_recv(&mut second);
    let repeated = source.calls.load(Ordering::SeqCst);
    running.stop().await;
    let first_len = first_packet.expect("startup routed probe");
    let second_len = second_packet.expect("cached candidate must retain its five-second probe");
    let first_nonce = crate::discovery::query::decode_query(&first[..first_len]).unwrap();
    let second_nonce = crate::discovery::query::decode_query(&second[..second_len]).unwrap();
    assert_ne!(first_nonce, second_nonce, "liveness requires a fresh nonce");
    assert_eq!(startup, 1);
    assert_eq!(
        repeated, 1,
        "protocol liveness must reuse hints without re-running CLIs"
    );
}

#[tokio::test(start_paused = true)]
async fn routed_inventory_retains_coalesced_events_during_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let source = RoutedInventorySource::new(Vec::new(), true);
    let (node, running) = routed_inventory_participant(directory.path(), source.clone()).await;
    flush_routed_inventory().await;
    let active = source.active.load(Ordering::SeqCst);
    for _ in 0..64 {
        source.changed();
        running.trigger_discovery();
    }
    node.network_changes
        .send_modify(|generation| *generation += 1);
    source.first_gate.as_ref().unwrap().add_permits(1);
    flush_routed_inventory().await;
    let calls = source.calls.load(Ordering::SeqCst);
    let maximum = source.maximum.load(Ordering::SeqCst);
    running.stop().await;
    assert_eq!(active, 1, "hold the actual startup source future");
    assert_eq!(
        calls, 2,
        "events during a source read retain one coalesced follow-up"
    );
    assert_eq!(maximum, 1, "provider snapshots must remain serial");
}

#[tokio::test(start_paused = true)]
async fn routed_inventory_retirement_drops_active_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let source = RoutedInventorySource::new(Vec::new(), true);
    let (_node, running) = routed_inventory_participant(directory.path(), source.clone()).await;
    flush_routed_inventory().await;
    let active = source.active.load(Ordering::SeqCst);
    running.stop().await;
    source.changed();
    source.first_gate.as_ref().unwrap().add_permits(1);
    flush_routed_inventory().await;
    assert_eq!(active, 1);
    assert_eq!(source.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        1,
        "retired source cannot resume"
    );
    assert_eq!(
        source.changes.receiver_count(),
        0,
        "retirement drops topology subscription"
    );
}

#[tokio::test(start_paused = true)]
async fn routed_inventory_outer_timeout_keeps_only_hint_liveness() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut source = RoutedInventorySource::new(vec![receiver.local_addr().unwrap()], true);
    Arc::get_mut(&mut source).unwrap().held_turn = 1;
    let (_node, running) = routed_inventory_participant(directory.path(), source.clone()).await;
    flush_routed_inventory().await;
    let mut packet = [0u8; 128];
    let initial = receiver.try_recv(&mut packet);
    tokio::time::advance(Duration::from_millis(20)).await;
    flush_routed_inventory().await;
    source.changed();
    flush_routed_inventory().await;
    let held = source.active.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(3)).await;
    flush_routed_inventory().await;
    let active_after_timeout = source.active.load(Ordering::SeqCst);
    let after_timeout = receiver.try_recv(&mut packet);
    let calls = source.calls.load(Ordering::SeqCst);
    running.stop().await;
    assert!(initial.is_ok());
    assert_eq!(held, 1);
    assert_eq!(
        active_after_timeout, 0,
        "whole-source timeout drops its future"
    );
    assert_eq!(calls, 2);
    let length = after_timeout.expect("a provider timeout must not stop known hint probes");
    assert!(crate::discovery::query::decode_query(&packet[..length]).is_some());
}

#[tokio::test(start_paused = true)]
async fn routed_inventory_visibility_network_and_provider_events_refresh_promptly() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let source = RoutedInventorySource::new(vec![receiver.local_addr().unwrap()], false);
    let (node, running) = routed_inventory_participant(directory.path(), source.clone()).await;
    flush_routed_inventory().await;
    let mut packet = [0u8; 128];
    let startup = receiver.try_recv(&mut packet);
    tokio::time::advance(Duration::from_millis(20)).await;
    flush_routed_inventory().await;
    running.visibility.set_public(false).await;
    flush_routed_inventory().await;
    source.changed();
    running.trigger_discovery();
    node.network_changes
        .send_modify(|generation| *generation += 1);
    tokio::time::advance(Duration::from_secs(5)).await;
    flush_routed_inventory().await;
    let private_reads = source.calls.load(Ordering::SeqCst);
    let private_packet = receiver.try_recv(&mut packet);
    running.visibility.set_physical_lan_only(true).await;
    running.visibility.set_public(true).await;
    flush_routed_inventory().await;
    let physical_reads = source.calls.load(Ordering::SeqCst);
    running.visibility.set_physical_lan_only(false).await;
    flush_routed_inventory().await;
    let reactivated = source.calls.load(Ordering::SeqCst);
    // The source owner is pacing its current UDP sweep here. Retain the event
    // until that bounded sweep completes instead of requiring another timer.
    node.network_changes
        .send_modify(|generation| *generation += 1);
    tokio::time::advance(Duration::from_millis(20)).await;
    flush_routed_inventory().await;
    let network = source.calls.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_millis(20)).await;
    flush_routed_inventory().await;
    running.trigger_discovery();
    flush_routed_inventory().await;
    let manual = source.calls.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_millis(20)).await;
    flush_routed_inventory().await;
    source.changed();
    flush_routed_inventory().await;
    let provider = source.calls.load(Ordering::SeqCst);
    running.stop().await;
    assert!(startup.is_ok());
    assert!(matches!(private_packet, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    assert_eq!(
        (private_reads, physical_reads),
        (1, 1),
        "privacy suppresses inventories and probes"
    );
    assert_eq!(
        reactivated, 2,
        "public routed activation refreshes immediately"
    );
    assert_eq!(network, 3, "network demand survives current probe pacing");
    assert_eq!(manual, 4, "explicit rescan does not await sparse repair");
    assert_eq!(
        provider, 5,
        "provider topology notification does not await sparse repair"
    );
}

#[tokio::test]
async fn routed_inventory_disclosure_change_during_read_remains_pending() {
    let (updates, mut changes) = tokio::sync::watch::channel(0u64);
    let (release, held) = tokio::sync::oneshot::channel();
    let mut observation = Box::pin(super::participant_runtime::observe_candidate_disclosure(
        &mut changes,
        async {
            // The visibility read captured false; activation occurs before the
            // owner receives that result and decides whether to refresh.
            let allowed = false;
            held.await.unwrap();
            allowed
        },
    ));
    use std::{future::Future, task::Poll};
    std::future::poll_fn(|cx| {
        assert!(observation.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    updates.send_modify(|generation| *generation += 1);
    release.send(()).unwrap();
    assert!(!observation.await);
    assert!(
        changes.has_changed().unwrap(),
        "activation after the visibility read must retain a fresh inventory turn"
    );
}
