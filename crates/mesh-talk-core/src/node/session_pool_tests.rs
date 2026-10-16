//! Real TCP regressions for reusable sync sessions and the Node TCP budget.

use super::tests::node;
use super::*;
use crate::identity::device::DeviceIdentity;
use tokio::net::TcpListener;

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
        let expected = expected.clone();
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
