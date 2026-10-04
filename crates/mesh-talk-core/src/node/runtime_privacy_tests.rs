use super::*;
use std::time::Duration;

async fn start(
    base: &Path,
    owner: &str,
    port: u16,
) -> (
    NodeRuntime,
    tokio::sync::mpsc::UnboundedReceiver<ReceivedDm>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let rt = NodeRuntime::start(
        base,
        owner,
        owner,
        "pw",
        port,
        move |dm| {
            let _ = tx.send(dm);
        },
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    (rt, rx)
}

#[tokio::test]
async fn manual_allow_publishes_private_return_presence_without_sending_dm() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, mut arx) = start(dir.path(), "alice", 0).await;
    let (bob, _) = start(dir.path(), "bob", 0).await;
    alice.set_invisible(true).await.unwrap();
    let ba = bob.node.signed_announce("Bob", bob.listen_tcp_port());
    alice.roster.lock().unwrap().update(
        &ba,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        alice.user_id(),
    );
    assert!(bob.peer_public(alice.user_id()).is_none());
    alice.set_allowed(bob.account_id(), true).await.unwrap();
    assert_eq!(
        bob.peers()
            .into_iter()
            .find(|p| p.public.user_id() == alice.user_id())
            .unwrap()
            .addr
            .port(),
        alice.listen_tcp_port()
    );
    bob.send_dm(alice.user_id(), b"first message is the return message")
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), arx.recv())
            .await
            .unwrap()
            .unwrap()
            .text,
        b"first message is the return message"
    );
}

#[tokio::test]
async fn public_runtime_can_still_send_to_legacy_discovered_peer_without_account_proof() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = start(dir.path(), "alice", 0).await;
    let (bob, mut brx) = start(dir.path(), "bob", 0).await;
    // Isolate this seeded legacy discovery fixture from live interface scans:
    // policy/admission stays public, only ambient UDP advertisements are muted.
    for runtime in [&alice, &bob] {
        let visibility = runtime
            .node
            .privacy
            .state
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .visibility
            .clone();
        visibility.set_public(false).await;
    }
    let legacy =
        crate::discovery::Announce::new(&bob.node.identity, "Legacy Bob", bob.listen_tcp_port());
    alice.roster.lock().unwrap().update(
        &legacy,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        alice.user_id(),
    );
    alice
        .send_dm(bob.user_id(), b"public legacy compatible")
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), brx.recv())
            .await
            .unwrap()
            .unwrap()
            .text,
        b"public legacy compatible"
    );
    assert!(
        alice.privacy_snapshot().allowed_accounts.is_empty(),
        "unverified legacy presence must never become an account grant"
    );
}

#[tokio::test]
async fn both_invisible_runtimes_restart_with_pinned_routes_and_return_dm() {
    let dir = tempfile::tempdir().unwrap();
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let (alice, _) = start(dir.path(), "alice", port).await;
    let (bob, mut brx) = start(dir.path(), "bob", port).await;
    alice.set_invisible(true).await.unwrap();
    // Verified LAN presence is received passively while Alice is invisible.
    let ba = bob.node.signed_announce("Bob", bob.listen_tcp_port());
    let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    udp.send_to(
        &crate::discovery::announce::encode(&ba),
        (std::net::Ipv4Addr::LOCALHOST, port),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while alice.peer_public(bob.user_id()).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    alice.send_dm(bob.user_id(), b"first").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), brx.recv())
            .await
            .unwrap()
            .unwrap()
            .text,
        b"first"
    );
    bob.set_allowed(alice.account_id(), true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    let ports = (alice.listen_tcp_port(), bob.listen_tcp_port());
    let ids = (alice.user_id().to_owned(), bob.user_id().to_owned());
    drop(alice);
    drop(bob);
    // Aborted accept tasks must release sockets before the restart bind.
    tokio::task::yield_now().await;
    let (alice, mut arx) = start(dir.path(), "alice", port).await;
    let (bob, _) = start(dir.path(), "bob", port).await;
    assert_eq!((alice.listen_tcp_port(), bob.listen_tcp_port()), ports);
    assert!(alice.privacy_snapshot().invisible && bob.privacy_snapshot().invisible);
    assert!(alice.peer_public(&ids.1).is_some());
    assert!(bob.peer_public(&ids.0).is_some());
    alice.roster.lock().unwrap().evict_stale(Duration::ZERO);
    bob.roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert!(alice.peers().is_empty() && bob.peers().is_empty());
    bob.send_dm(&ids.0, b"after both restart").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), arx.recv())
            .await
            .unwrap()
            .unwrap()
            .text,
        b"after both restart"
    );
}

#[tokio::test]
async fn offline_cached_route_is_not_fresh_online_presence_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = start(dir.path(), "alice", 0).await;
    let (bob, _) = start(dir.path(), "bob", 0).await;
    alice.set_invisible(true).await.unwrap();
    let proof = bob.node.signed_announce("Bob", bob.listen_tcp_port());
    alice.roster.lock().unwrap().update(
        &proof,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        alice.user_id(),
    );
    alice.set_allowed(bob.account_id(), true).await.unwrap();
    let bob_id = bob.user_id().to_owned();
    drop(alice);
    drop(bob);
    tokio::task::yield_now().await;
    let (alice, _) = start(dir.path(), "alice", 0).await;
    assert!(
        alice.peers().iter().all(|p| p.public.user_id() != bob_id),
        "cached route falsely refreshed offline presence"
    );
}

#[tokio::test]
async fn pinned_private_probe_refreshes_only_live_peers_without_new_grants_or_events() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = start(dir.path(), "alice", 0).await;
    let (bob, _) = start(dir.path(), "bob", 0).await;
    let proof = bob.node.signed_announce("Bob", bob.listen_tcp_port());
    alice.roster.lock().unwrap().update(
        &proof,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        alice.user_id(),
    );
    alice.set_allowed(bob.account_id(), true).await.unwrap();
    bob.set_allowed(alice.account_id(), true).await.unwrap();
    alice.set_invisible(true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    let policy = alice.privacy_snapshot();
    let conversations = alice.node.log.lock().unwrap().conversations();
    alice.roster.lock().unwrap().evict_stale(Duration::ZERO);
    alice.node.probe_private_routes().await;
    assert!(alice
        .peers()
        .iter()
        .any(|p| p.public.user_id() == bob.user_id()));
    assert_eq!(alice.privacy_snapshot(), policy);
    assert_eq!(
        alice.node.log.lock().unwrap().conversations(),
        conversations
    );
    drop(bob);
    tokio::task::yield_now().await;
    alice.roster.lock().unwrap().evict_stale(Duration::ZERO);
    alice.node.probe_private_routes().await;
    assert!(
        alice.peers().is_empty(),
        "failed probes must not refresh cached presence"
    );
}

#[tokio::test]
async fn explicit_contact_can_accept_verified_remote_rekey_without_transferring_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = start(dir.path(), "alice", 0).await;
    let (bob, mut brx) = start(dir.path(), "bob", 0).await;
    let proof = bob.node.signed_announce("Bob", bob.listen_tcp_port());
    alice.roster.lock().unwrap().update(
        &proof,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        alice.user_id(),
    );
    let old = bob.account_id().to_owned();
    alice
        .send_to_account(&old, b"before rekey", None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), brx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(alice
        .account_history(&old, 100)
        .iter()
        .any(|e| e.text == b"before rekey"));
    let public = bob.node.identity.public();
    let new = bob.rekey_account().unwrap();
    drop(bob);
    tokio::task::yield_now().await;
    let (bob, mut brx) = start(dir.path(), "bob", 0).await;
    assert_eq!(bob.node.identity.public(), public);
    assert_eq!(bob.account_id(), new);
    let new_proof = bob
        .node
        .signed_announce("Rekeyed Bob", bob.listen_tcp_port());
    alice.roster.lock().unwrap().update(
        &new_proof,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        alice.user_id(),
    );
    assert!(alice
        .privacy_snapshot()
        .allowed_accounts
        .iter()
        .any(|a| a.id == old));
    assert!(!alice
        .privacy_snapshot()
        .allowed_accounts
        .iter()
        .any(|a| a.id == new));
    assert!(
        alice
            .node
            .privacy_dial(
                (std::net::Ipv4Addr::LOCALHOST, bob.listen_tcp_port()).into(),
                &public
            )
            .await
            .is_err(),
        "background authentication must remain sticky"
    );
    alice
        .send_to_account(&new, b"explicit contact after rekey", None)
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), brx.recv())
            .await
            .unwrap()
            .unwrap()
            .text,
        b"explicit contact after rekey"
    );
    assert!(alice
        .privacy_snapshot()
        .allowed_accounts
        .iter()
        .any(|a| a.id == new));
    assert!(alice
        .privacy_snapshot()
        .allowed_accounts
        .iter()
        .any(|a| a.id == old));
    assert!(alice
        .account_history(&old, 100)
        .iter()
        .all(|e| e.text != b"explicit contact after rekey"));
    assert!(alice
        .account_history(&new, 100)
        .iter()
        .all(|e| e.text != b"before rekey"));
}

#[tokio::test]
async fn private_probe_rounds_do_not_starve_a_live_peer_after_eight_stalled_routes() {
    let dir = tempfile::tempdir().unwrap();
    // Fixed keys guarantee nine routes sort before Bob, without an unbounded
    // search for identities below a randomly generated live fingerprint.
    let mut identities: Vec<_> = (1..=10)
        .map(|i| crate::identity::device::DeviceIdentity::from_secret_bytes([i; 32], [i + 32; 32]))
        .collect();
    identities.sort_by_key(|device| device.public().user_id());
    let bob_identity = identities.pop().unwrap();
    crate::identity::keystore::save(
        &dir.path().join("accounts/bob/identity.keystore"),
        "pw",
        &bob_identity,
    )
    .unwrap();
    let (bob, _) = start(dir.path(), "bob", 0).await;
    let live_id = bob.user_id().to_owned();
    // Explicitly owned SDK Alice has no automatic runtime probe competing
    // with this fixture's two rounds or their cursor assertions.
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let (ctx, _) = tokio::sync::mpsc::unbounded_channel();
    let (ftx, _) = tokio::sync::mpsc::unbounded_channel();
    let alice = Node::open(
        crate::identity::device::DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        tx,
        ctx,
        ftx,
        &dir.path().join("alice-messages.log"),
        &dir.path().join("alice-sent.log"),
        "pw",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    alice
        .configure_privacy(
            dir.path(),
            "pw",
            &alice.signed_announce("Alice", listener.local_addr().unwrap().port()),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let accept = tokio::spawn(alice.clone().run_accept_loop(listener));
    let account = crate::identity::account::Account::generate();
    let mut stalled = Vec::new();
    let mut proofs = Vec::new();
    for device in identities {
        assert!(device.public().user_id() < live_id);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        proofs.push(crate::discovery::Announce::new_with_account(
            &device,
            &account,
            "Stalled",
            listener.local_addr().unwrap().port(),
        ));
        stalled.push(listener);
    }
    proofs.push(bob.node.signed_announce("Live Bob", bob.listen_tcp_port()));
    {
        let mut state = alice.privacy.state.write().unwrap();
        let state = state.as_mut().unwrap();
        for proof in proofs {
            state
                .policy
                .grant(
                    &proof.account_id().unwrap(),
                    &proof.name,
                    super::super::PermissionSource::Manual,
                )
                .unwrap();
            state.proofs.record(&proof).unwrap();
            state
                .routes
                .record(
                    &proof.public(),
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                )
                .unwrap();
        }
    }
    // A legitimate policy operation delays the honest responder beyond the
    // old fixture's 300ms deadline. Fair selection must not require instant auth.
    let delayed_gate = bob.node.privacy.gate.clone();
    let mut delayed_responder = Some(delayed_gate.write().await);
    alice.roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert_eq!(
        alice
            .privacy
            .route_probe_cursor
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    alice
        .probe_private_routes_with_budget(Duration::from_millis(300))
        .await;
    assert_eq!(
        alice
            .privacy
            .route_probe_cursor
            .load(std::sync::atomic::Ordering::Relaxed),
        8
    );
    assert!(alice.roster.lock().unwrap().get(&live_id).is_none());
    {
        let round =
            alice.probe_private_routes_with_budget(super::super::transport::HANDSHAKE_TIMEOUT);
        tokio::pin!(round);
        let release = tokio::time::sleep(Duration::from_millis(400));
        tokio::pin!(release);
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                tokio::select! {
                    _ = &mut release, if delayed_responder.is_some() => drop(delayed_responder.take()),
                    _ = &mut round => break,
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {
                        if alice.roster.lock().unwrap().get(&live_id).is_some() { break; }
                    }
                }
            }
        }).await.expect("live peer did not complete pinned authentication within the production probe deadline");
        // Dropping the pending round cancels the other stalled child probes.
    }
    assert_eq!(
        alice
            .privacy
            .route_probe_cursor
            .load(std::sync::atomic::Ordering::Relaxed),
        16
    );
    assert!(
        alice.roster.lock().unwrap().get(&live_id).is_some(),
        "later live route was starved by the same first eight stalled devices"
    );
    accept.abort();
    drop(stalled);
}

#[tokio::test]
async fn private_file_transfer_requires_manifest_scope_and_background_pull_does_not_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = start(dir.path(), "alice", 0).await;
    let (bob, _) = start(dir.path(), "bob", 0).await;
    let aa = alice.node.signed_announce("Alice", alice.listen_tcp_port());
    let ba = bob.node.signed_announce("Bob", bob.listen_tcp_port());
    for (receiver, announcement) in [(&alice, &ba), (&bob, &aa)] {
        receiver.roster.lock().unwrap().update(
            announcement,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            receiver.user_id(),
        );
    }
    bob.set_allowed(alice.account_id(), true).await.unwrap();
    alice.set_invisible(true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    let path = dir.path().join("sample.txt");
    std::fs::write(&path, b"private attachment bytes").unwrap();
    let file = alice
        .node
        .send_file_dm(bob.user_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            bob.node.pull_pending_files().await;
            if bob.node.read_file(file).is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        bob.node.read_file(file).unwrap(),
        b"private attachment bytes"
    );
    // A different file conversation is not automatically authorized merely
    // because the same allowed peer owns it.
    use crate::eventlog::sync::SyncStore;
    let unrelated = crate::eventlog::ConversationId::new([78; 32]);
    let event = crate::eventlog::Event::new(
        &alice.node.identity,
        unrelated,
        1,
        vec![],
        1,
        0,
        crate::eventlog::EventKind::Message,
        b"unrelated chunk".to_vec(),
    );
    alice.node.log.lock().unwrap().append(event).unwrap();
    assert!(alice
        .node
        .sync_store(&bob.node.identity.public())
        .lock()
        .unwrap()
        .event_ids(&unrelated)
        .is_empty());
    alice.set_allowed(bob.account_id(), false).await.unwrap();
    let snapshot = alice.privacy_snapshot();
    alice.node.pull_pending_files().await;
    assert_eq!(alice.privacy_snapshot(), snapshot);
}
