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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_waits_for_actual_profile_rewrite_before_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = start(dir.path()).await;
    let node = runtime.node.clone();
    let conv = ConversationId::new([77; 32]);
    let (selected_tx, selected_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
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
        log.before_rewrite_rename(Box::new(move || {
            selected_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }));
        log.request_profile_compaction(conv);
    }
    tokio::time::timeout(Duration::from_secs(6), selected_rx)
        .await
        .unwrap()
        .unwrap();
    let (initialized_tx, mut initialized_rx) = tokio::sync::oneshot::channel();
    let base = dir.path().to_path_buf();
    let replacement = tokio::spawn(async move {
        retire(runtime).await;
        initialized_tx.send(()).unwrap();
        start(&base).await
    });
    let initialized_early = tokio::time::timeout(Duration::from_millis(150), &mut initialized_rx)
        .await
        .is_ok();
    release_tx.send(()).unwrap();
    let replacement = replacement.await.unwrap();
    assert!(
        !initialized_early,
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
    assert!(reopened.node.log.lock().unwrap().get(&event_id).is_some());
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
    runtime.tasks.push(tokio::spawn(async move {
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
    let mut bob = start(bob_dir.path()).await;
    let node = alice.node.clone();
    let public = bob.node.identity.public();
    let old_presence = bob.node.signed_announce("old name", bob.listen_tcp_port());
    node.remember_peer(
        &public,
        Some(&old_presence),
        std::net::Ipv4Addr::LOCALHOST.into(),
    )
    .unwrap();
    *node.roster.lock().unwrap() = Roster::default();
    bob.set_display_name("new authenticated name");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *node.remember_peer_hook.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    }));
    let probe = node.clone();
    alice.tasks.push(tokio::spawn(async move {
        probe
            .probe_private_routes_with_budget(Duration::from_secs(5))
            .await;
    }));
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
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
    assert!(
        !finished_early,
        "retirement completed while authenticated route probe was paused before persistence"
    );
    let reopened = start(alice_dir.path()).await;
    let proof = reopened
        .node
        .historical_author(&public.ed25519_pub)
        .unwrap();
    assert_eq!(proof.name, "new authenticated name");
    assert!(reopened
        .node
        .cached_routing_peers()
        .iter()
        .any(|peer| peer.public == public && peer.addr.port() == bob.listen_tcp_port()));
    reopened.stop().await;
    bob.stop().await;
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
