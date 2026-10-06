use super::*;
use crate::{identity::device::DeviceIdentity, node::transport::dial, transport::TransportError};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Mutex,
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
const DEADLINE: Duration = Duration::from_secs(3);

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
    assert!(bob
        .historical_author(&alice.identity.public().ed25519_pub)
        .is_none());
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
    assert!(!bob
        .cached_peer_snapshot()
        .unwrap()
        .iter()
        .any(|p| p.public == alice.identity.public()));
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
    let event = alice.log.lock().unwrap().events(&conv)[0].clone();
    store.lock().unwrap().accept(event.clone()).unwrap();
    let mut channel = bob
        .privacy_dial(addr, &relay.identity.public())
        .await
        .unwrap();
    assert!(super::session::request_round(
        &mut channel,
        &bob.sync_store(&relay.identity.public()),
        conv
    )
    .await
    .is_err());
    assert!(bob.log.lock().unwrap().events(&conv).is_empty());
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
    let revoked = alice.log.lock().unwrap().events(&conv)[1].clone();
    store.lock().unwrap().accept(revoked.clone()).unwrap();
    let mut channel = bob
        .privacy_dial(addr, &relay.identity.public())
        .await
        .unwrap();
    assert!(super::session::request_round(
        &mut channel,
        &bob.sync_store(&relay.identity.public()),
        conv
    )
    .await
    .is_err());
    assert!(!bob.log.lock().unwrap().has(&revoked.id));
    assert!(bob.log.lock().unwrap().has(&event.id));
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
    let event = alice.log.lock().unwrap().events(&conv)[0].clone();
    assert!(store.lock().unwrap().has(&event.id));
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
    assert!(bob
        .historical_author(&DeviceIdentity::generate().public().ed25519_pub)
        .is_none());
    let mut tampered = proof.clone();
    tampered.sig[0] ^= 1;
    assert!(bob
        .remember_peer(
            &proof.public(),
            Some(&tampered),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        )
        .is_err());
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
                .event_ids(&file),
            expected
        );
    }
    assert_eq!(
        alice.account_history(&account.account_id(), 100).len(),
        1,
        "one UI bubble, not one per destination"
    );
    let keys = alice.identity.secret_bytes();
    let own = crate::identity::account::Account::from_secret_bytes(alice.account.secret_bytes());
    drop(alice);
    // Simulate a crash during an append, then verify future scopes remain replayable.
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(base.join("sent-manifest-scopes.log"))
        .unwrap()
        .write_all(&[0, 0, 0, 50, 1, 2])
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
                .event_ids(&file),
            expected,
            "every recipient must retain its own manifest scope after sender restart"
        );
    }
    assert_eq!(
        reopened.account_history(&account.account_id(), 100).len(),
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
        assert_eq!(store.lock().unwrap().event_ids(&file), expected);
    }
    let second = reopened
        .send_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let (_, scopes) = crate::storage::record_log::EncryptedRecordLog::<StoredManifestScope>::open(
        &base.join("sent-manifest-scopes.log"),
        "pw",
        b"MTFSC1",
    )
    .unwrap();
    assert!(
        scopes.is_empty(),
        "tracked file scopes belong to bounded delivery metadata"
    );
    {
        let delivery = reopened.delivery.lock().unwrap();
        for file_conversation in [file, second] {
            let card = delivery
                .file_cards()
                .find(|card| card.file_conversation == file_conversation)
                .unwrap();
            assert_eq!(card.destinations.len(), 2);
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
                let manifest = log.get(&destination.binding.event_id).unwrap();
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
                    log.events(&file_conversation).last().map(|e| e.id)
                );
                for (index, chunk) in log.events(&file_conversation).iter().enumerate() {
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
                .is_empty(),
            "durable file scope must not bypass account revocation"
        );
    }
    assert_eq!(
        reopened.account_history(&account.account_id(), 100).len(),
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
    alice
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .scope_repair_needed = true;
    let file = crate::eventlog::ConversationId::new([31; 32]);
    let parent = crate::eventlog::ConversationId::new([32; 32]);
    let author = crate::eventlog::Author::from_ed25519(alice.identity.public().ed25519_pub);
    let event = crate::eventlog::EventId::new([33; 32]);
    assert!(alice
        .remember_sent_manifest_scope(file, parent, author, event)
        .is_err());
    assert!(!alice
        .privacy
        .state
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .file_scopes
        .contains_key(&file));
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
    alice
        .remember_sent_manifest_scope(file, parent, author, event)
        .unwrap();
    alice
        .remember_sent_manifest_scope(file, parent, author, event)
        .unwrap();
    let (_, scopes) = crate::storage::record_log::EncryptedRecordLog::<StoredManifestScope>::open(
        &path, "pw", b"MTFSC1",
    )
    .unwrap();
    assert_eq!(
        scopes.len(),
        1,
        "retries must repair the journal and remain idempotent"
    );
    assert!(
        !alice
            .privacy
            .state
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .scope_repair_needed
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
        let event = alice.log.lock().unwrap().events(&parent)[0].id;
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
        .wall_clock;
    alice
        .received_files
        .lock()
        .unwrap()
        .record_durable(&super::received_log::ReceivedEntry {
            event_id: originals[0],
            conversation: crate::node::conversation::account_conversation_id(
                &alice.account_id(),
                &account.account_id(),
            ),
            from: alice.user_id(),
            wall_clock: original_wall_clock,
            plaintext: manifest.encode(),
        })
        .unwrap();
    let expected = alice.log.lock().unwrap().event_ids(&file);
    let keys = alice.identity.secret_bytes();
    let own = crate::identity::account::Account::from_secret_bytes(alice.account.secret_bytes());
    drop(alice);
    let scope_path = dir.path().join("sent-manifest-scopes.log");
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
                .event_ids(&file),
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
    let event = alice.log.lock().unwrap().events(&parent)[0].id;
    alice
        .remember_sent_manifest_scope(file, parent, author, event)
        .unwrap();
    let (_, scopes) = crate::storage::record_log::EncryptedRecordLog::<StoredManifestScope>::open(
        &scope_path,
        "pw",
        b"MTFSC1",
    )
    .unwrap();
    assert_eq!(scopes.len(), 3);
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
        assert_eq!(store.lock().unwrap().event_ids(&file), expected);
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
    assert!(alice
        .privacy_snapshot()
        .allowed_accounts
        .iter()
        .any(|a| a.id == account.account_id()));
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
    assert!(bob
        .sync_store(&alice.identity.public())
        .lock()
        .unwrap()
        .ingest(genesis)
        .is_err());
    assert!(bob.log.lock().unwrap().events(&conv).is_empty());
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
    let last = bob.log.lock().unwrap().events(&group).last().unwrap().id;
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
        !bob.group_member(&alice.identity.public()),
        "stale channel book must not disclose signed listening presence to a removed member"
    );
    assert!(bob
        .sync_store(&alice.identity.public())
        .lock()
        .unwrap()
        .event_ids(&group)
        .is_empty());
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
    assert_eq!(store.lock().unwrap().event_ids(&conv), vec![allowed.id]);
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
    assert_eq!(bob.log.lock().unwrap().events(&conv).len(), 1);
    bob.set_allowed(&alice.account_id(), false).await.unwrap();
    assert!(store.lock().unwrap().event_ids(&conv).is_empty());
    assert_eq!(
        bob.log.lock().unwrap().events(&conv).len(),
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
    assert!(!visibility
        .send_announce(&sender, b"presence", receiver.local_addr().unwrap())
        .await
        .unwrap());
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
    drop(original);
    let (tx, _) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    let new_account = crate::identity::account::Account::generate();
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
    assert!(tokio::time::timeout(
        DEADLINE,
        crate::node::session::request_round(&mut channel, &store, conv)
    )
    .await
    .unwrap()
    .is_err());
    assert!(bob.log.lock().unwrap().events(&conv).is_empty());
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
    for event in bob.log.lock().unwrap().events(&group) {
        log.append(event.clone()).unwrap();
    }
    let (parents, lamport) = log.prepare(&group);
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
    assert!(bob
        .log
        .lock()
        .unwrap()
        .events(&group)
        .iter()
        .any(|e| e.id == id));
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
    assert!(tokio::time::timeout(
        DEADLINE,
        crate::node::session::request_round(&mut channel, &store, conv)
    )
    .await
    .unwrap()
    .is_err());
    assert!(bob.log.lock().unwrap().events(&conv).is_empty());
    assert!(bob
        .send_call_signal(&alice.user_id(), b"offer")
        .await
        .is_err());
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
        alice.log.lock().unwrap().events(&conv).is_empty(),
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
        bob.log.lock().unwrap().events(&conv).is_empty(),
        "permitted transport must not admit unknown event authors"
    );
    server.abort();
}

fn node(path: &Path) -> (Arc<Node>, mpsc::UnboundedReceiver<ReceivedDm>) {
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
    let ba = bob.signed_announce("Bob", bl.local_addr().unwrap().port());
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
        &ba,
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    alice.set_invisible(true).await.unwrap();
    alice
        .roster
        .lock()
        .unwrap()
        .update(&ba, IpAddr::V4(Ipv4Addr::LOCALHOST), &alice.user_id());
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
    assert!(alice
        .privacy_snapshot()
        .allowed_accounts
        .iter()
        .any(|a| a.id == bob.account_id()));
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
    assert!(node
        .configure_privacy(
            dir.path(),
            "pw",
            &own,
            Arc::new(DiscoveryVisibility::new(true))
        )
        .is_err());
    assert!(node.privacy_snapshot().invisible);
}
