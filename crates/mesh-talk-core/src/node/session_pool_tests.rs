//! Real TCP regressions for reusable sync sessions and the Node TCP budget.

use super::tests::node;
use super::*;
use crate::identity::device::DeviceIdentity;
use tokio::net::TcpListener;

#[tokio::test]
async fn completed_receipt_scope_keeps_the_authenticated_connection_for_the_next_message() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let public = bob.identity.public();
    let account = bob.account_id();
    let (addr, count, server) = counted_sync_peer(bob.clone()).await;
    let proof = bob.signed_announce("Bob", addr.port());
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &proof,
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    alice
        .configure_privacy(
            &dir.path().join("alice"),
            "pw",
            &alice.signed_announce("Alice", 1),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, addr.ip(), &alice.user_id());
    let peer = alice
        .roster
        .lock()
        .unwrap()
        .get(&public.user_id())
        .unwrap()
        .clone();
    let id = alice
        .enqueue_to_account(&account, b"one locally queued message", None)
        .await
        .unwrap();
    let destination = alice
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .clone();
    assert!(
        alice
            .transfer_delivery_destination(&peer, &destination, destination.event.conversation_id)
            .await
    );
    let control =
        super::delivery_receipt::delivery_conversation_id(&alice.identity.public(), &public);
    alice
        .transfer_delivery_destination(&peer, &destination, control)
        .await;
    alice
        .deliver_direct(&peer, destination.event.conversation_id)
        .await
        .unwrap();
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "a completed receipt round must not force a new Noise handshake"
    );
    server.abort();
}

#[tokio::test]
async fn successful_sync_rounds_reuse_one_authenticated_tcp_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = accepted.clone();
    let proof = bob.signed_announce("Bob", addr.port());
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &proof,
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, addr.ip(), &alice.user_id());
    let peer = alice
        .roster
        .lock()
        .unwrap()
        .get(&bob.user_id())
        .unwrap()
        .clone();
    let server = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let bob = bob.clone();
            tasks.spawn(async move {
                if let Ok(channel) = bob.privacy_accept(stream).await {
                    bob.serve_connection(channel).await;
                }
            });
        }
    });
    for _ in 0..5 {
        alice
            .deliver_direct(&peer, crate::eventlog::event::ConversationId::new([17; 32]))
            .await
            .unwrap();
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "completed rounds should retain their authenticated channel"
    );
    server.abort();
}

#[tokio::test]
async fn accountless_signed_discovery_reuses_five_completed_generic_rounds() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    assert!(alice.privacy.state.read().unwrap().is_none());
    assert!(bob.privacy.state.read().unwrap().is_none());
    let expected = bob.identity.public();
    let (addr, accepted, server) = counted_sync_peer(bob.clone()).await;
    let proof = Announce::new(&bob.identity, "Accountless Bob", addr.port());
    assert!(proof.valid_presence() && !proof.post_office);
    assert_eq!(proof.public(), expected);
    assert!(proof.account_id().is_none());
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, addr.ip(), &alice.user_id());
    let peer = alice.routing_peer(&bob.user_id()).unwrap();
    for _ in 0..5 {
        alice
            .deliver_direct(&peer, crate::eventlog::ConversationId::new([0x53; 32]))
            .await
            .unwrap();
    }
    let connections = accepted.load(Ordering::SeqCst);
    alice.sessions.clear();
    server.abort();
    let _ = server.await;
    assert_eq!(
        connections, 1,
        "a signed accountless discovery record must not discard a completed identity-authenticated generic sync session"
    );
}

#[tokio::test]
async fn accountless_cached_sync_preserves_identity_role_and_account_pins() {
    for boundary in [
        "unchanged",
        "identity_x",
        "account",
        "relay",
        "historical_pin",
        "historical_pin_no_announce",
        "linked_pin",
        "linked_pin_no_announce",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (alice, _) = node(&dir.path().join("alice"));
        let (bob, _) = node(&dir.path().join("bob"));
        alice
            .configure_privacy(
                &dir.path().join("alice"),
                "pw",
                &alice.signed_announce("Alice", 1),
                Arc::new(DiscoveryVisibility::new(true)),
            )
            .unwrap();
        let expected = bob.identity.public();
        let (addr, accepted, server) = counted_sync_peer(bob.clone()).await;
        let accountless = Announce::new(&bob.identity, "Bob", addr.port());
        alice
            .roster
            .lock()
            .unwrap()
            .update(&accountless, addr.ip(), &alice.user_id());
        let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
        assert!(channel.peer_announcement().is_none());
        super::session::request_round(
            &mut channel,
            &alice.sync_store(&expected),
            crate::eventlog::ConversationId::new([0x54; 32]),
        )
        .await
        .unwrap();
        alice.recycle_sync_channel(channel);
        assert_eq!(alice.sessions.test_capacity_snapshot().3, Some(1));
        let generation = alice.privacy.generation.load(Ordering::SeqCst);
        let mut requested = expected.clone();
        match boundary {
            "identity_x" => {
                let changed = DeviceIdentity::from_secret_bytes(
                    bob.identity.secret_bytes().0,
                    DeviceIdentity::generate().secret_bytes().1,
                );
                let proof = Announce::new(&changed, "Changed X", addr.port());
                assert!(proof.valid_presence());
                assert_eq!(proof.ed25519_pub, expected.ed25519_pub);
                assert_ne!(proof.x25519_pub, expected.x25519_pub);
                requested = proof.public();
            }
            "account" | "relay" => {
                let proof = if boundary == "account" {
                    bob.signed_announce("Account-bound Bob", addr.port())
                } else {
                    Announce::new_post_office(&bob.identity, "Relay Bob", addr.port())
                };
                assert!(proof.valid_presence());
                alice
                    .roster
                    .lock()
                    .unwrap()
                    .update(&proof, addr.ip(), &alice.user_id());
                assert_eq!(
                    alice.roster.lock().unwrap().announcement(&expected),
                    Some(&proof)
                );
            }
            "historical_pin" | "historical_pin_no_announce" => {
                let proof = bob.signed_announce("Pinned Bob", addr.port());
                let mut state = alice.privacy.state.write().unwrap();
                let proofs = &mut state.as_mut().unwrap().proofs;
                proofs.record(&proof).unwrap();
                assert_eq!(proofs.account_for(&expected), Some(bob.account_id()));
                drop(state);
                if boundary == "historical_pin_no_announce" {
                    alice.roster.lock().unwrap().forget(&expected);
                    assert!(
                        alice
                            .roster
                            .lock()
                            .unwrap()
                            .announcement(&expected)
                            .is_none()
                    );
                }
            }
            "linked_pin" | "linked_pin_no_announce" => {
                let mut state = alice.privacy.state.write().unwrap();
                let proofs = &mut state.as_mut().unwrap().proofs;
                assert!(proofs.by_author(&expected.ed25519_pub).is_none());
                proofs
                    .authorize_linked_device(
                        super::super::account_devices::AccountDevice {
                            public: expected.clone(),
                            certificate: alice.account.certify(&expected.ed25519_pub),
                        },
                        &alice.account_id(),
                    )
                    .unwrap();
                assert!(proofs.by_author(&expected.ed25519_pub).is_none());
                assert_eq!(proofs.account_for(&expected), Some(alice.account_id()));
                assert!(!proofs.accepts_binding(&accountless));
                drop(state);
                if boundary == "linked_pin_no_announce" {
                    alice.roster.lock().unwrap().authorize_linked_account(
                        &expected,
                        &alice.account.certify(&expected.ed25519_pub),
                    );
                    assert!(
                        alice
                            .roster
                            .lock()
                            .unwrap()
                            .announcement(&expected)
                            .is_none()
                    );
                }
            }
            "unchanged" => {}
            _ => unreachable!(),
        }
        assert_eq!(
            alice.privacy.generation.load(Ordering::SeqCst),
            generation,
            "presence/pin assertions must not be masked by generation invalidation"
        );
        let reused = alice
            .checkout_reusable_channel(
                super::super::session_pool::SessionProtocol::Sync,
                addr,
                &requested,
            )
            .await
            .unwrap();
        assert_eq!(
            reused.is_some(),
            boundary == "unchanged",
            "boundary={boundary}"
        );
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            1,
            "checking a retained session must not manufacture a replacement handshake"
        );
        drop(reused);
        alice.sessions.clear();
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn bare_noise_reuse_never_substitutes_for_an_unconfigured_durable_sibling_pin() {
    for pinned in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (alice, _) = node(&dir.path().join("alice"));
        let (bob, _) = node(&dir.path().join("bob"));
        let expected = bob.identity.public();
        let (addr, accepted, server) = counted_sync_peer(bob).await;
        let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
        assert!(channel.peer_announcement().is_none());
        super::session::request_round(
            &mut channel,
            &alice.sync_store(&expected),
            crate::eventlog::ConversationId::new([0x55; 32]),
        )
        .await
        .unwrap();
        alice.recycle_sync_channel(channel);
        let generation = alice.privacy.generation.load(Ordering::SeqCst);
        if pinned {
            let certificate = alice.account.certify(&expected.ed25519_pub);
            alice
                .account_devices
                .lock()
                .unwrap()
                .insert(super::super::account_devices::AccountDevice {
                    public: expected.clone(),
                    certificate: certificate.clone(),
                })
                .unwrap();
            alice
                .roster
                .lock()
                .unwrap()
                .authorize_linked_account(&expected, &certificate);
        }
        assert!(alice.privacy.state.read().unwrap().is_none());
        assert!(
            alice
                .roster
                .lock()
                .unwrap()
                .announcement(&expected)
                .is_none()
        );
        assert_eq!(alice.certified_sibling(&expected), pinned);
        assert_eq!(alice.privacy.generation.load(Ordering::SeqCst), generation);
        let reused = alice
            .checkout_reusable_channel(
                super::super::session_pool::SessionProtocol::Sync,
                addr,
                &expected,
            )
            .await
            .unwrap();
        assert_eq!(
            reused.is_some(),
            !pinned,
            "durable sibling pin present={pinned}"
        );
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        drop(reused);
        alice.sessions.clear();
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn outbound_handshakes_are_bounded_and_cancellation_releases_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let expected = DeviceIdentity::generate().public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = accepted.clone();
    let server = tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            held.push(stream);
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut callers = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let alice = alice.clone();
        let expected = DeviceIdentity::generate().public();
        callers.spawn(async move { alice.privacy_dial(addr, &expected).await });
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        7,
        "one shared TCP slot must remain available to inbound traffic"
    );
    callers.abort_all();
    while callers.join_next().await.is_some() {}
    let attempt = tokio::spawn(async move { alice.privacy_dial(addr, &expected).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while accepted.load(Ordering::SeqCst) != 8 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    attempt.abort();
    server.abort();
}

#[tokio::test]
async fn duplicate_hung_endpoint_handshakes_do_not_starve_healthy_sync() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = DeviceIdentity::generate().public();
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = stalled.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = accepted.clone();
    let stalled_server = tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            held.push(stalled.accept().await.unwrap().0);
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut duplicates = tokio::task::JoinSet::new();
    for _ in 0..7 {
        let alice = alice.clone();
        let expected = expected.clone();
        duplicates.spawn(async move { alice.privacy_dial(target, &expected).await });
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while accepted.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Let every spawned producer reach authentication admission. The remote
    // holds real sockets open without responding to their Noise handshakes.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (healthy, _, healthy_server) = counted_sync_peer(bob.clone()).await;
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        let mut channel = alice
            .privacy_sync_dial(healthy, &bob.identity.public())
            .await
            .map_err(|error| error.to_string())?;
        super::session::request_round(
            &mut channel,
            &alice.sync_store(&bob.identity.public()),
            crate::eventlog::ConversationId::new([229; 32]),
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok::<_, String>(())
    })
    .await;
    duplicates.abort_all();
    while duplicates.join_next().await.is_some() {}
    stalled_server.abort();
    healthy_server.abort();
    assert!(
        result.is_ok_and(|round| round.is_ok()),
        "duplicate stalled authentication must leave capacity for a real healthy sync round"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "only one unauthenticated socket per exact endpoint may consume capacity"
    );
}

async fn counted_sync_peer(
    bob: Arc<Node>,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accepted = count.clone();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::SeqCst);
            let bob = bob.clone();
            tasks.spawn(async move {
                if let Ok(channel) = bob.privacy_accept(stream).await {
                    bob.serve_connection(channel).await;
                }
            });
        }
    });
    (addr, count, task)
}

#[tokio::test]
async fn concurrent_live_endpoint_dials_keep_independent_authenticated_channels() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let (address, accepted, server) = counted_sync_peer(bob).await;
    let mut dials = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let alice = alice.clone();
        let expected = expected.clone();
        dials.spawn(async move { alice.privacy_dial(address, &expected).await });
    }
    let mut channels = Vec::new();
    while let Some(result) = dials.join_next().await {
        channels.push(result.unwrap().unwrap());
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 4);
    for channel in &mut channels {
        super::session::request_round(
            channel,
            &alice.sync_store(&expected),
            crate::eventlog::ConversationId::new([230; 32]),
        )
        .await
        .unwrap();
    }
    server.abort();
}

#[tokio::test]
async fn sync_pool_does_not_reuse_for_one_shot_or_wrong_identity_and_drops_cancelled_channels() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let (addr, count, server) = counted_sync_peer(bob).await;
    let mut ch = alice.privacy_sync_dial(addr, &expected).await.unwrap();
    let store = alice.sync_store(ch.peer_identity());
    super::session::request_round(
        &mut ch,
        &store,
        crate::eventlog::event::ConversationId::new([31; 32]),
    )
    .await
    .unwrap();
    alice.recycle_sync_channel(ch);
    let one_shot = alice.privacy_dial(addr, &expected).await.unwrap();
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "call/link/history first frames require a fresh session"
    );
    drop(one_shot);
    let wrong = DeviceIdentity::generate().public();
    assert!(alice.privacy_sync_dial(addr, &wrong).await.is_err());
    assert_eq!(
        count.load(Ordering::SeqCst),
        3,
        "cached channels must pin both identity keys"
    );
    let reused = alice.privacy_sync_dial(addr, &expected).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
    drop(reused); // cancellation/default drop is never an implicit return
    drop(alice.privacy_sync_dial(addr, &expected).await.unwrap());
    assert_eq!(count.load(Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn sync_pool_isolates_endpoint_and_privacy_generation() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let (first, first_count, first_server) = counted_sync_peer(bob.clone()).await;
    let (second, second_count, second_server) = counted_sync_peer(bob).await;
    for (addr, generation_change) in [(first, false), (second, false), (first, true)] {
        if generation_change {
            alice.privacy.generation.fetch_add(1, Ordering::SeqCst);
        }
        let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
        let store = alice.sync_store(channel.peer_identity());
        super::session::request_round(
            &mut channel,
            &store,
            crate::eventlog::event::ConversationId::new([32; 32]),
        )
        .await
        .unwrap();
        alice.recycle_sync_channel(channel);
    }
    assert_eq!(first_count.load(Ordering::SeqCst), 2);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    first_server.abort();
    second_server.abort();
}

#[tokio::test]
async fn sync_pool_reconnects_after_remote_session_close() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed, finished) = tokio::sync::oneshot::channel();
    let server_bob = bob.clone();
    let first = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = server_bob.privacy_accept(stream).await.unwrap();
        let store = server_bob.serve_sync_store(channel.peer_identity());
        // Fingerprint fast path is itself a complete sequential sync round.
        crate::node::session::serve_one(&mut channel, &store)
            .await
            .unwrap();
        drop(channel);
        closed.send(()).unwrap();
        listener
    });
    let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
    let store = alice.sync_store(channel.peer_identity());
    super::session::request_round(
        &mut channel,
        &store,
        crate::eventlog::event::ConversationId::new([33; 32]),
    )
    .await
    .unwrap();
    finished.await.unwrap();
    // The server's local drop only queues FIN. Wait until this endpoint has
    // observed EOF before testing rejection of an already-closed idle session.
    // No application frame remains after the completed request round.
    let closed = tokio::time::timeout(Duration::from_secs(1), channel.recv())
        .await
        .expect("client observes remote session closure");
    assert!(matches!(
        closed,
        Err(crate::transport::TransportError::Io(error))
            if error.kind() == std::io::ErrorKind::UnexpectedEof
    ));
    assert!(!channel.idle_tcp_alive());
    alice.recycle_sync_channel(channel);
    let listener = first.await.unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = bob.privacy_accept(stream).await.unwrap();
        channel.send(b"fresh").await.unwrap();
    });
    let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
    assert_eq!(channel.recv().await.unwrap(), b"fresh");
    server.await.unwrap();
}

#[tokio::test]
async fn shared_tcp_budget_limits_combined_inbound_and_outbound_and_accepts_after_release() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let inbound = listener.local_addr().unwrap();
    let accept_loop = tokio::spawn(alice.clone().run_accept_loop(listener));
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = stalled.local_addr().unwrap();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = count.clone();
    let remote = tokio::spawn(async move {
        let mut streams = Vec::new();
        loop {
            streams.push(stalled.accept().await.unwrap().0);
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut outgoing = tokio::task::JoinSet::new();
    for _ in 0..7 {
        let alice = alice.clone();
        outgoing.spawn(async move {
            alice
                .privacy_dial(target, &DeviceIdentity::generate().public())
                .await
        });
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while count.load(Ordering::SeqCst) != 7 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let first = crate::transport::SecureChannel::connect(
        TcpStream::connect(inbound).await.unwrap(),
        &DeviceIdentity::generate(),
        Some(&alice.identity.public()),
    )
    .await
    .unwrap();
    let expected = alice.identity.public();
    let second = tokio::spawn(async move {
        crate::transport::SecureChannel::connect(
            TcpStream::connect(inbound).await.unwrap(),
            &DeviceIdentity::generate(),
            Some(&expected),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !second.is_finished(),
        "seven outbound TCP handshakes plus one authenticated incoming connection exhaust the shared eight-slot budget"
    );
    drop(first);
    let second_channel = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(second_channel);
    outgoing.abort_all();
    while outgoing.join_next().await.is_some() {}
    remote.abort();
    accept_loop.abort();
}

#[tokio::test]
async fn retained_sync_session_expires_without_further_network_work() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = bob.privacy_accept(stream).await.unwrap();
        let store = bob.serve_sync_store(channel.peer_identity());
        crate::node::session::serve_one(&mut channel, &store)
            .await
            .unwrap();
        assert!(
            channel.recv().await.is_err(),
            "idle expiry closes the actual TCP socket"
        );
    });
    let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
    let store = alice.sync_store(channel.peer_identity());
    super::session::request_round(
        &mut channel,
        &store,
        crate::eventlog::event::ConversationId::new([34; 32]),
    )
    .await
    .unwrap();
    alice.recycle_sync_channel(channel);
    // Below the server's 30-second idle timeout: expiry must happen even while
    // no later checkout or maintenance operation triggers lazy pruning.
    tokio::time::timeout(Duration::from_secs(20), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sync_pool_reauthenticates_after_verified_account_or_relay_role_change() {
    for role_change in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (alice, _) = node(&dir.path().join("alice"));
        let (bob, _) = node(&dir.path().join("bob"));
        bob.configure_privacy(
            &dir.path().join("bob"),
            "pw",
            &bob.signed_announce("Bob", 47475),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let expected = bob.identity.public();
        let (addr, count, server) = counted_sync_peer(bob.clone()).await;
        let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
        let store = alice.sync_store(channel.peer_identity());
        super::session::request_round(
            &mut channel,
            &store,
            crate::eventlog::event::ConversationId::new([35; 32]),
        )
        .await
        .unwrap();
        alice.recycle_sync_channel(channel);
        let changed = if role_change {
            crate::discovery::Announce::new_post_office_with_account(
                &bob.identity,
                &bob.account,
                "Bob relay",
                addr.port(),
            )
        } else {
            crate::discovery::Announce::new_with_account(
                &bob.identity,
                &crate::identity::account::Account::generate(),
                "Adopted Bob",
                addr.port(),
            )
        };
        alice
            .roster
            .lock()
            .unwrap()
            .update(&changed, addr.ip(), &alice.user_id());
        drop(alice.privacy_sync_dial(addr, &expected).await.unwrap());
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "signed account/role changes require reauthentication rather than an old session proof"
        );
        server.abort();
    }
}

#[tokio::test]
async fn sync_pool_rejects_retired_account_before_new_presence_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    for (name, node) in [("alice", &alice), ("bob", &bob)] {
        node.configure_privacy(
            &dir.path().join(name),
            "pw",
            &node.signed_announce(name, 47475),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    let expected = bob.identity.public();
    let (addr, count, server) = counted_sync_peer(bob.clone()).await;
    let mut channel = alice.privacy_sync_dial(addr, &expected).await.unwrap();
    let store = alice.sync_store(channel.peer_identity());
    super::session::request_round(
        &mut channel,
        &store,
        crate::eventlog::event::ConversationId::new([36; 32]),
    )
    .await
    .unwrap();
    alice.recycle_sync_channel(channel);
    // Pairing authorization is durable before Bob restarts with the adopted
    // account. The prior Announce stays on disk until a new signed one arrives.
    let certificate = alice.account.certify(&expected.ed25519_pub);
    alice
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .authorize_linked_device(
            super::account_devices::AccountDevice {
                public: expected.clone(),
                certificate: certificate.clone(),
            },
            &alice.account_id(),
        )
        .unwrap();
    alice
        .roster
        .lock()
        .unwrap()
        .authorize_linked_account(&expected, &certificate);
    assert!(
        alice.privacy_sync_dial(addr, &expected).await.is_err(),
        "an idle session authenticated under the retired account cannot bypass pairing binding admission"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "fresh authentication must be attempted"
    );
    server.abort();
}

#[tokio::test]
async fn fresh_sync_protocol_errors_do_not_trigger_another_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let proof = bob.signed_announce("Bob", addr.port());
    alice
        .roster
        .lock()
        .unwrap()
        .update(&proof, addr.ip(), &alice.user_id());
    let peer = alice
        .roster
        .lock()
        .unwrap()
        .get(&bob.user_id())
        .unwrap()
        .clone();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = accepted.clone();
    let server = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let bob = bob.clone();
            tasks.spawn(async move {
                let mut channel = bob.privacy_accept(stream).await.unwrap();
                channel.recv().await.unwrap();
                channel.send(b"malformed sync response").await.unwrap();
            });
        }
    });
    let result = alice
        .deliver_direct(&peer, crate::eventlog::ConversationId::new([227; 32]))
        .await;
    server.abort();
    assert!(result.is_err());
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "fresh malformed responses must not be retried"
    );
}

#[tokio::test]
async fn sync_pool_reuses_authenticated_fallback_route_and_mapped_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let (addr, accepted, server) = counted_sync_peer(bob).await;
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let absent = unused.local_addr().unwrap();
    drop(unused);
    // Learn an authenticated route, then actually fall back to it from the
    // unavailable input. Other explicit listener addresses remain independent.
    drop(alice.privacy_dial(addr, &expected).await.unwrap());
    let mut warm = alice.privacy_sync_dial(absent, &expected).await.unwrap();
    super::session::request_round(
        &mut warm,
        &alice.sync_store(&expected),
        crate::eventlog::ConversationId::new([228; 32]),
    )
    .await
    .unwrap();
    alice.recycle_sync_channel(warm);
    for endpoint in [
        absent,
        std::net::SocketAddr::new(
            std::net::IpAddr::V6(std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped()),
            addr.port(),
        ),
    ] {
        let (mut channel, reused) = alice
            .privacy_sync_dial_with_reuse(endpoint, &expected)
            .await
            .unwrap();
        assert!(
            reused,
            "a retained authenticated route must be found before dialing"
        );
        super::session::request_round(
            &mut channel,
            &alice.sync_store(&expected),
            crate::eventlog::ConversationId::new([228; 32]),
        )
        .await
        .unwrap();
        alice.recycle_sync_channel(channel);
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn sync_pool_reuses_authenticated_route_after_verified_hint_loses_route_race() {
    let dir = tempfile::tempdir().unwrap();
    let (alice, _) = node(&dir.path().join("alice"));
    let (bob, _) = node(&dir.path().join("bob"));
    let expected = bob.identity.public();
    let (reachable, count, server) = counted_sync_peer(bob.clone()).await;
    let proof = bob.signed_announce("Bob", reachable.port());
    bob.configure_privacy(
        &dir.path().join("bob"),
        "pw",
        &proof,
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let departed = std::net::SocketAddr::new("127.0.0.2".parse().unwrap(), reachable.port());
    {
        let mut roster = alice.roster.lock().unwrap();
        roster.update(&proof, reachable.ip(), &alice.user_id());
        roster.update(&proof, departed.ip(), &alice.user_id());
    }
    for _ in 0..2 {
        // The caller keeps its verified snapshot; the route race chooses the
        // reachable address and returns that actual socket to the sync pool.
        let mut channel = alice.privacy_sync_dial(departed, &expected).await.unwrap();
        assert_eq!(channel.tcp_peer_addr().unwrap(), reachable);
        let store = alice.sync_store(channel.peer_identity());
        super::session::request_round(
            &mut channel,
            &store,
            crate::eventlog::event::ConversationId::new([47; 32]),
        )
        .await
        .unwrap();
        alice.recycle_sync_channel(channel);
    }
    let connections = count.load(Ordering::SeqCst);
    server.abort();
    assert_eq!(
        connections, 1,
        "the completed authenticated route must carry the next sync scope without another Noise handshake"
    );
}
