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
