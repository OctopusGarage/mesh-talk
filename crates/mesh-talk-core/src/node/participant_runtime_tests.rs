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
