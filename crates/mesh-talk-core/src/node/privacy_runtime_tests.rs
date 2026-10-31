use super::*;
use crate::eventlog::EventKind;
use crate::{identity::device::DeviceIdentity, node::transport::dial, transport::TransportError};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Mutex,
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
const DEADLINE: Duration = Duration::from_secs(3);

#[tokio::test]
async fn private_probe_refreshes_public_presence_before_its_lease_expires() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice");
    let (alice, _) = node(&path);
    let (bob, _) = node(&dir.path().join("bob"));
    alice
        .configure_privacy(
            &path,
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let proof = bob.signed_announce("Bob", addr.port());
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &proof,
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    {
        let mut state = alice.privacy.state.write().unwrap();
        let state = state.as_mut().unwrap();
        state.proofs.record(&proof).unwrap();
        state.routes.record(&proof.public(), addr.ip()).unwrap();
    }
    let (accepted, mut arrivals) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let bob = bob.clone();
            let accepted = accepted.clone();
            sessions.spawn(async move {
                // Slow but reachable authentication uses part of the lease.
                tokio::time::sleep(Duration::from_secs(6)).await;
                if bob.privacy_accept(socket).await.is_ok() {
                    let _ = accepted.send(());
                }
            });
        }
    });
    let probes = tokio::spawn(alice.run_private_route_probes());
    let result = tokio::time::timeout(Duration::from_secs(13), async {
        arrivals.recv().await.unwrap();
        arrivals.recv().await.unwrap();
    })
    .await;
    probes.abort();
    server.abort();
    assert!(
        result.is_ok(),
        "probe duration plus a fixed post-probe sleep delays the next fresh observation beyond the public lease"
    );
}

#[tokio::test]
async fn private_probe_does_not_multiply_handshakes_for_one_devices_cached_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice");
    let (alice, _) = node(&path);
    let (bob, _) = node(&dir.path().join("bob"));
    alice
        .configure_privacy(
            &path,
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let proof = crate::discovery::Announce::new_with_account(
        &bob.identity,
        &crate::identity::account::Account::generate(),
        "Bob",
        addr.port(),
    );
    {
        let mut state = alice.privacy.state.write().unwrap();
        let state = state.as_mut().unwrap();
        state.proofs.record(&proof).unwrap();
        for octet in 1..=8 {
            state
                .routes
                .record(&proof.public(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, octet)))
                .unwrap();
        }
    }
    alice
        .preferred_routes
        .lock()
        .unwrap()
        .insert(bob.user_id(), addr);
    assert_eq!(alice.cached_routing_peers().len(), 8);
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accepted = count.clone();
    let server = tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            held.push(socket);
            accepted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let probe = alice.probe_private_routes_with_budget(Duration::from_millis(500));
    tokio::pin!(probe);
    tokio::select! {
        _ = &mut probe => panic!("the held handshake must still be pending"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {},
    }
    let independent = alice.sessions.try_outbound();
    assert!(
        independent.is_some(),
        "one device's cached addresses exhausted the outbound budget before other peers could connect"
    );
    drop(independent);
    probe.await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "one device's address hints must share one bounded route race, not occupy all TCP slots"
    );
    server.abort();
}

#[tokio::test]
async fn historical_route_probes_leave_capacity_for_healthy_foreground_sync() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice");
    let (alice, _) = node(&path);
    alice
        .configure_privacy(
            &path,
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut servers = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let identity = DeviceIdentity::generate();
        let account = crate::identity::account::Account::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let proof = crate::discovery::Announce::new_with_account(
            &identity,
            &account,
            "Cached departed device",
            address.port(),
        );
        {
            let mut state = alice.privacy.state.write().unwrap();
            let state = state.as_mut().unwrap();
            state.proofs.record(&proof).unwrap();
            state.routes.record(&proof.public(), address.ip()).unwrap();
        }
        let accepted = accepted.clone();
        servers.spawn(async move {
            let mut held = Vec::new();
            loop {
                held.push(listener.accept().await.unwrap().0);
                accepted.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    assert_eq!(alice.cached_routing_peers().len(), 8);
    let producer = tokio::spawn({
        let alice = alice.clone();
        async move {
            alice
                .probe_private_routes_with_budget(Duration::from_secs(10))
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while accepted.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // These are eight different real identity-pinned TCP attempts, rather than
    // manually occupied permits or duplicates of one admission key.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let stalled_sockets = accepted.load(Ordering::SeqCst);
    let (healthy, _) = node(&dir.path().join("healthy"));
    let expected = healthy.identity.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let responder = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let channel = healthy.privacy_accept(socket).await.unwrap();
        healthy.serve_connection(channel).await;
    });
    let round = tokio::time::timeout(Duration::from_secs(1), async {
        let mut channel = alice
            .privacy_sync_dial(address, &expected)
            .await
            .map_err(|error| error.to_string())?;
        let store = alice.sync_store(&expected);
        super::session::request_round(
            &mut channel,
            &store,
            crate::eventlog::ConversationId::new([231; 32]),
        )
        .await
        .map_err(|error| error.to_string())
    })
    .await;
    producer.abort();
    let _ = producer.await;
    responder.abort();
    let _ = responder.await;
    servers.abort_all();
    while servers.join_next().await.is_some() {}
    assert!(
        round.is_ok_and(|result| result.is_ok()),
        "healthy foreground Noise+sync missed its one-second deadline while historical probes held {stalled_sockets} real stalled TCP sockets"
    );
}

#[tokio::test]
async fn historical_probe_rotation_visits_unlaunched_peers_and_rechecks_privacy_after_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice");
    let (alice, _) = node(&path);
    alice
        .configure_privacy(
            &path,
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let mut servers = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let identity = DeviceIdentity::generate();
        let account = crate::identity::account::Account::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let proof = crate::discovery::Announce::new_with_account(
            &identity,
            &account,
            "Cached departed device",
            address.port(),
        );
        {
            let mut state = alice.privacy.state.write().unwrap();
            let state = state.as_mut().unwrap();
            state.proofs.record(&proof).unwrap();
            state.routes.record(&proof.public(), address.ip()).unwrap();
        }
        let arrivals = arrivals.clone();
        servers.spawn(async move {
            let mut held = Vec::new();
            loop {
                held.push(listener.accept().await.unwrap().0);
                arrivals.lock().unwrap().push(address);
            }
        });
    }
    for _ in 0..4 {
        alice
            .probe_private_routes_with_budget(Duration::from_millis(250))
            .await;
    }
    let visited = arrivals
        .lock()
        .unwrap()
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert_eq!(
        visited, 8,
        "a canceled round must resume at peers it never launched rather than skipping the same cached suffix forever"
    );
    alice.set_invisible(true).await.unwrap();
    let before = arrivals.lock().unwrap().len();
    alice
        .probe_private_routes_with_budget(Duration::from_millis(100))
        .await;
    assert_eq!(
        arrivals.lock().unwrap().len(),
        before,
        "retained routing metadata cannot bypass the current private policy"
    );
    alice.set_invisible(false).await.unwrap();
    alice
        .probe_private_routes_with_budget(Duration::from_millis(100))
        .await;
    assert!(
        arrivals.lock().unwrap().len() > before,
        "a later permitted privacy generation must recover probing"
    );
    let released = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let permits = (0..super::session_pool::MAX_DIRECTION_CONNECTIONS)
                .map(|_| alice.sessions.try_outbound())
                .collect::<Option<Vec<_>>>();
            if let Some(permits) = permits {
                break permits;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("whole-round cancellation must release every background TCP permit");
    drop(released);
    servers.abort_all();
    while servers.join_next().await.is_some() {}
}

#[test]
fn failed_presence_cache_write_is_not_a_successful_empty_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice");
    let (alice, _) = node(&path);
    let (bob, _) = node(&dir.path().join("bob"));
    alice
        .configure_privacy(
            &path,
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let proof = bob.signed_announce("Bob", 2);
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &alice.user_id());
    std::fs::remove_file(path.join("peer-proofs")).unwrap();
    std::fs::create_dir(path.join("peer-proofs")).unwrap();
    assert!(alice.cached_peer_snapshot().is_err());
}

#[tokio::test]
async fn pinned_dial_falls_back_to_second_verified_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let good = listener.local_addr().unwrap();
    let bad = std::net::SocketAddr::new("127.0.0.2".parse().unwrap(), good.port());
    let proof = bob.signed_announce("Bob", good.port());
    {
        let mut roster = alice.roster.lock().unwrap();
        roster.update(&proof, good.ip(), &alice.user_id());
        roster.update_routed(&proof, bad.ip(), &alice.user_id());
    }
    let expected = bob.identity.public();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        bob.privacy_accept(stream).await.unwrap()
    });
    let connected =
        tokio::time::timeout(Duration::from_secs(8), alice.privacy_dial(bad, &expected))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(connected.peer_identity(), &expected);
    server.abort();
}

#[test]
fn authenticated_private_peer_keeps_route_but_clears_online_roster() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    alice
        .configure_privacy(
            &dir.path().join("alice"),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let proof = bob.signed_announce("Bob", 4321);
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, ip, &alice.user_id());
    assert!(
        alice
            .cached_peer_snapshot()
            .unwrap()
            .iter()
            .any(|p| p.public == bob.identity.public())
    );

    alice
        .remember_peer_with_visibility(&bob.identity.public(), Some(&proof), ip, false)
        .unwrap();

    assert!(
        alice
            .cached_peer_snapshot()
            .unwrap()
            .iter()
            .all(|p| p.public != bob.identity.public())
    );
    assert!(
        alice
            .cached_routing_peers()
            .iter()
            .any(|p| p.public == bob.identity.public())
    );
}

#[test]
fn discovery_snapshot_caches_both_verified_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    alice
        .configure_privacy(
            &dir.path().join("alice"),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let proof = bob.signed_announce("Bob", 4321);
    let lan: IpAddr = "192.168.1.20".parse().unwrap();
    let vpn: IpAddr = "100.101.1.20".parse().unwrap();
    {
        let mut roster = alice.roster.lock().unwrap();
        roster.update(&proof, lan, &alice.user_id());
        roster.update_routed(&proof, vpn, &alice.user_id());
    }
    alice.cached_peer_snapshot().unwrap();
    let routes = alice
        .privacy
        .state
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .routes
        .routes();
    assert!(routes.contains(&(bob.identity.public(), lan)));
    assert!(routes.contains(&(bob.identity.public(), vpn)));
}

#[test]
fn channel_member_account_stays_known_without_a_live_peer() {
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let proof = alice.signed_announce("Alice", 1234);
    bob.privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .record(&proof)
        .unwrap();

    assert!(bob.roster.lock().unwrap().peers().is_empty());
    assert_eq!(
        bob.account_id_for_device(&alice.identity.public()),
        Some(alice.account_id())
    );
    let mut mismatched = alice.identity.public();
    mismatched.x25519_pub[0] ^= 1;
    assert_eq!(bob.account_id_for_device(&mismatched), None);
    assert_eq!(
        bob.account_id_for_device(&DeviceIdentity::generate().public()),
        None
    );
}

#[tokio::test]
async fn accepted_privacy_socket_disables_nagle_after_authenticated_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let client_identity = alice.identity.public();
    let expected = bob.identity.public();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let socket = stream.into_std().unwrap();
        let monitor = socket.try_clone().unwrap();
        assert!(!monitor.nodelay().unwrap());
        let stream = tokio::net::TcpStream::from_std(socket).unwrap();
        let mut channel = bob.privacy_accept(stream).await.unwrap();
        assert_eq!(channel.peer_identity(), &client_identity);
        channel.send(b"accepted").await.unwrap();
        monitor.nodelay().unwrap()
    });
    let mut channel = alice.privacy_dial(addr, &expected).await.unwrap();
    assert_eq!(channel.recv().await.unwrap(), b"accepted");
    assert!(server.await.unwrap(), "accepted sockets must disable Nagle");
}

#[tokio::test]
async fn configured_public_node_receives_encrypted_accountless_discovery_dm_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let (bob, mut rx) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", addr.port()),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    bob.roster.lock().unwrap().update(
        &Announce::new(&alice.identity, "Legacy Alice", 1),
        addr.ip(),
        &bob.user_id(),
    );
    alice.roster.lock().unwrap().update(
        &bob.signed_announce("Bob", addr.port()),
        addr.ip(),
        &alice.user_id(),
    );
    let server = tokio::spawn(bob.clone().run_accept_loop(listener));
    alice
        .send_dm(&bob.user_id(), b"legacy encrypted hello")
        .await
        .unwrap();
    assert_eq!(
        rx.try_recv()
            .expect("known signed legacy author must surface")
            .text,
        b"legacy encrypted hello"
    );
    let attachment = dir.path().join("legacy.txt");
    std::fs::write(&attachment, b"legacy sealed file").unwrap();
    let file = alice
        .send_file_dm(&bob.user_id(), &attachment, crate::file::FileKind::File)
        .await
        .unwrap();
    assert!(
        bob.files.lock().unwrap().file_convs().contains(&file),
        "accountless sealed manifest must surface"
    );
    bob.set_invisible(true).await.unwrap();
    assert!(
        bob.historical_author(&alice.identity.public().ed25519_pub)
            .is_none()
    );
    server.abort();
}

#[test]
fn rejected_discovery_proof_does_not_hide_successfully_persisted_peer() {
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    let (invalid, _) = node(&dir.path().join("invalid"));
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    for proof in [
        alice.signed_announce("Alice", 1),
        invalid.signed_announce("Invalid", 0),
    ] {
        bob.roster
            .lock()
            .unwrap()
            .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &bob.user_id());
    }
    let peers = bob
        .cached_peer_snapshot()
        .expect("individual rejected proof must not hide valid contacts");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].public, alice.identity.public());
    let (carol, _) = node(&dir.path().join("carol"));
    for proof in [
        carol.signed_announce("Carol", 1),
        invalid.signed_announce("x".repeat(1025), 1),
        Announce::new_with_account(
            &alice.identity,
            &crate::identity::account::Account::generate(),
            "Conflicting Alice",
            1,
        ),
    ] {
        bob.roster
            .lock()
            .unwrap()
            .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &bob.user_id());
    }
    let peers = bob.cached_peer_snapshot().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].public, carol.identity.public());
    // A legacy downgrade cannot replace a directory-certified author binding.
    bob.roster.lock().unwrap().update(
        &Announce::new(&alice.identity, "Downgrade", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &bob.user_id(),
    );
    assert_eq!(
        bob.historical_author(&alice.identity.public().ed25519_pub)
            .unwrap()
            .account_id(),
        Some(alice.account_id())
    );
    assert!(
        !bob.cached_peer_snapshot()
            .unwrap()
            .iter()
            .any(|p| p.public == alice.identity.public())
    );
}

#[tokio::test]
async fn relay_discovery_cache_does_not_block_async_executor() {
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(dir.path());
    bob.configure_privacy(
        dir.path(),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let control = bob.privacy.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = control.state.write().unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(250));
    });
    ready_rx.recv().unwrap();
    let start = std::time::Instant::now();
    let (_, elapsed) = tokio::join!(bob.drain_from_post_office(), async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        start.elapsed()
    });
    holder.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(150),
        "cache blocked executor for {elapsed:?}"
    );
}

#[test]
fn cached_peer_snapshot_excludes_arrivals_after_its_capture() {
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let captured = bob.discovery_snapshot();
    let proof = alice.signed_announce("Alice", 1);
    bob.roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &bob.user_id());
    let returned = Node::persist_discovery_snapshot(&bob.privacy, captured).unwrap();
    assert!(
        returned.is_empty(),
        "new unpersisted contact leaked into captured query"
    );
    assert!(bob.historical_author(&proof.ed25519_pub).is_none());
    let returned = bob.cached_peer_snapshot().unwrap();
    assert_eq!(returned.len(), 1);
    assert!(bob.historical_author(&proof.ed25519_pub).is_some());
}

#[tokio::test]
async fn cached_private_author_requires_permission_during_real_relay_rounds() {
    let dir = tempfile::tempdir().unwrap();
    let (bob, mut rx) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    let (relay, _) = node(&dir.path().join("relay"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let aa = alice.signed_announce("Alice", 1);
    let ra = Announce::new_post_office_with_account(
        &relay.identity,
        &relay.account,
        "Relay",
        addr.port(),
    );
    for proof in [&aa, &ra] {
        bob.roster
            .lock()
            .unwrap()
            .update(proof, addr.ip(), &bob.user_id());
    }
    bob.cache_discovered_peers().unwrap();
    bob.set_allowed(&relay.account_id(), true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    bob.roster.lock().unwrap().evict_stale(Duration::ZERO);
    let keys = relay.identity.secret_bytes();
    let store = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(
            &dir.path().join("relay.log"),
            "pw",
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        )
        .unwrap(),
    ));
    let server = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        listener,
        store.clone(),
    ));
    let conv =
        super::conversation::dm_conversation_id(&alice.identity.public(), &bob.identity.public());
    let sealed = alice
        .dm_ratchet
        .lock()
        .unwrap()
        .encrypt(
            &alice.identity,
            &bob.identity.public(),
            &MessageBody::new(b"allowed once".to_vec(), None).encode(),
        )
        .unwrap();
    alice
        .append_event(conv, crate::eventlog::EventKind::Message, sealed)
        .unwrap();
    let event = alice.log.lock().unwrap().events(&conv).unwrap()[0].clone();
    store.lock().unwrap().accept(event.clone()).unwrap();
    let mut channel = bob
        .privacy_dial(addr, &relay.identity.public())
        .await
        .unwrap();
    assert!(
        super::session::request_round(
            &mut channel,
            &bob.sync_store(&relay.identity.public()),
            conv
        )
        .await
        .is_err()
    );
    assert!(bob.log.lock().unwrap().events(&conv).unwrap().is_empty());
    assert!(rx.try_recv().is_err());
    bob.set_allowed(&alice.account_id(), true).await.unwrap();
    let mut channel = bob
        .privacy_dial(addr, &relay.identity.public())
        .await
        .unwrap();
    super::session::request_round(
        &mut channel,
        &bob.sync_store(&relay.identity.public()),
        conv,
    )
    .await
    .unwrap();
    bob.emit_new_messages(conv);
    assert_eq!(rx.try_recv().unwrap().text, b"allowed once");
    bob.set_allowed(&alice.account_id(), false).await.unwrap();
    let sealed = alice
        .dm_ratchet
        .lock()
        .unwrap()
        .encrypt(
            &alice.identity,
            &bob.identity.public(),
            &MessageBody::new(b"revoked".to_vec(), None).encode(),
        )
        .unwrap();
    alice
        .append_event(conv, crate::eventlog::EventKind::Message, sealed)
        .unwrap();
    let revoked = alice.log.lock().unwrap().events(&conv).unwrap()[1].clone();
    store.lock().unwrap().accept(revoked.clone()).unwrap();
    let mut channel = bob
        .privacy_dial(addr, &relay.identity.public())
        .await
        .unwrap();
    assert!(
        super::session::request_round(
            &mut channel,
            &bob.sync_store(&relay.identity.public()),
            conv
        )
        .await
        .is_err()
    );
    assert!(!bob.log.lock().unwrap().has(&revoked.id).unwrap());
    assert!(bob.log.lock().unwrap().has(&event.id).unwrap());
    assert!(rx.try_recv().is_err());
    assert!(bob.roster.lock().unwrap().get(&aa.user_id).is_none());
    server.abort();
}

#[tokio::test]
async fn public_relay_delivery_survives_recipient_restart_without_sender_route() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("bob");
    let (bob, _) = node(&base);
    let (alice, _) = node(&dir.path().join("alice"));
    bob.configure_privacy(
        &base,
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let relay = DeviceIdentity::generate();
    let secret = relay.secret_bytes();
    let relay_proof = Announce::new_post_office(&relay, "Relay", addr.port());
    let store = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(&dir.path().join("relay.log"), "pw", relay).unwrap(),
    ));
    let server = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        listener,
        store.clone(),
    ));
    let proof = alice.signed_announce("Alice", 1);
    bob.roster
        .lock()
        .unwrap()
        .update(&proof, addr.ip(), &bob.user_id());
    bob.cache_discovered_peers().unwrap();
    bob.roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert!(bob.roster.lock().unwrap().peers().is_empty());
    alice.roster.lock().unwrap().update(
        &bob.signed_announce("Bob", 1),
        addr.ip(),
        &alice.user_id(),
    );
    alice
        .roster
        .lock()
        .unwrap()
        .update(&relay_proof, addr.ip(), &alice.user_id());
    let conv =
        super::conversation::dm_conversation_id(&alice.identity.public(), &bob.identity.public());
    alice
        .send_dm(&bob.user_id(), b"held across restart")
        .await
        .unwrap();
    let event = alice.log.lock().unwrap().events(&conv).unwrap()[0].clone();
    let mailbox = crate::postoffice::opaque::RelayMailbox::new(
        &alice.identity,
        &bob.identity.public(),
        &relay_proof.public(),
    )
    .unwrap();
    assert!(store.lock().unwrap().has(&mailbox.wrap(&event).unwrap().id));
    assert!(!store.lock().unwrap().has(&event.id));
    let keys = bob.identity.secret_bytes();
    let account = crate::identity::account::Account::from_secret_bytes(bob.account.secret_bytes());
    drop(alice);
    drop(bob);
    // Historical authors must not require even a stale private endpoint hint.
    std::fs::remove_file(base.join("peer-routes")).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let bob = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        account,
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        tx,
        ctx,
        ftx,
        &base.join("messages.log"),
        &base.join("sent.log"),
        "pw",
    )
    .unwrap();
    bob.configure_privacy(
        &base,
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    assert!(bob.roster.lock().unwrap().peers().is_empty());
    bob.roster
        .lock()
        .unwrap()
        .update(&relay_proof, addr.ip(), &bob.user_id());
    bob.replace_verified_dm_pull_peers(vec![proof.user_id.clone()]);
    bob.drain_from_post_office().await;
    let received = rx
        .try_recv()
        .expect("historical original sender must deliver without live route");
    assert_eq!(received.text, b"held across restart");
    assert_eq!(received.from, proof.user_id);
    assert!(bob.privacy_snapshot().allowed_accounts.is_empty());
    assert!(bob.roster.lock().unwrap().get(&proof.user_id).is_none());
    server.abort();
}

#[tokio::test]
async fn historical_proofs_do_not_grant_private_dm_scope_or_online_presence() {
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let proof = alice.signed_announce("Alice", 1);
    bob.roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &bob.user_id());
    bob.cache_discovered_peers().unwrap();
    bob.roster.lock().unwrap().evict_stale(Duration::ZERO);
    bob.set_invisible(true).await.unwrap();
    assert!(bob.historical_author(&proof.ed25519_pub).is_none());
    assert!(bob.historical_dm_peers().is_empty());
    bob.set_allowed(&alice.account_id(), true).await.unwrap();
    assert!(bob.historical_author(&proof.ed25519_pub).is_some());
    bob.set_allowed(&alice.account_id(), false).await.unwrap();
    assert!(bob.historical_author(&proof.ed25519_pub).is_none());
    assert!(
        bob.historical_author(&DeviceIdentity::generate().public().ed25519_pub)
            .is_none()
    );
    let mut tampered = proof.clone();
    tampered.sig[0] ^= 1;
    assert!(
        bob.remember_peer(
            &proof.public(),
            Some(&tampered),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        )
        .is_err()
    );
    assert!(bob.roster.lock().unwrap().peers().is_empty());
}

#[tokio::test]
async fn invisible_account_file_scopes_survive_restart_for_every_destination() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("alice");
    let (alice, _) = node(&base);
    alice
        .configure_privacy(
            &base,
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let account = crate::identity::account::Account::generate();
    let devices = [DeviceIdentity::generate(), DeviceIdentity::generate()];
    for device in &devices {
        alice.roster.lock().unwrap().update(
            &Announce::new_with_account(device, &account, "Bob", 1),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &alice.user_id(),
        );
    }
    alice.set_invisible(true).await.unwrap();
    let path = dir.path().join("attachment.txt");
    std::fs::write(&path, b"resumable private attachment").unwrap();
    let file = alice
        .send_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let expected = alice
        .log
        .lock()
        .unwrap()
        .events(&file)
        .unwrap()
        .iter()
        .map(|event| event.id)
        .collect::<Vec<_>>();
    assert!(!expected.is_empty());
    for device in &devices {
        assert_eq!(
            alice
                .sync_store(&device.public())
                .lock()
                .unwrap()
                .event_ids(&file)
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        alice
            .account_history(&account.account_id(), 100)
            .unwrap()
            .len(),
        1,
        "one UI bubble, not one per destination"
    );
    let keys = alice.identity.secret_bytes();
    let own = crate::identity::account::Account::from_secret_bytes(alice.account.secret_bytes());
    drop(alice);
    // A published migration source is a downgrade barrier, not an active
    // append log. Reject a torn legacy tail, then restore its exact original
    // bytes and verify the indexed scopes survive the ordinary reopen below.
    use std::io::Write;
    let scopes = base.join("sent-manifest-scopes.log");
    let source_len = std::fs::metadata(&scopes).unwrap().len();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&scopes)
        .unwrap()
        .write_all(&[0, 0, 0, 50, 1, 2])
        .unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let rejected = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        crate::identity::account::Account::from_secret_bytes(own.secret_bytes()),
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        tx,
        ctx,
        ftx,
        &base.join("messages.log"),
        &base.join("sent.log"),
        "pw",
    );
    assert!(matches!(
        rejected,
        Err(crate::eventlog::LogError::CorruptFile(_))
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&scopes)
        .unwrap()
        .set_len(source_len)
        .unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let reopened = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        own,
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        tx,
        ctx,
        ftx,
        &base.join("messages.log"),
        &base.join("sent.log"),
        "pw",
    )
    .unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    reopened
        .configure_privacy(
            &base,
            "pw",
            &reopened.signed_announce("Alice", addr.port()),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    for device in &devices {
        assert_eq!(
            reopened
                .sync_store(&device.public())
                .lock()
                .unwrap()
                .event_ids(&file)
                .unwrap(),
            expected,
            "every recipient must retain its own manifest scope after sender restart"
        );
    }
    assert_eq!(
        reopened
            .account_history(&account.account_id(), 100)
            .unwrap()
            .len(),
        1
    );
    let server = tokio::spawn(reopened.clone().run_accept_loop(listener));
    for device in &devices {
        let store = Mutex::new(crate::eventlog::EventLog::default());
        tokio::time::timeout(DEADLINE, async {
            let mut channel = dial(addr, device, Some(&reopened.identity.public()))
                .await
                .unwrap();
            crate::node::session::request_round(&mut channel, &store, file)
                .await
                .unwrap();
        })
        .await
        .expect("restarted sender must actually serve chunks to every device");
        assert_eq!(store.lock().unwrap().event_ids(&file).unwrap(), expected);
    }
    let second = reopened
        .send_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    // Live scope references use the bounded encrypted index. The authenticated
    // legacy barrier stays fixed and must never resume append-log writes.
    assert_eq!(std::fs::metadata(&scopes).unwrap().len(), source_len);
    crate::node::filebook::scopes::IndexedManifestScopes::authenticate(
        &reopened.storage,
        &scopes,
        "pw",
    )
    .unwrap();
    {
        let delivery = reopened.delivery.lock().unwrap();
        for file_conversation in [file, second] {
            let card = delivery
                .file_cards()
                .find(|card| card.file_conversation == file_conversation)
                .unwrap();
            assert_eq!(card.destinations.len(), 2);
            let indexed_scopes = reopened
                .manifest_scopes
                .scopes(file_conversation)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(indexed_scopes.len(), card.destinations.len());
            for scope in indexed_scopes {
                assert!(
                    card.destinations
                        .iter()
                        .any(|destination| destination.binding.event_id == scope.event)
                );
                let original = reopened
                    .log
                    .lock()
                    .unwrap()
                    .get(&scope.event)
                    .unwrap()
                    .unwrap();
                assert!(original.verify_signature() && original.verify_integrity());
                assert_eq!(scope.parent, original.conversation_id);
                assert_eq!(scope.author, original.author);
            }
            for device in &devices {
                let destination = card
                    .destinations
                    .iter()
                    .find(|d| d.binding.device == device.public())
                    .unwrap();
                assert_eq!(
                    destination.binding.account.as_deref(),
                    Some(account.account_id().as_str())
                );
                let log = reopened.log.lock().unwrap();
                let manifest = log.get(&destination.binding.event_id).unwrap().unwrap();
                assert_eq!(manifest.kind, crate::eventlog::EventKind::FileManifest);
                assert_eq!(
                    manifest.conversation_id,
                    crate::node::conversation::dm_conversation_id(
                        &reopened.identity.public(),
                        &device.public()
                    )
                );
                assert!(manifest.verify_signature() && manifest.verify_integrity());
                assert_eq!(
                    card.final_chunk,
                    log.events(&file_conversation).unwrap().last().map(|e| e.id)
                );
                for (index, chunk) in log.events(&file_conversation).unwrap().iter().enumerate() {
                    assert_eq!(chunk.seq, index as u64 + 1);
                    assert_eq!(chunk.parents.len(), usize::from(index > 0));
                }
            }
        }
    }
    reopened
        .set_allowed(&account.account_id(), false)
        .await
        .unwrap();
    for device in &devices {
        assert!(
            reopened
                .sync_store(&device.public())
                .lock()
                .unwrap()
                .event_ids(&file)
                .unwrap()
                .is_empty(),
            "durable file scope must not bypass account revocation"
        );
    }
    assert_eq!(
        reopened
            .account_history(&account.account_id(), 100)
            .unwrap()
            .len(),
        2
    );
    server.abort();
}

#[test]
fn sent_scope_repair_failure_installs_no_permission_and_retry_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(dir.path());
    alice
        .configure_privacy(
            dir.path(),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let path = dir.path().join("sent-manifest-scopes.log");
    let backup = dir.path().join("scopes-backup");
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();
    let file = crate::eventlog::ConversationId::new([31; 32]);
    let parent = crate::eventlog::ConversationId::new([32; 32]);
    let author = crate::eventlog::Author::from_ed25519(alice.identity.public().ed25519_pub);
    let event = crate::eventlog::EventId::new([33; 32]);
    assert!(
        alice
            .remember_sent_manifest_scope(file, parent, author, event)
            .is_err()
    );
    assert!(!alice.has_file_scope(file).unwrap());
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
    alice
        .remember_sent_manifest_scope(file, parent, author, event)
        .unwrap();
    alice
        .remember_sent_manifest_scope(file, parent, author, event)
        .unwrap();
    let count: i64 = alice
        .storage
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM sent_file_scopes WHERE file=?1",
                [file.as_bytes().as_slice()],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        count, 1,
        "retry preserves exact idempotent sent scope metadata"
    );
    assert!(
        !alice.has_file_scope(file).unwrap(),
        "orphan metadata cannot authorize a file without a validated binding"
    );
}

#[tokio::test]
async fn legacy_signed_manifest_fanout_scopes_import_after_torn_tail_and_accept_later_append() {
    use crate::eventlog::sync::SyncStore;
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(dir.path());
    alice
        .configure_privacy(
            dir.path(),
            "pw",
            &alice.signed_announce("Alice", 9),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    alice.set_invisible(true).await.unwrap();
    let account = crate::identity::account::Account::generate();
    let devices = [
        DeviceIdentity::generate(),
        DeviceIdentity::generate(),
        DeviceIdentity::generate(),
    ];
    for device in &devices {
        let proof = Announce::new_with_account(device, &account, "Bob", 9);
        alice
            .privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record(&proof)
            .unwrap();
    }
    alice
        .initiate_contact_locally(&account.account_id())
        .await
        .unwrap();
    let path = dir.path().join("legacy.txt");
    std::fs::write(&path, b"legacy scope fixture").unwrap();
    let (manifest, file) = alice
        .stage_file(&path, crate::file::FileKind::File, |_| {})
        .unwrap();
    let author = crate::eventlog::Author::from_ed25519(alice.identity.public().ed25519_pub);
    let mut originals = Vec::new();
    for device in &devices[..2] {
        let parent = crate::node::conversation::dm_conversation_id(
            &alice.identity.public(),
            &device.public(),
        );
        alice
            .append_event(
                parent,
                crate::eventlog::EventKind::FileManifest,
                crate::dm::seal(
                    &alice.identity,
                    &device.public().x25519_pub,
                    &manifest.encode(),
                )
                .unwrap(),
            )
            .unwrap();
        let event = alice.log.lock().unwrap().events(&parent).unwrap()[0].id;
        alice
            .remember_sent_manifest_scope(file, parent, author, event)
            .unwrap();
        originals.push(event);
    }
    let original_wall_clock = alice
        .log
        .lock()
        .unwrap()
        .get(&originals[0])
        .unwrap()
        .unwrap()
        .wall_clock;
    let original = alice
        .log
        .lock()
        .unwrap()
        .get(&originals[0])
        .unwrap()
        .unwrap();
    alice
        .received_files
        .lock()
        .unwrap()
        .record_file_with_reception(
            &super::received_log::ReceivedEntry {
                event_id: original.id,
                conversation: crate::node::conversation::account_conversation_id(
                    &alice.account_id(),
                    &account.account_id(),
                ),
                from: alice.user_id(),
                wall_clock: original_wall_clock,
                plaintext: manifest.encode(),
            },
            Some(&original),
            None,
        )
        .unwrap();
    let old_scopes = originals
        .iter()
        .map(|id| {
            let event = alice.log.lock().unwrap().get(id).unwrap().unwrap();
            StoredManifestScope {
                file,
                parent: event.conversation_id,
                author: event.author,
                event: *id,
            }
        })
        .collect::<Vec<_>>();
    alice
        .storage
        .database()
        .transaction(|c| {
            c.execute("DELETE FROM transcript_sources WHERE kind=9", [])?;
            c.execute_batch("DROP TABLE sent_file_scopes;DROP TABLE sent_file_scope_revision")?;
            Ok(())
        })
        .unwrap();
    let expected = alice.log.lock().unwrap().event_ids(&file).unwrap();
    let keys = alice.identity.secret_bytes();
    let own = crate::identity::account::Account::from_secret_bytes(alice.account.secret_bytes());
    drop(alice);
    let scope_path = dir.path().join("sent-manifest-scopes.log");
    std::fs::remove_file(&scope_path).unwrap();
    let (mut old_log, _) =
        crate::storage::record_log::EncryptedRecordLog::<StoredManifestScope>::open(
            &scope_path,
            "pw",
            b"MTFSC1",
        )
        .unwrap();
    for scope in old_scopes {
        old_log.append_durable(&scope).unwrap();
    }
    drop(old_log);

    std::fs::OpenOptions::new()
        .append(true)
        .open(&scope_path)
        .unwrap()
        .write_all(&[0, 0, 0, 50, 1, 2])
        .unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let alice = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        own,
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        tx,
        ctx,
        ftx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    alice
        .configure_privacy(
            dir.path(),
            "pw",
            &alice.signed_announce("Alice", addr.port()),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    for device in &devices[..2] {
        assert_eq!(
            alice
                .sync_store(&device.public())
                .lock()
                .unwrap()
                .event_ids(&file)
                .unwrap(),
            expected
        );
    }
    let parent = crate::node::conversation::dm_conversation_id(
        &alice.identity.public(),
        &devices[2].public(),
    );
    alice
        .append_event(
            parent,
            crate::eventlog::EventKind::FileManifest,
            crate::dm::seal(
                &alice.identity,
                &devices[2].public().x25519_pub,
                &manifest.encode(),
            )
            .unwrap(),
        )
        .unwrap();
    let event = alice.log.lock().unwrap().events(&parent).unwrap()[0].id;
    alice
        .remember_sent_manifest_scope(file, parent, author, event)
        .unwrap();
    let count: i64 = alice
        .storage
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM sent_file_scopes WHERE file=?1",
                [file.as_bytes().as_slice()],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 3);
    let server = tokio::spawn(alice.clone().run_accept_loop(listener));
    for device in &devices {
        let store = Mutex::new(crate::eventlog::EventLog::default());
        tokio::time::timeout(DEADLINE, async {
            let mut channel = dial(addr, device, Some(&alice.identity.public()))
                .await
                .unwrap();
            crate::node::session::request_round(&mut channel, &store, file)
                .await
                .unwrap();
        })
        .await
        .unwrap();
        assert_eq!(store.lock().unwrap().event_ids(&file).unwrap(), expected);
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn ipv4_mapped_authenticated_route_is_canonical_and_can_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    for (node, name, port) in [(&alice, "Alice", 1), (&bob, "Bob", port)] {
        node.configure_privacy(
            &dir.path().join(name.to_lowercase()),
            "pw",
            &node.signed_announce(name, port),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    let proof = bob.signed_announce("Bob", port);
    alice
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .policy
        .grant(&bob.account_id(), "Bob", PermissionSource::Manual)
        .unwrap();
    let mapped = IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped());
    // Exact source representation returned by a dual-stack listener accepting
    // an IPv4 peer. Presence must not turn a usable IPv4 route into a v6 dial.
    alice
        .remember_peer(&bob.identity.public(), Some(&proof), mapped)
        .unwrap();
    assert_eq!(
        alice
            .roster
            .lock()
            .unwrap()
            .get(&bob.user_id())
            .unwrap()
            .addr
            .ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
    assert_eq!(
        alice.cached_routing_peers()[0].addr.ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
    let expected = bob.identity.public();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = bob.privacy_accept(stream).await.unwrap();
        channel.send(b"authenticated reply").await.unwrap();
    });
    // Also accept mapped hints from previously persisted routes. Only the IP
    // representation changes; the full Noise peer identity remains pinned.
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut channel = alice
            .privacy_dial((mapped, port).into(), &expected)
            .await
            .unwrap();
        assert_eq!(channel.recv().await.unwrap(), b"authenticated reply");
        server.await.unwrap();
    })
    .await
    .expect("mapped route did not complete an authenticated reply");
}

#[tokio::test]
async fn explicit_rehome_invalidates_channels_even_when_allowlist_snapshot_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let (carol, _) = node(&dir.path().join("carol"));
    alice
        .configure_privacy(
            &dir.path().join("alice"),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    for node in [&bob, &carol] {
        alice.roster.lock().unwrap().update(
            &node.signed_announce("Same", 1),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &alice.user_id(),
        );
        alice.set_allowed(&node.account_id(), true).await.unwrap();
    }
    let before = alice.privacy_snapshot();
    let generation = alice.privacy.generation.load(Ordering::SeqCst);
    let proof = Announce::new_with_account(&bob.identity, &carol.account, "Same", 1);
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &alice.user_id());
    alice.set_allowed(&carol.account_id(), true).await.unwrap();
    assert_eq!(alice.privacy_snapshot(), before);
    assert!(alice.privacy.generation.load(Ordering::SeqCst) > generation);
}

#[tokio::test]
async fn manual_presence_publication_has_one_deadline_for_multiple_stalled_devices() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(dir.path());
    alice
        .configure_privacy(
            dir.path(),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let account = crate::identity::account::Account::generate();
    let mut listeners = Vec::new();
    for _ in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(
            &device,
            &account,
            "Stalled",
            listener.local_addr().unwrap().port(),
        );
        alice.roster.lock().unwrap().update(
            &proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &alice.user_id(),
        );
        listeners.push(listener);
    }
    tokio::time::timeout(
        // The publication has one 10s handshake deadline. Leave room for
        // scheduling on loaded Windows runners while still rejecting three
        // sequential 10s handshakes.
        Duration::from_secs(20),
        alice.set_allowed(&account.account_id(), true),
    )
    .await
    .expect("publication multiplied the per-device deadline")
    .unwrap();
    assert!(
        alice
            .privacy_snapshot()
            .allowed_accounts
            .iter()
            .any(|a| a.id == account.account_id())
    );
    tokio::time::timeout(DEADLINE, alice.set_allowed(&account.account_id(), false))
        .await
        .unwrap()
        .unwrap();
    drop(listeners);
}

#[tokio::test]
async fn allowed_carrier_cannot_create_a_group_owned_by_an_unknown_author() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    let unknown = DeviceIdentity::generate();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    bob.roster.lock().unwrap().update(
        &alice.signed_announce("Alice", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &bob.user_id(),
    );
    bob.set_allowed(&alice.account_id(), true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    let conv = crate::eventlog::ConversationId::new([79; 32]);
    let meta = crate::channel::ChannelMeta {
        name: "Unapproved invitation".into(),
        members: vec![unknown.public(), bob.identity.public()],
        epoch: 0,
        owner: unknown.public().user_id(),
    };
    let genesis = crate::eventlog::Event::new(
        &unknown,
        conv,
        1,
        vec![],
        1,
        0,
        crate::eventlog::EventKind::MembershipChange,
        meta.encode(),
    );
    assert!(
        bob.sync_store(&alice.identity.public())
            .lock()
            .unwrap()
            .ingest(genesis)
            .is_err()
    );
    assert!(bob.log.lock().unwrap().events(&conv).unwrap().is_empty());
}

#[tokio::test]
async fn manually_allowed_invisible_device_can_receive_later_channel_invitation() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let (carol, _) = node(&dir.path().join("carol"));
    carol
        .configure_privacy(
            &dir.path().join("carol"),
            "pw",
            &carol.signed_announce("Carol", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    carol.roster.lock().unwrap().update(
        &alice.signed_announce("Alice", 1234),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &carol.user_id(),
    );
    carol.set_allowed(&alice.account_id(), true).await.unwrap();
    carol.set_invisible(true).await.unwrap();
    let channel = alice
        .create_channel_invited("Team", vec![bob.identity.public()])
        .await
        .unwrap();
    alice
        .add_channel_member(channel, carol.identity.public())
        .await
        .unwrap();
    let events: Vec<_> = alice
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .collect();
    let store = carol.sync_store(&alice.identity.public());
    for event in events {
        store
            .lock()
            .unwrap()
            .ingest(event)
            .expect("approved inviter's channel ancestry syncs");
    }
    carol.process_channel(channel);
    assert!(
        carol
            .list_channels()
            .unwrap()
            .iter()
            .any(|summary| summary.id == channel && summary.pending_invite)
    );
}

#[tokio::test]
async fn invisible_late_invitation_can_arrive_through_an_allowed_post_office() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (relay, _) = node(&dir.path().join("relay"));
    let (carol, _) = node(&dir.path().join("carol"));
    carol
        .configure_privacy(
            &dir.path().join("carol"),
            "pw",
            &carol.signed_announce("Carol", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let relay_proof =
        Announce::new_post_office_with_account(&relay.identity, &relay.account, "Relay", 1234);
    for (peer, proof) in [
        (&alice, alice.signed_announce("Alice", 1234)),
        (&relay, relay_proof),
    ] {
        carol.roster.lock().unwrap().update(
            &proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &carol.user_id(),
        );
        carol.set_allowed(&peer.account_id(), true).await.unwrap();
    }
    carol.set_invisible(true).await.unwrap();
    let group = alice.create_channel_invited("Team", vec![]).await.unwrap();
    alice
        .add_channel_member(group, carol.identity.public())
        .await
        .unwrap();
    let events: Vec<_> = alice
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .collect();
    let store = carol.sync_store(&relay.identity.public());
    for event in events {
        store.lock().unwrap().ingest(event).unwrap();
    }
    crate::node::test_support::settle_group_metadata(&carol, group);
    assert!(
        carol
            .list_channels()
            .unwrap()
            .iter()
            .any(|summary| summary.id == group && summary.pending_invite)
    );
}

#[tokio::test]
async fn manually_allowed_member_cannot_keep_syncing_after_recipient_is_removed() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let (carol, _) = node(&dir.path().join("carol"));
    carol
        .configure_privacy(
            &dir.path().join("carol"),
            "pw",
            &carol.signed_announce("Carol", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    for peer in [&alice, &bob] {
        carol.roster.lock().unwrap().update(
            &peer.signed_announce("Peer", 1234),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &carol.user_id(),
        );
        carol.set_allowed(&peer.account_id(), true).await.unwrap();
    }
    carol.set_invisible(true).await.unwrap();
    let group = alice
        .create_channel("Team", vec![bob.identity.public(), carol.identity.public()])
        .await
        .unwrap();
    let genesis = alice.log.lock().unwrap().events(&group).unwrap()[0].clone();
    let removal = crate::eventlog::Event::new(
        &alice.identity,
        group,
        2,
        vec![genesis.id],
        2,
        1,
        EventKind::MembershipChange,
        crate::channel::ChannelMeta {
            name: "Team".into(),
            members: vec![alice.identity.public(), bob.identity.public()],
            epoch: 1,
            owner: alice.user_id(),
        }
        .encode(),
    );
    let later = crate::eventlog::Event::new(
        &bob.identity,
        group,
        1,
        vec![removal.id],
        3,
        1,
        EventKind::Message,
        vec![1, 2, 3],
    );
    let store = carol.sync_store(&bob.identity.public());
    store.lock().unwrap().ingest(genesis).unwrap();
    store.lock().unwrap().ingest(removal).unwrap();
    assert!(store.lock().unwrap().ingest(later.clone()).is_err());
    assert!(!carol.log.lock().unwrap().has(&later.id).unwrap());
}

#[tokio::test]
async fn invisible_late_invitee_follows_an_accepted_owner_transfer() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let (carol, _) = node(&dir.path().join("carol"));
    carol
        .configure_privacy(
            &dir.path().join("carol"),
            "pw",
            &carol.signed_announce("Carol", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    for peer in [&alice, &bob] {
        carol.roster.lock().unwrap().update(
            &peer.signed_announce("Peer", 1234),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &carol.user_id(),
        );
        carol.set_allowed(&peer.account_id(), true).await.unwrap();
    }
    carol.set_invisible(true).await.unwrap();
    let channel = alice
        .create_channel_invited("Team", vec![bob.identity.public()])
        .await
        .unwrap();
    let (root, lamport) = {
        let log = alice.log.lock().unwrap();
        let events = log.events(&channel).unwrap();
        let last = events.last().unwrap();
        (last.id, last.lamport)
    };
    let accepted = crate::eventlog::Event::new(
        &bob.identity,
        channel,
        1,
        vec![root],
        lamport + 1,
        1,
        EventKind::ChannelAccept,
        vec![],
    );
    alice.log.lock().unwrap().append(accepted).unwrap();
    alice
        .append_event(
            channel,
            EventKind::ChannelOwnerTransfer,
            bob.user_id().into_bytes(),
        )
        .unwrap();
    let (transfer, lamport) = {
        let log = alice.log.lock().unwrap();
        let events = log.events(&channel).unwrap();
        let last = events.last().unwrap();
        (last.id, last.lamport)
    };
    let invite = crate::eventlog::Event::new(
        &bob.identity,
        channel,
        2,
        vec![transfer],
        lamport + 1,
        1,
        EventKind::MembershipChange,
        crate::channel::ChannelMeta {
            name: "Team".into(),
            members: vec![
                alice.identity.public(),
                bob.identity.public(),
                carol.identity.public(),
            ],
            epoch: 1,
            owner: bob.user_id(),
        }
        .encode_invitation(),
    );
    alice.log.lock().unwrap().append(invite).unwrap();
    let mut events: Vec<_> = alice
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .collect();
    let invitation = events.pop().unwrap();
    assert_eq!(invitation.kind, EventKind::MembershipChange);
    let store = carol.sync_store(&bob.identity.public());
    for event in events {
        let kind = event.kind;
        let seq = event.seq;
        store
            .lock()
            .unwrap()
            .ingest(event)
            .unwrap_or_else(|error| panic!("{kind:?} seq {seq} ancestry sync failed: {error}"));
    }
    assert!(
        carol
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .is_empty(),
        "channel ancestry must stay ephemeral until a valid invitation arrives"
    );
    carol.process_channel(channel);
    assert!(carol.list_channels().unwrap().is_empty());
    store.lock().unwrap().ingest(invitation).unwrap();
    assert!(
        !carol
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .is_empty()
    );
    carol.process_channel(channel);
    assert!(
        carol
            .list_channels()
            .unwrap()
            .iter()
            .any(|summary| summary.id == channel && summary.pending_invite)
    );
}

#[tokio::test]
async fn invisible_sync_discards_uninvited_history_and_unowned_invitation() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let (carol, _) = node(&dir.path().join("carol"));
    carol
        .configure_privacy(
            &dir.path().join("carol"),
            "pw",
            &carol.signed_announce("Carol", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    for peer in [&alice, &bob] {
        carol.roster.lock().unwrap().update(
            &peer.signed_announce("Peer", 1234),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &carol.user_id(),
        );
        carol.set_allowed(&peer.account_id(), true).await.unwrap();
    }
    carol.set_invisible(true).await.unwrap();
    let channel = alice
        .create_channel_invited("Unrelated", vec![bob.identity.public()])
        .await
        .unwrap();
    let genesis = alice.log.lock().unwrap().events(&channel).unwrap()[0].clone();
    let message = crate::eventlog::Event::new(
        &alice.identity,
        channel,
        2,
        vec![genesis.id],
        2,
        1,
        EventKind::Message,
        vec![1, 2, 3],
    );
    let forged_invite = crate::eventlog::Event::new(
        &bob.identity,
        channel,
        1,
        vec![message.id],
        3,
        1,
        EventKind::MembershipChange,
        crate::channel::ChannelMeta {
            name: "Unrelated".into(),
            members: vec![
                alice.identity.public(),
                bob.identity.public(),
                carol.identity.public(),
            ],
            epoch: 1,
            owner: bob.user_id(),
        }
        .encode_invitation(),
    );
    let store = carol.sync_store(&bob.identity.public());
    for event in [genesis.clone(), message, forged_invite] {
        store.lock().unwrap().ingest(event).unwrap();
    }
    assert_eq!(store.lock().unwrap().event_ids(&channel).unwrap().len(), 3);
    assert!(
        !store
            .lock()
            .unwrap()
            .durable_have(&channel, &genesis.id)
            .unwrap()
    );
    assert!(
        carol
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .is_empty()
    );
    drop(store);
    assert!(
        carol
            .sync_store(&bob.identity.public())
            .lock()
            .unwrap()
            .event_ids(&channel)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn projected_group_removal_takes_effect_before_channel_book_refresh() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    let group = bob
        .create_channel("Group", vec![alice.identity.public()])
        .await
        .unwrap();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    bob.set_invisible(true).await.unwrap();
    let last = bob
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .last()
        .unwrap()
        .id;
    let meta = crate::channel::ChannelMeta {
        name: "Group".into(),
        members: vec![bob.identity.public()],
        epoch: 1,
        owner: bob.user_id(),
    };
    let removal = crate::eventlog::Event::new(
        &bob.identity,
        group,
        100,
        vec![last],
        100,
        1,
        crate::eventlog::EventKind::MembershipChange,
        meta.encode(),
    );
    bob.log.lock().unwrap().append(removal).unwrap();
    assert!(
        !bob.group_member(&alice.identity.public()).unwrap(),
        "stale channel book must not disclose signed listening presence to a removed member"
    );
    assert!(
        bob.sync_store(&alice.identity.public())
            .lock()
            .unwrap()
            .event_ids(&group)
            .unwrap()
            .is_empty()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(bob.clone().run_accept_loop(listener));
    assert!(
        tokio::time::timeout(
            DEADLINE,
            dial(addr, &alice.identity, Some(&bob.identity.public()))
        )
        .await
        .unwrap()
        .is_err(),
        "removed member passed pre-auth group admission through stale book"
    );
    server.abort();
}

#[tokio::test]
async fn invisible_sync_stops_projecting_events_after_channel_dissolution() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (owner, _) = node(&dir.path().join("owner"));
    let (member, _) = node(&dir.path().join("member"));
    let group = owner
        .create_channel("Group", vec![member.identity.public()])
        .await
        .unwrap();
    owner
        .configure_privacy(
            &dir.path().join("owner"),
            "pw",
            &owner.signed_announce("Owner", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    owner.set_invisible(true).await.unwrap();

    let last = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .last()
        .unwrap()
        .id;
    let terminal = crate::eventlog::Event::new(
        &owner.identity,
        group,
        100,
        vec![last],
        100,
        1,
        crate::eventlog::EventKind::ChannelDissolve,
        Vec::new(),
    );
    let late = crate::eventlog::Event::new(
        &owner.identity,
        group,
        101,
        vec![terminal.id],
        101,
        2,
        crate::eventlog::EventKind::ChannelRename,
        b"too late".to_vec(),
    );
    {
        let mut log = owner.log.lock().unwrap();
        log.append(terminal.clone()).unwrap();
        log.append(late.clone()).unwrap();
    }

    let visible = owner
        .sync_store(&member.identity.public())
        .lock()
        .unwrap()
        .event_ids(&group)
        .unwrap();
    assert!(visible.contains(&terminal.id));
    assert!(!visible.contains(&late.id));
}

#[tokio::test]
async fn concurrent_channel_snapshot_does_not_restore_departed_peer_sync() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let carol = DeviceIdentity::generate();
    let group = alice
        .create_channel("Group", vec![bob.identity.public()])
        .await
        .unwrap();
    alice
        .configure_privacy(
            &dir.path().join("alice"),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    alice.roster.lock().unwrap().update(
        &bob.signed_announce("Bob", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &alice.user_id(),
    );
    alice.set_allowed(&bob.account_id(), true).await.unwrap();
    alice.set_invisible(true).await.unwrap();
    assert!(
        !alice
            .sync_store(&bob.identity.public())
            .lock()
            .unwrap()
            .event_ids(&group)
            .unwrap()
            .is_empty(),
        "the peer must be able to sync before leaving"
    );
    let (before_id, before_lamport) = {
        let log = alice.log.lock().unwrap();
        let events = log.events(&group).unwrap();
        let before = events.last().unwrap();
        (before.id, before.lamport)
    };
    let snapshot = crate::eventlog::Event::new(
        &alice.identity,
        group,
        3,
        vec![before_id],
        before_lamport + 2,
        1,
        EventKind::MembershipChange,
        crate::channel::ChannelMeta {
            name: "Group".into(),
            members: vec![
                alice.identity.public(),
                bob.identity.public(),
                carol.public(),
            ],
            epoch: 1,
            owner: alice.user_id(),
        }
        .encode_invitation(),
    );
    let snapshot_id = snapshot.id;
    alice.log.lock().unwrap().append(snapshot).unwrap();
    let leave = crate::eventlog::Event::new(
        &bob.identity,
        group,
        1,
        vec![before_id],
        before_lamport + 1,
        1,
        EventKind::ChannelLeave,
        vec![],
    );
    let leave_id = leave.id;
    alice.log.lock().unwrap().append(leave).unwrap();
    assert!(
        !alice.group_member(&bob.identity.public()).unwrap(),
        "admission must honor the signed leave despite the stale owner snapshot"
    );
    alice
        .append_event(group, EventKind::ChannelRename, b"After leave".to_vec())
        .unwrap();
    let after_leave = alice
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .last()
        .unwrap()
        .id;
    let visible = alice
        .sync_store(&bob.identity.public())
        .lock()
        .unwrap()
        .event_ids(&group)
        .unwrap();
    assert!(visible.contains(&snapshot_id));
    assert!(visible.contains(&leave_id));
    assert!(
        !visible.contains(&after_leave),
        "a stale owner snapshot must not resume departed peer sync"
    );
}

#[tokio::test]
async fn departed_peer_can_sync_transfer_needed_for_causal_reinvitation() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let carol = DeviceIdentity::generate();
    let group = alice
        .create_channel("Group", vec![bob.identity.public(), carol.public()])
        .await
        .unwrap();
    alice
        .configure_privacy(
            &dir.path().join("alice"),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    alice.roster.lock().unwrap().update(
        &bob.signed_announce("Bob", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &alice.user_id(),
    );
    alice.set_allowed(&bob.account_id(), true).await.unwrap();
    alice.set_invisible(true).await.unwrap();
    let (parent, lamport) = {
        let log = alice.log.lock().unwrap();
        let events = log.events(&group).unwrap();
        let last = events.last().unwrap();
        (last.id, last.lamport)
    };
    let leave = crate::eventlog::Event::new(
        &bob.identity,
        group,
        1,
        vec![parent],
        lamport + 1,
        1,
        EventKind::ChannelLeave,
        vec![],
    );
    alice.log.lock().unwrap().append(leave).unwrap();
    alice
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            carol.public().user_id().into_bytes(),
        )
        .unwrap();
    let (transfer_id, transfer_lamport) = {
        let log = alice.log.lock().unwrap();
        let events = log.events(&group).unwrap();
        let last = events.last().unwrap();
        (last.id, last.lamport)
    };
    let invite = crate::eventlog::Event::new(
        &carol,
        group,
        1,
        vec![transfer_id],
        transfer_lamport + 1,
        1,
        EventKind::MembershipChange,
        crate::channel::ChannelMeta {
            name: "Group".into(),
            members: vec![
                alice.identity.public(),
                bob.identity.public(),
                carol.public(),
            ],
            epoch: 2,
            owner: carol.public().user_id(),
        }
        .encode_invitation(),
    );
    let invite_id = invite.id;
    alice.log.lock().unwrap().append(invite).unwrap();
    assert!(
        alice
            .sync_store(&bob.identity.public())
            .lock()
            .unwrap()
            .event_ids(&group)
            .unwrap()
            .contains(&invite_id),
        "the departed peer needs the valid transfer to verify a later invitation"
    );
}

#[tokio::test]
async fn manually_trusted_relay_only_delivers_cached_verified_dm_authors() {
    use crate::eventlog::sync::SyncStore;
    let dir = tempfile::tempdir().unwrap();
    let (bob, _) = node(&dir.path().join("bob"));
    let (alice, _) = node(&dir.path().join("alice"));
    let (relay, _) = node(&dir.path().join("relay"));
    let stranger = DeviceIdentity::generate();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let aa = alice.signed_announce("Alice", 1);
    let ra = crate::discovery::Announce::new_post_office_with_account(
        &relay.identity,
        &relay.account,
        "Relay",
        1,
    );
    for proof in [&aa, &ra] {
        bob.roster
            .lock()
            .unwrap()
            .update(proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &bob.user_id());
    }
    bob.set_allowed(&alice.account_id(), true).await.unwrap();
    bob.set_allowed(&relay.account_id(), true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    let conv =
        super::conversation::dm_conversation_id(&alice.identity.public(), &bob.identity.public());
    let allowed = crate::eventlog::Event::new(
        &alice.identity,
        conv,
        1,
        vec![],
        1,
        0,
        crate::eventlog::EventKind::Message,
        b"allowed offline author".to_vec(),
    );
    let store = bob.sync_store(&relay.identity.public());
    assert!(store.lock().unwrap().ingest(allowed.clone()).is_ok());
    assert_eq!(
        store.lock().unwrap().event_ids(&conv).unwrap(),
        vec![allowed.id]
    );
    let denied = crate::eventlog::Event::new(
        &stranger,
        conv,
        2,
        vec![allowed.id],
        2,
        0,
        crate::eventlog::EventKind::Message,
        b"unknown relayed device".to_vec(),
    );
    assert!(store.lock().unwrap().ingest(denied).is_err());
    assert_eq!(bob.log.lock().unwrap().events(&conv).unwrap().len(), 1);
    bob.set_allowed(&alice.account_id(), false).await.unwrap();
    assert!(store.lock().unwrap().event_ids(&conv).unwrap().is_empty());
    assert_eq!(
        bob.log.lock().unwrap().events(&conv).unwrap().len(),
        1,
        "revocation must not delete history"
    );
}

#[tokio::test]
async fn sdk_reopening_private_policy_suppresses_initial_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let (first, _) = node(dir.path());
    first
        .configure_privacy(
            dir.path(),
            "pw",
            &first.signed_announce("First", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    first.set_invisible(true).await.unwrap();
    let keys = first.identity.secret_bytes();
    let account =
        crate::identity::account::Account::from_secret_bytes(first.account.secret_bytes());
    drop(first);
    let (tx, _) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let reopened = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        account,
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        tx,
        ctx,
        ftx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let visibility = Arc::new(DiscoveryVisibility::new(true));
    reopened
        .configure_privacy(
            dir.path(),
            "pw",
            &reopened.signed_announce("Reopened", 1234),
            visibility.clone(),
        )
        .unwrap();
    let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert!(
        !visibility
            .send_announce(&sender, b"presence", receiver.local_addr().unwrap())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn trusted_local_account_adoption_reopens_without_relaxing_foreign_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let (original, _) = node(dir.path());
    let keys = original.identity.secret_bytes();
    original
        .configure_privacy(
            dir.path(),
            "pw",
            &original.signed_announce("Original", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    original.set_invisible(true).await.unwrap();
    let new_account = crate::identity::account::Account::generate();
    original
        .persist_account_adoption(
            &new_account.public(),
            || Ok(()),
            || Ok(original.account.public()),
        )
        .unwrap();
    drop(original);
    let (tx, _) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let node = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        new_account,
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        tx,
        ctx,
        ftx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    node.configure_privacy(
        dir.path(),
        "pw",
        &node.signed_announce("Adopted", 1234),
        Arc::new(DiscoveryVisibility::new(false)),
    )
    .unwrap();
    assert!(node.privacy_snapshot().invisible);
    let state = node.privacy.state.read().unwrap();
    assert_eq!(
        state
            .as_ref()
            .unwrap()
            .proofs
            .account_for(&node.identity.public()),
        Some(node.account_id())
    );
}

#[tokio::test]
async fn revoking_an_open_connection_rejects_late_event_without_append() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", addr.port()),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    bob.roster.lock().unwrap().update(
        &alice.signed_announce("Alice", 1234),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &bob.user_id(),
    );
    bob.set_allowed(&alice.account_id(), true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    let server = tokio::spawn(bob.clone().run_accept_loop(listener));
    let mut channel = dial(addr, &alice.identity, Some(&bob.identity.public()))
        .await
        .unwrap();
    bob.set_allowed(&alice.account_id(), false).await.unwrap();
    let conv = crate::node::conversation::dm_conversation_id(
        &alice.identity.public(),
        &bob.identity.public(),
    );
    let mut store = crate::eventlog::EventLog::default();
    store
        .append(crate::eventlog::Event::new(
            &alice.identity,
            conv,
            1,
            vec![],
            1,
            0,
            crate::eventlog::EventKind::Message,
            b"late".to_vec(),
        ))
        .unwrap();
    let store = Mutex::new(store);
    assert!(
        tokio::time::timeout(
            DEADLINE,
            crate::node::session::request_round(&mut channel, &store, conv)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(bob.log.lock().unwrap().events(&conv).unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn joined_group_member_can_sync_group_but_not_dm_or_call() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let group = bob
        .create_channel("Existing group", vec![alice.identity.public()])
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", addr.port()),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    bob.set_invisible(true).await.unwrap();
    bob.roster.lock().unwrap().update(
        &alice.signed_announce("Alice", 1234),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &bob.user_id(),
    );
    let server = tokio::spawn(bob.clone().run_accept_loop(listener));
    let mut log = crate::eventlog::EventLog::default();
    for event in bob.log.lock().unwrap().events(&group).unwrap() {
        log.append(event.clone()).unwrap();
    }
    let (parents, lamport) = log.prepare(&group).unwrap();
    let event = crate::eventlog::Event::new(
        &alice.identity,
        group,
        1,
        parents,
        lamport,
        0,
        crate::eventlog::EventKind::Message,
        b"group".to_vec(),
    );
    let id = event.id;
    log.append(event).unwrap();
    let mut channel = dial(addr, &alice.identity, Some(&bob.identity.public()))
        .await
        .unwrap();
    let store = Mutex::new(log);
    tokio::time::timeout(
        DEADLINE,
        crate::node::session::request_round(&mut channel, &store, group),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        bob.log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .iter()
            .any(|e| e.id == id)
    );
    let conv = crate::node::conversation::dm_conversation_id(
        &alice.identity.public(),
        &bob.identity.public(),
    );
    let mut log = crate::eventlog::EventLog::default();
    log.append(crate::eventlog::Event::new(
        &alice.identity,
        conv,
        1,
        vec![],
        1,
        0,
        crate::eventlog::EventKind::Message,
        b"denied dm".to_vec(),
    ))
    .unwrap();
    let store = Mutex::new(log);
    assert!(
        tokio::time::timeout(
            DEADLINE,
            crate::node::session::request_round(&mut channel, &store, conv)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(bob.log.lock().unwrap().events(&conv).unwrap().is_empty());
    assert!(
        bob.send_call_signal(&alice.user_id(), b"offer")
            .await
            .is_err()
    );
    assert!(bob.privacy_snapshot().allowed_accounts.is_empty());
    server.abort();
}

#[tokio::test]
async fn background_profiles_never_create_private_dm_events_or_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(dir.path());
    alice
        .configure_privacy(
            dir.path(),
            "pw",
            &alice.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    alice.set_invisible(true).await.unwrap();
    let bob = DeviceIdentity::generate();
    let account = crate::identity::account::Account::generate();
    let announce = crate::discovery::Announce::new_with_account(&bob, &account, "Bob", 1);
    alice.roster.lock().unwrap().update(
        &announce,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &alice.user_id(),
    );
    alice
        .set_avatar(Some(b"private avatar".to_vec()))
        .await
        .unwrap();
    let conv =
        crate::node::conversation::dm_conversation_id(&alice.identity.public(), &bob.public());
    assert!(
        alice.log.lock().unwrap().events(&conv).unwrap().is_empty(),
        "background profile must not stage denied peer events"
    );
    assert!(alice.privacy_snapshot().allowed_accounts.is_empty());
}

#[tokio::test]
async fn policy_authorization_is_rechecked_after_waiting_for_operation_gate() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _) = node(dir.path());
    node.configure_privacy(
        dir.path(),
        "pw",
        &node.signed_announce("Alice", 1234),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let guard = node.privacy.gate.read().await;
    let authorized = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let check = authorized.clone();
    let owner = node.clone();
    let mutation = tokio::spawn(async move {
        owner
            .set_invisible_if(true, move || {
                if check.load(std::sync::atomic::Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "owner changed",
                    ))
                }
            })
            .await
    });
    authorized.store(false, std::sync::atomic::Ordering::SeqCst);
    drop(guard);
    assert!(mutation.await.unwrap().is_err());
    assert!(!node.privacy_snapshot().invisible);
}

#[tokio::test]
async fn allowed_carrier_cannot_append_a_stranger_authored_dm_event() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let aa = alice.signed_announce("Alice", 1234);
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob.signed_announce("Bob", addr.port()),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    bob.roster
        .lock()
        .unwrap()
        .update(&aa, IpAddr::V4(Ipv4Addr::LOCALHOST), &bob.user_id());
    bob.set_allowed(&alice.account_id(), true).await.unwrap();
    bob.set_invisible(true).await.unwrap();
    let server = tokio::spawn(bob.clone().run_accept_loop(listener));
    let stranger = DeviceIdentity::generate();
    let conv = crate::node::conversation::dm_conversation_id(
        &alice.identity.public(),
        &bob.identity.public(),
    );
    let event = crate::eventlog::Event::new(
        &stranger,
        conv,
        1,
        vec![],
        1,
        0,
        crate::eventlog::EventKind::Message,
        b"untrusted".to_vec(),
    );
    let mut log = crate::eventlog::EventLog::default();
    log.append(event).unwrap();
    let store = Mutex::new(log);
    let mut channel = dial(addr, &alice.identity, Some(&bob.identity.public()))
        .await
        .unwrap();
    let _ = tokio::time::timeout(
        DEADLINE,
        crate::node::session::request_round(&mut channel, &store, conv),
    )
    .await
    .unwrap();
    assert!(
        bob.log.lock().unwrap().events(&conv).unwrap().is_empty(),
        "permitted transport must not admit unknown event authors"
    );
    server.abort();
}

pub(super) fn node(path: &Path) -> (Arc<Node>, mpsc::UnboundedReceiver<ReceivedDm>) {
    std::fs::create_dir_all(path).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        tx,
        ctx,
        ftx,
        &path.join("messages.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap();
    (node, rx)
}

#[tokio::test]
async fn invisible_node_denies_stranger_before_responder_identity_auth() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _) = node(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    node.configure_privacy(
        dir.path(),
        "pw",
        &node.signed_announce("Private", addr.port()),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    node.set_invisible(true).await.unwrap();
    let server = tokio::spawn(node.clone().run_accept_loop(listener));
    let stranger = DeviceIdentity::generate();
    assert!(matches!(
        tokio::time::timeout(
            DEADLINE,
            dial(addr, &stranger, Some(&node.identity.public()))
        )
        .await
        .unwrap(),
        Err(TransportError::Io(_))
    ));
    server.abort();
}

#[tokio::test]
async fn explicit_send_grants_and_authenticates_return_contact_without_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, mut arx) = node(&dir.path().join("alice"));
    let (bob, mut brx) = node(&dir.path().join("bob"));
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let aa = alice.signed_announce("Alice", al.local_addr().unwrap().port());
    let bob_announcement = bob.signed_announce("Bob", bl.local_addr().unwrap().port());
    alice
        .configure_privacy(
            &dir.path().join("alice"),
            "pw",
            &aa,
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &bob_announcement,
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    alice.set_invisible(true).await.unwrap();
    alice.roster.lock().unwrap().update(
        &bob_announcement,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &alice.user_id(),
    );
    let aserver = tokio::spawn(alice.clone().run_accept_loop(al));
    let bserver = tokio::spawn(bob.clone().run_accept_loop(bl));
    alice.send_dm(&bob.user_id(), b"hello").await.unwrap();
    assert_eq!(
        tokio::time::timeout(DEADLINE, brx.recv())
            .await
            .unwrap()
            .unwrap()
            .text,
        b"hello"
    );
    assert!(
        alice
            .privacy_snapshot()
            .allowed_accounts
            .iter()
            .any(|a| a.id == bob.account_id())
    );
    bob.send_dm(&alice.user_id(), b"reply").await.unwrap();
    assert_eq!(
        tokio::time::timeout(DEADLINE, arx.recv())
            .await
            .unwrap()
            .unwrap()
            .text,
        b"reply"
    );
    aserver.abort();
    bserver.abort();
}

#[tokio::test]
async fn policy_startup_failure_and_unknown_grants_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _) = node(dir.path());
    let own = node.signed_announce("Alice", 1234);
    node.configure_privacy(
        dir.path(),
        "pw",
        &own,
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    assert!(node.set_allowed(&"a".repeat(32), true).await.is_err());
    node.set_invisible(true).await.unwrap();
    std::fs::write(dir.path().join("privacy.policy"), b"corrupt").unwrap();
    assert!(
        node.configure_privacy(
            dir.path(),
            "pw",
            &own,
            Arc::new(DiscoveryVisibility::new(true))
        )
        .is_err()
    );
    assert!(node.privacy_snapshot().invisible);
}

#[tokio::test]
async fn relay_direction_change_revokes_already_assembled_channel_writes() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let expected = bob.identity.public();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = bob.privacy_accept(stream).await.unwrap();
        assert!(
            channel.recv().await.is_err(),
            "staged payload must never arrive after direction revocation"
        );
    });
    let mut channel = alice.privacy_dial(addr, &expected).await.unwrap();
    let staged = b"already assembled encrypted publication".to_vec();
    alice.set_post_office_permissions(false, true);
    alice.set_post_office_permissions(true, true);
    assert!(
        !channel.io_admitted(),
        "permission ABA must not revive an older session"
    );
    assert!(matches!(
        channel.send(&staged).await,
        Err(TransportError::AdmissionDenied)
    ));
    drop(channel);
    server.await.unwrap();
}

#[tokio::test]
async fn interrupted_pairing_can_retry_with_new_code_without_unlocking_normal_sync() {
    let dir = tempfile::tempdir().unwrap();
    let (linker, _) = node(&dir.path().join("linker"));
    let (joiner, _) = node(&dir.path().join("joiner"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    for (name, node, port) in [("linker", &linker, addr.port()), ("joiner", &joiner, 47475)] {
        node.configure_privacy(
            &dir.path().join(name),
            "pw",
            &node.signed_announce(name, port),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    let old = joiner.signed_announce("Before adoption", 47475);
    linker
        .remember_peer(&joiner.identity.public(), Some(&old), addr.ip())
        .unwrap();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let serving = linker.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let channel = serving.privacy_accept(stream).await.unwrap();
        assert!(held_tx.send(channel).is_ok());
        Box::pin(serving.run_accept_loop(listener)).await;
    });
    let before_pairing = joiner
        .privacy_dial(addr, &linker.identity.public())
        .await
        .unwrap();
    let held_inbound = held_rx.await.unwrap();
    assert!(held_inbound.io_admitted());
    let first_code = linker.start_linking();
    let first = joiner
        .link_to_device(addr, &linker.identity.public(), &first_code)
        .await
        .unwrap();
    assert_eq!(first.account_id, linker.account_id());
    assert!(
        !held_inbound.io_admitted(),
        "pairing enrollment must invalidate previously authenticated inbound normal-sync channels"
    );
    drop(held_inbound);
    drop(before_pairing);
    // Model the joiner disconnecting/crashing after the response but before
    // saving the adopted account: its next Noise proof still presents the old pin.
    drop(first);
    assert_ne!(joiner.account_id(), linker.account_id());
    let retry_code = linker.start_linking();
    let mut normal = joiner
        .privacy_dial(addr, &linker.identity.public())
        .await
        .expect("pending retry may authenticate only into the restricted pairing handler");
    let store = joiner.sync_store(normal.peer_identity());
    let sync = super::session::request_round(
        &mut normal,
        &store,
        crate::eventlog::event::ConversationId::new([45; 32]),
    )
    .await;
    assert!(
        sync.is_err(),
        "retired-account retry admission must never expose normal sync"
    );
    drop(normal);
    let wrong = if let Some(rest) = retry_code.strip_prefix('0') {
        format!("1{rest}")
    } else {
        format!("0{}", &retry_code[1..])
    };
    assert!(
        joiner
            .link_to_device(addr, &linker.identity.public(), &wrong)
            .await
            .is_err()
    );
    let retried = joiner
        .link_to_device(addr, &linker.identity.public(), &retry_code)
        .await
        .expect("the authorized original device can recover with the new valid code");
    assert_eq!(retried.account_id, linker.account_id());
    assert_eq!(linker.linked_devices().len(), 2);
    server.abort();
}

#[tokio::test]
async fn physical_scope_tcp_presence_preserves_lan_roster_and_hides_overlay_presence() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    for (node, path, port) in [
        (&alice, dir.path().join("alice"), 4567),
        (&bob, dir.path().join("bob"), addr.port()),
    ] {
        node.initialize_physical_lan_only(true);
        let visibility = Arc::new(DiscoveryVisibility::with_inventory(true, || {
            vec![crate::discovery::scope::LanInterface {
                address: "127.0.0.1".parse().unwrap(),
                prefix: 8,
                index: Some(1),
            }]
        }));
        node.configure_privacy(&path, "pw", &node.signed_announce("LAN", port), visibility)
            .unwrap();
        assert!(node.publicly_visible_at("127.0.0.1:1234".parse().unwrap()));
        assert!(node.publicly_visible_at("[::ffff:127.0.0.1]:1234".parse().unwrap()));
        assert!(!node.publicly_visible_at("100.82.240.19:1234".parse().unwrap()));
        assert!(!node.publicly_visible_at("192.168.99.1:1234".parse().unwrap()));
    }
    alice.roster.lock().unwrap().update(
        &bob.signed_announce("Bob", addr.port()),
        addr.ip(),
        &alice.user_id(),
    );
    bob.roster.lock().unwrap().update(
        &alice.signed_announce("Alice", 4567),
        addr.ip(),
        &bob.user_id(),
    );
    let server_node = bob.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        server_node.privacy_accept(stream).await.unwrap()
    });
    let client = alice
        .privacy_dial(addr, &bob.identity.public())
        .await
        .unwrap();
    let accepted = server.await.unwrap();
    assert!(client.peer_publicly_visible());
    assert!(accepted.peer_publicly_visible());
    assert!(alice.roster.lock().unwrap().get(&bob.user_id()).is_some());
    assert!(bob.roster.lock().unwrap().get(&alice.user_id()).is_some());
    alice.set_invisible(true).await.unwrap();
    assert!(!alice.publicly_visible_at(addr));
}

#[tokio::test]
async fn guarded_privacy_commits_enclose_all_writes_without_holding_state_lock() {
    for operation in ["mode", "allow", "initiate"] {
        let dir = tempfile::tempdir().unwrap();
        let (node, _) = node(dir.path());
        node.configure_privacy(
            dir.path(),
            "pw",
            &node.signed_announce("Alice", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let identity = DeviceIdentity::generate();
        let account = crate::identity::account::Account::generate();
        let proof = Announce::new_with_account(&identity, &account, "Bob", 9);
        node.roster.lock().unwrap().update(
            &proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &node.user_id(),
        );
        let documents = || {
            ["privacy.policy", "peer-proofs", "peer-routes"]
                .map(|name| std::fs::read(dir.path().join(name)).unwrap())
        };
        let before = documents();
        let generation = node.privacy.generation.load(Ordering::SeqCst);
        let deny = |_: &mut dyn FnMut() -> io::Result<()>| Err(denied());
        let rejected = match operation {
            "mode" => node.set_invisible_guarded(true, deny).await,
            "allow" => {
                node.set_allowed_guarded(&account.account_id(), true, deny)
                    .await
            }
            _ => {
                node.initiate_contact_guarded(&account.account_id(), deny)
                    .await
            }
        };
        assert!(rejected.is_err());
        assert_eq!(documents(), before);
        assert_eq!(node.privacy.generation.load(Ordering::SeqCst), generation);
        let enclosing = |commit: &mut dyn FnMut() -> io::Result<()>| {
            // Hosts acquire their session authority here. No privacy-state lock
            // may already be held when that authority is acquired.
            assert!(node.privacy.state.try_write().is_ok());
            assert_eq!(documents(), before);
            commit()?;
            assert_ne!(
                documents(),
                before,
                "{operation} mutation escaped authority"
            );
            assert!(node.privacy.generation.load(Ordering::SeqCst) > generation);
            Ok(())
        };
        match operation {
            "mode" => node.set_invisible_guarded(true, enclosing).await,
            "allow" => {
                node.set_allowed_guarded(&account.account_id(), true, enclosing)
                    .await
            }
            _ => {
                node.initiate_contact_guarded(&account.account_id(), enclosing)
                    .await
            }
        }
        .unwrap();
    }
}

#[tokio::test]
async fn reachable_route_is_not_blocked_by_stalled_former_lan_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let good = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old_addr = old.local_addr().unwrap();
    let good_addr = good.local_addr().unwrap();
    let proof = bob.signed_announce("Bob", good_addr.port());
    alice
        .roster
        .lock()
        .unwrap()
        .update_routed(&proof, good_addr.ip(), &alice.user_id());
    let expected = bob.identity.public();
    let server = tokio::spawn(async move {
        let (stream, _) = good.accept().await.unwrap();
        bob.privacy_accept(stream).await.unwrap()
    });
    let dial = alice.privacy_dial(old_addr, &expected);
    let old_server = async {
        let (_stalled, _) = old.accept().await.unwrap();
        std::future::pending::<()>().await;
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(1), dial) => result,
        _ = old_server => unreachable!(),
    };
    server.abort();
    assert!(
        result.is_ok(),
        "a stale TCP endpoint which accepts but never authenticates must not delay the reachable route"
    );
    let channel = result.unwrap().unwrap();
    assert_eq!(channel.peer_identity(), &expected);
    drop(channel);
    let permits: Vec<_> = (0..crate::node::session_pool::MAX_DIRECTION_CONNECTIONS)
        .map(|_| {
            alice
                .sessions
                .try_outbound()
                .expect("winner and canceled loser release socket capacity")
        })
        .collect();
    assert!(
        alice.sessions.try_outbound().is_none(),
        "racing routes must retain the connection limit"
    );
    drop(permits);
}

#[tokio::test]
async fn liveness_dial_deadline_includes_admission_and_releases_policy_writer() {
    dial_deadline_releases_policy_writer(super::session_pool::AuthenticationPriority::Ordinary)
        .await;
}

#[tokio::test]
async fn liveness_control_dial_deadline_includes_admission_and_releases_policy_writer() {
    dial_deadline_releases_policy_writer(super::session_pool::AuthenticationPriority::Control)
        .await;
}

async fn dial_deadline_releases_policy_writer(
    priority: super::session_pool::AuthenticationPriority,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice");
    let (alice, _) = node(&path);
    let (bob, _) = node(&dir.path().join("bob"));
    alice
        .configure_privacy(
            &path,
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected = bob.identity.public();
    for _ in 0..10 {
        alice.sessions.authentication_timed_out(&expected, address);
    }
    let authentication = alice
        .sessions
        .authentication_admission(alice.peer_endpoint_key(address, &expected))
        .await
        .unwrap();
    tokio::time::pause();
    let sender = alice.clone();
    let dial = tokio::spawn(async move {
        sender
            .privacy_dial_priority(address, &expected, priority)
            .await
    });
    tokio::task::yield_now().await;
    assert!(alice.privacy.gate.try_write().is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    drop(authentication);
    // Complete only the TCP accept using real I/O; the remote never speaks Noise.
    tokio::time::resume();
    let (_stalled, _) = listener.accept().await.unwrap();
    tokio::time::pause();
    let changing = alice.clone();
    let writer = tokio::spawn(async move { changing.set_invisible(true).await });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    tokio::time::advance(Duration::from_millis(19_100)).await;
    let released = tokio::time::timeout(Duration::from_millis(100), writer).await;
    assert!(
        released.is_ok(),
        "authentication extended the whole-dial deadline and retained the privacy gate"
    );
    released.unwrap().unwrap().unwrap();
    assert!(matches!(
        dial.await.unwrap(),
        Err(TransportError::Noise(message)) if message == "dial deadline exceeded"
    ));
    let permits: Vec<_> = (0..crate::node::session_pool::MAX_DIRECTION_CONNECTIONS)
        .map(|_| {
            alice
                .sessions
                .try_outbound()
                .expect("timed-out dial retained socket capacity")
        })
        .collect();
    drop(permits);
}

#[tokio::test]
async fn liveness_control_dial_cancellation_releases_authentication_and_socket_owners() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected = bob.identity.public();
    let sender = alice.clone();
    let peer = expected.clone();
    let dial = tokio::spawn(async move {
        sender
            .privacy_dial_priority(
                address,
                &peer,
                super::session_pool::AuthenticationPriority::Control,
            )
            .await
    });
    let (_stalled, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .expect("the control dial must own a real socket before cancellation")
        .unwrap();
    assert!(alice.privacy.gate.try_write().is_err());
    dial.abort();
    assert!(matches!(dial.await, Err(error) if error.is_cancelled()));
    assert!(alice.privacy.gate.try_write().is_ok());
    let authentication = tokio::time::timeout(
        Duration::from_millis(100),
        alice
            .sessions
            .authentication_admission(alice.peer_endpoint_key(address, &expected)),
    )
    .await
    .expect("cancelling control authentication must release the route to ordinary work")
    .unwrap();
    let permits: Vec<_> = (0..crate::node::session_pool::MAX_DIRECTION_CONNECTIONS)
        .map(|_| {
            alice
                .sessions
                .try_outbound()
                .expect("cancelled control dial retained socket capacity")
        })
        .collect();
    drop(permits);
    drop(authentication);
}

#[tokio::test]
async fn existing_contact_local_acceptance_does_not_wait_for_stalled_network_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice");
    let (alice, _) = node(&path);
    let (bob, _) = node(&dir.path().join("bob"));
    let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old_addr = old.local_addr().unwrap();
    let proof = bob.signed_announce("Bob", old_addr.port());
    alice
        .configure_privacy(
            &path,
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, old_addr.ip(), &alice.user_id());
    alice
        .initiate_contact_locally(&bob.account_id())
        .await
        .unwrap();
    let sender = alice.clone();
    let expected = bob.identity.public();
    let dial = tokio::spawn(async move { sender.privacy_dial(old_addr, &expected).await });
    let (_stalled, _) = old.accept().await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        alice.enqueue_to_account(&bob.account_id(), b"send while old route stalls", None),
    )
    .await;
    dial.abort();
    assert!(
        result.is_ok(),
        "local durable acceptance must not wait behind an existing contact's stalled network handshake"
    );
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn routed_handshake_can_finish_after_three_seconds_without_losing_identity_pin() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected = bob.identity.public();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_millis(3200)).await;
        bob.privacy_accept(stream).await.unwrap()
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        alice.privacy_dial(address, &expected),
    )
    .await;
    server.abort();
    assert!(result.is_ok());
    assert_eq!(
        result
            .unwrap()
            .expect("a slow reachable routed peer must be allowed to finish authentication")
            .peer_identity(),
        &expected
    );
}

#[tokio::test]
async fn alternate_route_reclaims_idle_sessions_before_waiting_on_stale_route() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let good = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = good.local_addr().unwrap();
    let proof = bob.signed_announce("Bob", address.port());
    alice
        .roster
        .lock()
        .unwrap()
        .update_routed(&proof, address.ip(), &alice.user_id());
    let expected = bob.identity.public();
    let mut inbound = Vec::new();
    for _ in 0..(crate::node::session_pool::MAX_DIRECTION_CONNECTIONS - 1) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (outbound, accepted) = tokio::join!(
            alice.privacy_dial(listener.local_addr().unwrap(), &expected),
            async {
                let (stream, _) = listener.accept().await.unwrap();
                bob.privacy_accept(stream).await.unwrap()
            },
        );
        alice.recycle_sync_channel(outbound.unwrap());
        inbound.push(accepted);
    }
    let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dial = alice.privacy_dial(old.local_addr().unwrap(), &expected);
    let server = async {
        let (stream, _) = good.accept().await.unwrap();
        bob.privacy_accept(stream).await.unwrap()
    };
    let stalled = async {
        let (_stream, _) = old.accept().await.unwrap();
        std::future::pending::<()>().await;
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(dial, server).0 }) => result,
        _ = stalled => unreachable!(),
    };
    assert!(
        result.is_ok(),
        "idle pooled connections must yield capacity to a reachable alternate route"
    );
    assert_eq!(result.unwrap().unwrap().peer_identity(), &expected);
}

#[tokio::test]
async fn account_change_recovery_is_read_only_until_exact_device_acceptance() {
    use crate::identity::account::Account;
    let directory = tempfile::tempdir().unwrap();
    let (recipient, _) = node(directory.path());
    recipient
        .configure_privacy(
            directory.path(),
            "pw",
            &recipient.signed_announce("Recipient", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let devices = [DeviceIdentity::generate(), DeviceIdentity::generate()];
    let previous = Account::generate();
    let current = Account::generate();
    for device in &devices {
        let old = Announce::new_with_account(device, &previous, "Before", 2);
        recipient.roster.lock().unwrap().update(
            &old,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &recipient.user_id(),
        );
    }
    recipient.cached_peer_snapshot_async().await.unwrap();
    let claims: Vec<_> = devices
        .iter()
        .map(|device| Announce::new_with_account(device, &current, "After", 2))
        .collect();
    for proof in &claims {
        recipient.roster.lock().unwrap().update(
            proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &recipient.user_id(),
        );
    }
    let documents = || {
        ["privacy.policy", "peer-proofs"]
            .map(|name| std::fs::read(directory.path().join(name)).unwrap())
    };
    let before = documents();
    let changes = recipient.account_binding_changes(&current.account_id());
    assert_eq!(changes.len(), 2);
    assert_eq!(
        recipient.account_binding_changes(&previous.account_id()),
        changes
    );
    assert_eq!(
        documents(),
        before,
        "diagnostics must not grant or persist new bindings"
    );
    assert!(
        recipient
            .accept_account_binding_change_guarded(&changes[0], |_| Err(denied()))
            .await
            .is_err()
    );
    assert_eq!(
        documents(),
        before,
        "retired host authority cannot mutate a binding"
    );
    let selected = changes[0].clone();
    let third = Account::generate();
    let device = devices
        .iter()
        .find(|device| device.public().user_id() == selected.device_id)
        .unwrap();
    let newer = Announce::new_with_account(device, &third, "Changed again", 2);
    assert!(
        recipient
            .accept_account_binding_change_guarded(&selected, |commit| {
                recipient.roster.lock().unwrap().update(
                    &newer,
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    &recipient.user_id(),
                );
                commit()
            })
            .await
            .is_err()
    );
    assert_eq!(
        documents(),
        before,
        "a changed proposal must fail at commit"
    );
    let reviewed = claims
        .iter()
        .find(|proof| proof.user_id == selected.device_id)
        .unwrap();
    recipient.roster.lock().unwrap().update(
        reviewed,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &recipient.user_id(),
    );
    recipient
        .accept_account_binding_change_guarded(&selected, |commit| commit())
        .await
        .unwrap();
    let remaining = recipient.account_binding_changes(&current.account_id());
    assert_eq!(
        remaining.len(),
        1,
        "accepting one device must not accept a second unreviewed change"
    );
    assert_ne!(remaining[0].device_id, selected.device_id);
    assert!(
        recipient
            .privacy_snapshot()
            .allowed_accounts
            .iter()
            .any(|account| account.id == current.account_id())
    );
    let state = recipient.privacy.state.read().unwrap();
    let proofs = &state.as_ref().unwrap().proofs;
    assert!(proofs.accepts_binding(reviewed));
    assert!(
        !proofs.accepts_binding(
            claims
                .iter()
                .find(|proof| proof.user_id == remaining[0].device_id)
                .unwrap()
        )
    );
}

#[tokio::test]
async fn authenticated_preferred_route_keeps_last_slot_ahead_of_known_stalled_route() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    // One wildcard listener accepts two loopback destination IPs on one port.
    // The stale IP accepts TCP but pauses before Noise authentication.
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let good = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let stale = std::net::SocketAddr::from(([127, 0, 0, 2], port));
    let proof = bob.signed_announce("Bob", port);
    let expected = bob.identity.public();
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, good.ip(), &alice.user_id());
    let (connected, accepted) = tokio::join!(alice.privacy_dial(good, &expected), async {
        let (stream, _) = listener.accept().await.unwrap();
        bob.privacy_accept(stream).await.unwrap()
    },);
    drop(connected.unwrap());
    drop(accepted);
    assert_eq!(
        alice.preferred_routes.lock().unwrap().get(&bob.user_id()),
        Some(&good)
    );
    alice
        .roster
        .lock()
        .unwrap()
        .update_routed(&proof, stale.ip(), &alice.user_id());
    let held: Vec<_> = (1..super::session_pool::MAX_DIRECTION_CONNECTIONS)
        .map(|_| alice.sessions.try_outbound().unwrap())
        .collect();
    let stalled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_stalls = stalled.clone();
    let server = tokio::spawn(async move {
        let mut stalled_sockets = Vec::new();
        let mut accepted_channels = Vec::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            if stream.local_addr().unwrap().ip() == stale.ip() {
                observed_stalls.fetch_add(1, Ordering::SeqCst);
                stalled_sockets.push(stream);
            } else {
                accepted_channels.push(bob.privacy_accept(stream).await.unwrap());
            }
        }
    });
    let outcome = tokio::time::timeout(
        Duration::from_millis(700),
        alice.privacy_dial(stale, &expected),
    )
    .await;
    server.abort();
    let _ = server.await;
    assert!(
        outcome.is_ok(),
        "a known stale supplied endpoint consumed the last outbound slot before the authenticated preferred route"
    );
    let channel = outcome.unwrap().unwrap();
    assert_eq!(channel.peer_identity(), &expected);
    assert_eq!(
        stalled.load(Ordering::SeqCst),
        0,
        "the known stale endpoint should never lead the healthy authenticated path"
    );
    drop(channel);
    drop(held);
}

#[test]
fn fresh_one_off_routes_lead_and_signed_ports_supersede_cached_ports() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let old = std::net::SocketAddr::from(([127, 0, 0, 1], 41001));
    let replacement = std::net::SocketAddr::from(([127, 0, 0, 1], 41002));
    let one_off = std::net::SocketAddr::from(([127, 0, 0, 3], 41003));
    let proof = bob.signed_announce("Bob", replacement.port());
    alice
        .preferred_routes
        .lock()
        .unwrap()
        .insert(bob.user_id(), old);
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, replacement.ip(), &alice.user_id());
    let candidates = alice.privacy_route_candidates(replacement, &expected, Some(&proof));
    assert_eq!(candidates[0], replacement);
    assert!(
        !candidates.contains(&old),
        "a newer signed listening port supersedes a cached port on the same IP"
    );
    let candidates = alice.privacy_route_candidates(one_off, &expected, Some(&proof));
    assert_eq!(
        candidates[0], one_off,
        "a genuinely new one-off listener retains explicit priority"
    );
    assert!(candidates.contains(&replacement));
    assert!(!candidates.contains(&old));
}
