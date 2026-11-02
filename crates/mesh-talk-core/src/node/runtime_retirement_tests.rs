use super::*;
use crate::eventlog::{ConversationId, Event, EventKind};

async fn start(base: &Path) -> NodeRuntime {
    NodeRuntime::start(
        base,
        "owner",
        "Owner",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap()
}

async fn retire(runtime: NodeRuntime) {
    runtime.stop().await;
}

struct RewriteRelease(Option<std::sync::mpsc::Sender<()>>);
impl Drop for RewriteRelease {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

struct RewriteObservation {
    initialized_early: bool,
    blocked_workers: usize,
    // Keep the sender valid when normal retirement finishes after observation.
    _initialized: std::sync::mpsc::Receiver<()>,
}

struct RewriteObserver {
    arm: Option<std::sync::mpsc::Sender<()>>,
    retirement_started: tokio::sync::oneshot::Receiver<()>,
    observed: tokio::sync::oneshot::Receiver<Result<RewriteObservation, &'static str>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl RewriteObserver {
    fn new(
        release: std::sync::mpsc::Sender<()>,
        initialized: std::sync::mpsc::Receiver<()>,
        work: Arc<crate::node::runtime_work::RuntimeWork>,
        blocked: Arc<std::sync::atomic::AtomicUsize>,
        required_blocked: usize,
    ) -> Self {
        let (arm, armed) = std::sync::mpsc::channel();
        let (ready, retirement_started) = tokio::sync::oneshot::channel();
        let (done, observed) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            let release = RewriteRelease(Some(release));
            let observation = (|| {
                // This independent safety uses the existing setup bound. A
                // stalled reactor cannot leave the artificial gate held forever.
                let setup_deadline = std::time::Instant::now() + Duration::from_secs(6);
                armed
                    .recv_timeout(
                        setup_deadline.saturating_duration_since(std::time::Instant::now()),
                    )
                    .map_err(|_| "rewrite observer was not armed within setup")?;
                while !work.is_closed()
                    || blocked.load(std::sync::atomic::Ordering::SeqCst) < required_blocked
                {
                    if std::time::Instant::now() >= setup_deadline {
                        return Err("retirement or controlled blockers did not enter within setup");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                ready
                    .send(())
                    .map_err(|_| "rewrite observer caller retired")?;
                let initialized_early = match initialized.recv_timeout(Duration::from_millis(150)) {
                    Ok(()) => true,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err("retirement ended without reporting initialization");
                    }
                };
                Ok(RewriteObservation {
                    initialized_early,
                    blocked_workers: blocked.load(std::sync::atomic::Ordering::SeqCst),
                    _initialized: initialized,
                })
            })();
            // Release before asking the async caller to make any progress.
            drop(release);
            let _ = done.send(observation);
        });
        Self {
            arm: Some(arm),
            retirement_started,
            observed,
            worker: Some(worker),
        }
    }
    fn arm(&mut self) {
        self.arm.take().unwrap().send(()).unwrap();
    }
    async fn finish(&mut self) -> RewriteObservation {
        let result = (&mut self.observed).await;
        self.worker.take().unwrap().join().unwrap();
        result
            .unwrap()
            .expect("independent rewrite observation must qualify")
    }
}
impl Drop for RewriteObserver {
    fn drop(&mut self) {
        // Disconnect an unarmed observer on every panic/cancellation path; its
        // independent thread releases the writer and is always joined here.
        self.arm.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct JoinedBlockers(Option<std::thread::JoinHandle<()>>);
impl JoinedBlockers {
    fn join(&mut self) {
        if let Some(worker) = self.0.take() {
            worker.join().unwrap();
        }
    }
}
impl Drop for JoinedBlockers {
    fn drop(&mut self) {
        if let Some(worker) = self.0.take() {
            let _ = worker.join();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_waits_for_actual_profile_rewrite_before_reopening() {
    profile_rewrite_retirement_case(false).await;
}

// This companion qualifies only the observer's independent gate release. The
// unforced test above remains the primary retirement/initializer timing oracle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_rewrite_observer_releases_gate_with_both_runtime_workers_blocked() {
    profile_rewrite_retirement_case(true).await;
}

async fn profile_rewrite_retirement_case(starve_workers: bool) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = start(dir.path()).await;
    let node = runtime.node.clone();
    let conv = ConversationId::new([77; 32]);
    let (selected_tx, selected_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let (initialized_tx, initialized_rx) = std::sync::mpsc::channel();
    let blocked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Reverse drop order releases the gate before joining controlled children.
    let mut blocked_executor = JoinedBlockers(None);
    let mut observer;
    {
        let mut log = node.log.lock().unwrap();
        for seq in 1..=10 {
            log.append(Event::new(
                &node.identity,
                conv,
                seq,
                vec![],
                seq,
                0,
                EventKind::Profile,
                vec![seq as u8],
            ))
            .unwrap();
        }
        // Own the independent release before the hook can hold log/SQL locks.
        observer = RewriteObserver::new(
            release_tx,
            initialized_rx,
            node.runtime_work.clone(),
            blocked.clone(),
            if starve_workers { 2 } else { 0 },
        );
        log.before_rewrite_rename(Box::new(move || {
            let _ = selected_tx.send(());
            let _ = release_rx.lock().unwrap().recv();
        }));
        log.request_profile_compaction(conv).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(6), selected_rx)
        .await
        .unwrap()
        .unwrap();
    let blockers = if starve_workers {
        (0..2)
            .map(|_| (node.clone(), node.runtime_work.admit().unwrap()))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    drop(node);
    if starve_workers {
        let blocked = blocked.clone();
        blocked_executor.0 = Some(std::thread::spawn(move || {
            // These two admitted async children use an isolated executor. Its
            // saturation qualifies gate liveness, not production stop latency.
            let executor = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .build()
                .unwrap();
            executor.block_on(async move {
                let mut tasks = Vec::new();
                for (node, permit) in blockers {
                    let blocked = blocked.clone();
                    tasks.push(tokio::spawn(permit.track(async move {
                        assert!(matches!(
                            node.log.try_lock(),
                            Err(std::sync::TryLockError::WouldBlock)
                        ));
                        blocked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _log = node.log.lock().unwrap();
                    })));
                }
                for task in tasks {
                    task.await.unwrap();
                }
            });
        }));
    }
    observer.arm();
    let mut retirement = Box::pin(retire(runtime));
    // Enter real stop admission before queueing the remainder. This first poll
    // cannot depend on a background task being scheduled to begin retirement.
    let first = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(retirement.as_mut(), cx))
    })
    .await;
    let mut initialized_tx = Some(initialized_tx);
    if first.is_ready() {
        initialized_tx.take().unwrap().send(()).unwrap();
    }
    let base = dir.path().to_path_buf();
    let replacement = tokio::spawn(async move {
        if first.is_pending() {
            retirement.await;
            initialized_tx.take().unwrap().send(()).unwrap();
        }
        start(&base).await
    });
    (&mut observer.retirement_started)
        .await
        .expect("actual retirement and controlled blockers must enter before observation");
    let observation = observer.finish().await;
    blocked_executor.join();
    if starve_workers {
        assert_eq!(
            observation.blocked_workers, 2,
            "both admitted runtime workers must encounter the actual held log before OS release"
        );
    }
    let replacement = replacement.await.unwrap();
    assert!(
        !observation.initialized_early,
        "replacement initializer ran while old rewrite was paused"
    );
    let event = Event::new(
        &replacement.node.identity,
        conv,
        11,
        vec![],
        11,
        0,
        EventKind::Message,
        b"replacement event".to_vec(),
    );
    let event_id = event.id;
    replacement.node.log.lock().unwrap().append(event).unwrap();
    retire(replacement).await;
    let reopened = start(dir.path()).await;
    assert!(
        reopened
            .node
            .log
            .lock()
            .unwrap()
            .get(&event_id)
            .unwrap()
            .is_some()
    );
    retire(reopened).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_waits_for_admitted_blocking_delivery_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = start(dir.path()).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *runtime.node.delivery_snapshot_hook.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    }));
    runtime.node.delivery_notify.notify_one();
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let mut retirement = tokio::spawn(retire(runtime));
    let finished_early = tokio::time::timeout(Duration::from_millis(150), &mut retirement)
        .await
        .is_ok();
    release_tx.send(()).unwrap();
    if !finished_early {
        tokio::time::timeout(Duration::from_secs(3), retirement)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        !finished_early,
        "retirement finished before admitted blocking recovery exited"
    );
}

#[tokio::test]
async fn retirement_terminates_an_accepted_stalled_handshake() {
    use tokio::io::AsyncReadExt;
    let dir = tempfile::tempdir().unwrap();
    let runtime = start(dir.path()).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    *runtime.node.accepted_hook.lock().unwrap() = Some(entered_tx);
    let mut stream =
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, runtime.listen_tcp_port()))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    retire(runtime).await;
    let result = tokio::time::timeout(Duration::from_millis(150), stream.read(&mut [0; 1])).await;
    assert!(
        matches!(result, Ok(Ok(0))),
        "accepted connection survived retirement: {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_waits_for_peer_cache_closure_after_its_caller_is_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = start(dir.path()).await;
    let node = runtime.node.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *node.peer_snapshot_hook.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    }));
    let caller = node.clone();
    runtime.participant.tasks.push(tokio::spawn(async move {
        caller.cached_peer_snapshot_async().await.unwrap();
    }));
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let mut retirement = tokio::spawn(retire(runtime));
    let finished_early = tokio::time::timeout(Duration::from_millis(150), &mut retirement)
        .await
        .is_ok();
    release_tx.send(()).unwrap();
    if !finished_early {
        tokio::time::timeout(Duration::from_secs(3), retirement)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        !finished_early,
        "retirement finished before peer cache closure exited"
    );
    assert!(node.cached_peer_snapshot_async().await.is_err());
}

#[tokio::test]
async fn normal_retirement_finishes_without_peers_or_pending_work() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = start(dir.path()).await;
    tokio::time::timeout(Duration::from_secs(3), runtime.stop())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retirement_waits_for_authenticated_private_route_probe_persistence() {
    let alice_dir = tempfile::tempdir().unwrap();
    let bob_dir = tempfile::tempdir().unwrap();
    let mut alice = start(alice_dir.path()).await;
    let bob = start(bob_dir.path()).await;
    // Isolate this explicit probe from discovery, accept/delivery, and periodic
    // probes. Join producers and admitted children without closing admission.
    for task in &alice.participant.tasks {
        task.abort();
    }
    for task in std::mem::take(&mut alice.participant.tasks) {
        let _ = task.await;
    }
    alice.node.runtime_work.drain().await;
    let node = alice.node.clone();
    let public = bob.node.identity.public();
    bob.set_display_name("new authenticated name");
    let old_presence = bob.node.signed_announce("old name", bob.listen_tcp_port());
    node.remember_peer(
        &public,
        Some(&old_presence),
        std::net::Ipv4Addr::LOCALHOST.into(),
    )
    .unwrap();
    *node.roster.lock().unwrap() = Roster::default();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *node.remember_peer_hook.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    }));
    let probe = node.clone();
    alice.participant.tasks.push(tokio::spawn(async move {
        probe
            .probe_private_routes_with_budget(Duration::from_secs(5))
            .await;
    }));
    let entered = tokio::time::timeout(Duration::from_secs(3), entered_rx).await;
    if !matches!(entered, Ok(Ok(()))) {
        let _ = release_tx.send(());
        alice.stop().await;
        bob.stop().await;
        panic!("authenticated route probe did not enter persistence within its original deadline");
    }
    let mut retirement = tokio::spawn(alice.stop());
    let finished_early = tokio::time::timeout(Duration::from_millis(150), &mut retirement)
        .await
        .is_ok();
    release_tx.send(()).unwrap();
    if !finished_early {
        tokio::time::timeout(Duration::from_secs(3), retirement)
            .await
            .unwrap()
            .unwrap();
    }
    if finished_early {
        node.runtime_work.drain().await;
        bob.stop().await;
        panic!(
            "retirement completed while authenticated route probe was paused before persistence"
        );
    }
    assert!(
        !finished_early,
        "retirement completed while authenticated route probe was paused before persistence"
    );
    // Reopening spawns fresh probes immediately. Stop the remote before reopen
    // so a new authentication cannot mask missing pre-retirement persistence.
    let bob_port = bob.listen_tcp_port();
    bob.stop().await;
    drop(node);
    let reopened = start(alice_dir.path()).await;
    let proof = reopened
        .node
        .historical_author(&public.ed25519_pub)
        .unwrap();
    let same_port = reopened
        .node
        .cached_routing_peers()
        .iter()
        .any(|peer| peer.public == public && peer.addr.port() == bob_port);
    reopened.stop().await;
    assert_eq!(proof.name, "new authenticated name");
    assert!(same_port);
}

#[tokio::test]
async fn connection_limit_releases_slots_and_retirement_closes_all_owned_connections() {
    use tokio::io::AsyncReadExt;
    let dir = tempfile::tempdir().unwrap();
    let runtime = start(dir.path()).await;
    let address = (std::net::Ipv4Addr::LOCALHOST, runtime.listen_tcp_port());
    let mut streams = Vec::new();
    for _ in 0..crate::node::serving::MAX_CONCURRENT_CONNS {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        *runtime.node.accepted_hook.lock().unwrap() = Some(entered_tx);
        streams.push(tokio::net::TcpStream::connect(address).await.unwrap());
        tokio::time::timeout(Duration::from_secs(3), entered_rx)
            .await
            .unwrap()
            .unwrap();
    }
    let (entered_tx, mut entered_rx) = tokio::sync::oneshot::channel();
    *runtime.node.accepted_hook.lock().unwrap() = Some(entered_tx);
    streams.push(tokio::net::TcpStream::connect(address).await.unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut entered_rx)
            .await
            .is_err(),
        "excess connection was accepted before a slot freed"
    );
    drop(streams.remove(0));
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), runtime.stop())
        .await
        .unwrap();
    for mut stream in streams {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stream.read(&mut [0; 1]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_waits_for_file_staging_after_adopted_command_cancellation() {
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = start(dir.path()).await;
    let node = runtime.handle();
    let remote = Account::generate();
    let proof = Announce::new_with_account(&DeviceIdentity::generate(), &remote, "Remote", 1);
    node.roster
        .lock()
        .unwrap()
        .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
    let account = remote.account_id();
    let path = dir.path().join("staging.txt");
    std::fs::write(&path, b"blocking staging must retire before profile reuse").unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let sender = node.clone();
    let target = account.clone();
    let source = path.clone();
    runtime
        .participant
        .register_host_task(tokio::spawn(async move {
            let mut entered = Some(entered);
            let _ = sender
                .enqueue_file_to_account_progress(
                    &target,
                    &source,
                    crate::file::FileKind::File,
                    move |_| {
                        if let Some(entered) = entered.take() {
                            entered.send(()).unwrap();
                            released.recv().unwrap();
                        }
                    },
                )
                .await;
        }));
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let mut retirement = tokio::spawn(runtime.stop());
    let finished_early = tokio::time::timeout(Duration::from_millis(150), &mut retirement)
        .await
        .is_ok();
    release.send(()).unwrap();
    if !finished_early {
        retirement.await.unwrap();
    }
    assert!(
        !finished_early,
        "retirement returned while adopted file staging still wrote"
    );
    assert!(
        node.enqueue_file_to_account_progress(&account, &path, crate::file::FileKind::File, |_| {})
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_identity_precedes_blocked_durable_store_and_matches_runtime() {
    let root = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    NAMES_OPEN_HOOKS.lock().unwrap().insert(
        root.path().join("accounts/owner/names.log"),
        Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }),
    );
    let (identity_tx, mut identity_rx) = tokio::sync::oneshot::channel();
    let base = root.path().to_path_buf();
    let startup = tokio::spawn(async move {
        NodeRuntime::start_configured_guarded_with_network_observed(
            RuntimeConfig {
                base_dir: &base,
                account_id: "owner",
                display_name: "Owner",
                password: "pw",
                discovery_port: 0,
                post_office_enabled: false,
                candidate_source: None,
            },
            RuntimeEvents {
                on_dm: Box::new(|_| {}),
                on_channel: Box::new(|_| {}),
                on_file: Box::new(|_| {}),
                on_profile: Box::new(|_| {}),
                on_call_signal: Box::new(|_| {}),
            },
            RuntimeNetworkOptions::default(),
            move |identity| {
                identity_tx.send(identity).unwrap();
                Ok(())
            },
            |launch| launch("Owner"),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(15), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let identity = identity_rx.try_recv();
    let still_preparing = !startup.is_finished();
    release_tx.send(()).unwrap();
    let runtime = startup.await.unwrap().unwrap();
    let actual_account = runtime.account_id().to_owned();
    let actual_device = runtime.user_id().to_owned();
    runtime.stop().await;
    assert!(
        still_preparing,
        "durable preparation still blocks readiness"
    );
    let identity = identity.expect("authenticated identity must precede durable store readiness");
    assert_eq!(identity.account_id, actual_account);
    assert_eq!(identity.device_id, actual_device);
}

#[tokio::test]
async fn startup_identity_rejection_stops_before_listener_and_durable_stores() {
    let root = tempfile::tempdir().unwrap();
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback_observed = observed.clone();
    let result = NodeRuntime::start_configured_guarded_with_network_observed(
        RuntimeConfig {
            base_dir: root.path(),
            account_id: "owner",
            display_name: "Owner",
            password: "pw",
            discovery_port: 0,
            post_office_enabled: false,
            candidate_source: None,
        },
        RuntimeEvents {
            on_dm: Box::new(|_| {}),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|_| {}),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|_| {}),
        },
        RuntimeNetworkOptions::default(),
        move |_| {
            callback_observed.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "superseded").into())
        },
        |_| panic!("rejected identity cannot launch producers"),
    )
    .await;
    assert!(result.is_err());
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    for file in ["listen-port", "messages.log", "sent.log", "names.log"] {
        assert!(!root.path().join("accounts/owner").join(file).exists());
    }
    // A wrong password must fail without publishing even public identifiers.
    let result = NodeRuntime::start_configured_guarded_with_network_observed(
        RuntimeConfig {
            base_dir: root.path(),
            account_id: "owner",
            display_name: "Owner",
            password: "wrong",
            discovery_port: 0,
            post_office_enabled: false,
            candidate_source: None,
        },
        RuntimeEvents {
            on_dm: Box::new(|_| {}),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|_| {}),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|_| {}),
        },
        RuntimeNetworkOptions::default(),
        |_| panic!("failed keystore authentication cannot publish identity"),
        |_| panic!("failed keystore authentication cannot launch producers"),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn profile_contention_precedes_runtime_keystore_creation() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("accounts/owner");
    let _lease = crate::storage::profile::ProfileStorageLease::acquire(&profile).unwrap();
    let result = NodeRuntime::start(
        directory.path(),
        "owner",
        "Owner",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await;
    assert!(result.is_err(), "runtime ignored an existing profile owner");
    assert!(
        !profile.join("identity.keystore").exists(),
        "contender created device keys"
    );
    assert!(
        !profile.join("account.keystore").exists(),
        "contender created account keys"
    );
}

#[tokio::test]
async fn participant_rekey_reopens_same_device_with_exact_next_account() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = start(directory.path()).await;
    let device = runtime.user_id().to_owned();
    let previous = runtime.account_id().to_owned();
    let next = runtime.rekey_account().unwrap();
    assert_ne!(previous, next);
    runtime.stop().await;
    let replacement = start(directory.path()).await;
    assert_eq!(replacement.user_id(), device);
    assert_eq!(replacement.account_id(), next);
    replacement.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceled_startup_completion_waits_for_later_privacy_writer() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("accounts/owner");
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    PRIVACY_OPEN_HOOKS.lock().unwrap().insert(
        profile.clone(),
        Box::new(move || {
            entered.send(()).unwrap();
            released.recv().unwrap();
        }),
    );
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    let base = directory.path().to_owned();
    let startup = tokio::spawn(async move {
        let request = super::super::RuntimeStartup::new(
            RuntimeConfig {
                base_dir: &base,
                account_id: "owner",
                display_name: "Owner",
                password: "pw",
                discovery_port: 0,
                post_office_enabled: false,
                candidate_source: None,
            },
            RuntimeEvents {
                on_dm: Box::new(|_| {}),
                on_channel: Box::new(|_| {}),
                on_file: Box::new(|_| {}),
                on_profile: Box::new(|_| {}),
                on_call_signal: Box::new(|_| {}),
            },
        );
        completion_tx.send(request.completion()).ok().unwrap();
        NodeRuntime::start_owned(request, |_| Ok(()), |launch| launch("Owner")).await
    });
    let completion = completion_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), started)
        .await
        .unwrap()
        .unwrap();
    startup.abort();
    assert!(matches!(startup.await, Err(error) if error.is_cancelled()));
    let mut completed = tokio::spawn(completion.wait());
    let early = tokio::time::timeout(Duration::from_millis(100), &mut completed)
        .await
        .is_ok();
    assert!(crate::storage::profile::ProfileStorageLease::acquire(&profile).is_err());
    release.send(()).unwrap();
    if !early {
        tokio::time::timeout(Duration::from_secs(10), completed)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(!early, "startup completion lost the later blocking child");
    assert!(crate::storage::profile::ProfileStorageLease::acquire(&profile).is_ok());
}

#[tokio::test]
async fn retired_runtime_exports_without_reopening_and_closes_before_relocation() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = start(directory.path()).await;
    let profile = directory.path().join("accounts/owner");
    let account = runtime.account_id().to_owned();
    let retired = runtime.stop_for_storage().await;
    assert!(Node::open_recovery_profile(&profile, "pw").is_err());
    let mut snapshot = Vec::new();
    retired
        .node()
        .write_recovery_snapshot(&mut snapshot, 1)
        .unwrap();
    assert!(!snapshot.is_empty());
    assert_eq!(retired.node().account_id(), account);
    retired.close().unwrap();
    let moved = directory.path().join("closed-profile");
    std::fs::rename(&profile, &moved).unwrap();
    let reopened = Node::open_recovery_profile(&moved, "pw").unwrap();
    assert_eq!(reopened.account_id(), account);
    reopened.close_profile().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn redirected_account_keystore_is_rejected_before_device_creation() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("accounts/owner");
    std::fs::create_dir_all(&profile).unwrap();
    let external = directory.path().join("external-account");
    std::fs::write(&external, b"existing private authority").unwrap();
    std::os::unix::fs::symlink(&external, profile.join("account.keystore")).unwrap();
    let result = NodeRuntime::start(
        directory.path(),
        "owner",
        "Owner",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await;
    assert!(result.is_err());
    assert!(!profile.join("identity.keystore").exists());
    assert_eq!(
        std::fs::read(external).unwrap(),
        b"existing private authority"
    );
}

#[tokio::test]
async fn profile_owner_blocks_subprocess_before_any_keystore_creation() {
    if let Some(base) = std::env::var_os("MESH_TEST_RUNTIME_CONTENDER") {
        let result = NodeRuntime::start(
            Path::new(&base),
            "owner",
            "Owner",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await;
        assert!(result.is_err(), "subprocess ignored the profile lease");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("accounts/owner");
    let _owner = crate::storage::profile::ProfileStorageLease::acquire(&profile).unwrap();
    assert!(std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact","node::runtime::retirement_tests::profile_owner_blocks_subprocess_before_any_keystore_creation","--nocapture"])
        .env("MESH_TEST_RUNTIME_CONTENDER",directory.path()).status().unwrap().success());
    assert!(!profile.join("identity.keystore").exists());
    assert!(!profile.join("account.keystore").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_joins_actual_expiry_writer_before_profile_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = start(directory.path()).await;
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    *runtime.node.expiry_cleanup_hook.lock().unwrap() = Some(Box::new(move || {
        entered.send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
    }));
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    let mut retirement = tokio::spawn(runtime.stop());
    let early = tokio::time::timeout(Duration::from_millis(150), &mut retirement)
        .await
        .is_ok();
    release.send(()).unwrap();
    if !early {
        tokio::time::timeout(Duration::from_secs(5), retirement)
            .await
            .unwrap()
            .unwrap();
    }
    let profile = directory.path().join("accounts/owner");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if crate::storage::profile::ProfileStorageLease::acquire(&profile).is_ok() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expiry worker did not release profile"
        );
        tokio::task::yield_now().await;
    }
    assert!(
        !early,
        "runtime stop completed while its expiry writer was still active"
    );
    let reopened = start(directory.path()).await;
    reopened.stop().await;
}
