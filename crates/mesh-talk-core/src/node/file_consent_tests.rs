//! End-to-end file control and byte delivery through federated opaque mailboxes.
use super::test_support::node_with_files;
use crate::discovery::Announce;
use crate::eventlog::{ConversationId, EventId};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

struct RejectingDirectEndpoint {
    address: std::net::SocketAddr,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RejectingDirectEndpoint {
    async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                // Never authenticate or serve a Node protocol. A closed port
                // can spend the TCP connect budget on Windows; EOF is immediate.
                drop(stream);
            }
        });
        Self {
            address,
            task: Some(task),
        }
    }

    async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for RejectingDirectEndpoint {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[tokio::test]
async fn unavailable_direct_endpoint_closes_without_waiting_for_protocol_bytes() {
    use tokio::io::AsyncReadExt;

    let rejecting = RejectingDirectEndpoint::bind().await;
    // A silent first connection must not prevent subsequent connections from
    // receiving EOF. The fixture never waits for a handshake or spawns a server.
    let mut first = tokio::net::TcpStream::connect(rejecting.address)
        .await
        .unwrap();
    let mut second = tokio::net::TcpStream::connect(rejecting.address)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        assert_eq!(first.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(second.read(&mut [0]).await.unwrap(), 0);
    })
    .await
    .expect("unavailable direct endpoints must close without protocol progress");
    rejecting.shutdown().await;
}

#[tokio::test]
async fn offline_dm_file_custody_precedes_consent_and_survives_sender_shutdown() {
    use crate::postoffice::{PostOffice, opaque::RelayMailbox};
    use std::time::Duration;

    let sender_dir = tempfile::tempdir().unwrap();
    let recipient_dir = tempfile::tempdir().unwrap();
    let relay_dir = tempfile::tempdir().unwrap();
    let rejecting = RejectingDirectEndpoint::bind().await;
    let alice = DeviceIdentity::generate();
    let alice_account = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let alice_keys = alice.secret_bytes();
    let alice_account_key = alice_account.secret_bytes();
    let ap = Announce::new_with_account(&alice, &alice_account, "Alice", rejecting.address.port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", rejecting.address.port());
    let (sender, _, _) = node_with_files(sender_dir.path(), alice, alice_account, &bp);

    let relay = DeviceIdentity::generate();
    let relay_keys = relay.secret_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_address = listener.local_addr().unwrap();
    let relay_proof = Announce::new_post_office(&relay, "Relay", relay_address.port());
    let retained = Arc::new(Mutex::new(
        PostOffice::open(&relay_dir.path().join("relay"), "pw", relay).unwrap(),
    ));
    let relay_task = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1),
        listener,
        retained.clone(),
    ));
    sender
        .roster
        .lock()
        .unwrap()
        .update(&relay_proof, relay_address.ip(), &sender.user_id());
    let source = sender_dir.path().join("offline-document.bin");
    let bytes = vec![47; crate::file::CHUNK_SIZE + 73];
    std::fs::write(&source, &bytes).unwrap();
    let (card, file) = sender
        .enqueue_file_to_account(
            &bp.account_id().unwrap(),
            &source,
            crate::file::FileKind::File,
        )
        .await
        .unwrap();
    let (manifest, final_chunk) = {
        let delivery = sender.delivery.lock().unwrap();
        let card = delivery.file_card(card).unwrap();
        (
            card.destinations[0].binding.event_id,
            card.final_chunk.unwrap(),
        )
    };
    assert_eq!(sender.outgoing_file_consent_status(card, manifest), None);
    let mailbox = RelayMailbox::new(&sender.identity, &bp.public(), &relay_proof.public()).unwrap();
    let outer_final = mailbox
        .wrap(
            &sender
                .log
                .lock()
                .unwrap()
                .get(&final_chunk)
                .unwrap()
                .unwrap(),
        )
        .unwrap()
        .id;
    let worker = tokio::spawn(sender.clone().run_delivery_loop());
    let custody = tokio::time::timeout(Duration::from_secs(8), async {
        while !retained.lock().unwrap().has(&outer_final) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    assert!(
        custody.is_ok(),
        "the post office must hold the encrypted final chunk before the recipient starts or approves"
    );
    assert!(!retained.lock().unwrap().has(&final_chunk));
    assert_eq!(sender.outgoing_file_consent_status(card, manifest), None);
    assert!(!sender.outgoing_file_progress(card).unwrap().delivered);
    drop(sender);

    let (recipient, _, mut offers) = node_with_files(recipient_dir.path(), bob, bob_account, &ap);
    recipient.replace_verified_dm_pull_peers(vec![ap.user_id.clone()]);
    recipient
        .roster
        .lock()
        .unwrap()
        .update(&relay_proof, relay_address.ip(), &recipient.user_id());
    recipient.drain_from_post_office().await;
    assert_eq!(offers.try_recv().unwrap().file_conv, file);
    assert_eq!(recipient.file_progress(file).unwrap().unwrap().done, 0);
    assert!(recipient.read_file(file).is_err());
    recipient.decide_attachment(file, true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            recipient.drain_from_post_office().await;
            if recipient.file_ready_to_save(file).unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("approval must retrieve retained bytes while the sender stays offline");
    let saved = recipient_dir.path().join("saved-document.bin");
    recipient.save_file(file, &saved).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), bytes);
    recipient.drain_from_post_office().await;

    let (sender, _, _) = node_with_files(
        sender_dir.path(),
        DeviceIdentity::from_secret_bytes(alice_keys.0, alice_keys.1),
        Account::from_secret_bytes(alice_account_key),
        &bp,
    );
    sender.replace_verified_dm_pull_peers(vec![bp.user_id.clone()]);
    sender
        .roster
        .lock()
        .unwrap()
        .update(&relay_proof, relay_address.ip(), &sender.user_id());
    sender.drain_from_post_office().await;
    sender.flush_file_delivery(card).await;
    assert!(
        sender.outgoing_file_progress(card).unwrap().delivered,
        "only the exact recipient's verified-save control completes the transfer"
    );
    relay_task.abort();
    let _ = relay_task.await;
    rejecting.shutdown().await;
}

#[tokio::test]
async fn pending_direct_dm_file_also_deposits_opaque_relay_custody() {
    use crate::postoffice::{PostOffice, opaque::RelayMailbox};
    use std::time::Duration;

    let sender_dir = tempfile::tempdir().unwrap();
    let recipient_dir = tempfile::tempdir().unwrap();
    let relay_dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let alice_account = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "Bob",
        listener.local_addr().unwrap().port(),
    );
    let ap = Announce::new_with_account(&alice, &alice_account, "Alice", 1);
    let (sender, _, _) = node_with_files(sender_dir.path(), alice, alice_account, &bp);
    let (recipient, _, mut offers) = node_with_files(recipient_dir.path(), bob, bob_account, &ap);
    let recipient_task = tokio::spawn(recipient.clone().run_accept_loop(listener));
    let relay = DeviceIdentity::generate();
    let keys = relay.secret_bytes();
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_address = relay_listener.local_addr().unwrap();
    let relay_proof = Announce::new_post_office(&relay, "Relay", relay_address.port());
    let retained = Arc::new(Mutex::new(
        PostOffice::open(&relay_dir.path().join("relay"), "pw", relay).unwrap(),
    ));
    let relay_task = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        relay_listener,
        retained.clone(),
    ));
    sender
        .roster
        .lock()
        .unwrap()
        .update(&relay_proof, relay_address.ip(), &sender.user_id());
    let source = sender_dir.path().join("approval-later.bin");
    std::fs::write(&source, vec![71; crate::file::CHUNK_SIZE + 9]).unwrap();
    let (card, file) = sender
        .enqueue_file_to_account(
            &recipient.account_id(),
            &source,
            crate::file::FileKind::File,
        )
        .await
        .unwrap();
    let final_chunk = sender
        .delivery
        .lock()
        .unwrap()
        .file_card(card)
        .unwrap()
        .final_chunk
        .unwrap();
    let mailbox = RelayMailbox::new(&sender.identity, &bp.public(), &relay_proof.public()).unwrap();
    let outer_final = mailbox
        .wrap(
            &sender
                .log
                .lock()
                .unwrap()
                .get(&final_chunk)
                .unwrap()
                .unwrap(),
        )
        .unwrap()
        .id;
    // The immediate API and the retained worker must both be able to park bytes
    // without turning a recipient's pending decision into direct chunk consent.
    sender.flush_file_delivery(card).await;
    assert!(
        retained.lock().unwrap().has(&outer_final),
        "a reachable recipient awaiting approval must not prevent immediate relay custody"
    );
    assert_eq!(offers.try_recv().unwrap().file_conv, file);
    assert_eq!(recipient.file_progress(file).unwrap().unwrap().done, 0);
    assert!(!sender.outgoing_file_progress(card).unwrap().delivered);

    // A second immutable send exercises the production worker's independent
    // transition from direct Pending to the same custody route.
    let (second, second_file) = sender
        .enqueue_file_to_account(
            &recipient.account_id(),
            &source,
            crate::file::FileKind::File,
        )
        .await
        .unwrap();
    let second_final = sender
        .delivery
        .lock()
        .unwrap()
        .file_card(second)
        .unwrap()
        .final_chunk
        .unwrap();
    let second_outer = mailbox
        .wrap(
            &sender
                .log
                .lock()
                .unwrap()
                .get(&second_final)
                .unwrap()
                .unwrap(),
        )
        .unwrap()
        .id;
    let worker = tokio::spawn(sender.clone().run_delivery_loop());
    let custody = tokio::time::timeout(Duration::from_secs(8), async {
        while !retained.lock().unwrap().has(&second_outer) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    assert!(
        custody.is_ok(),
        "the retained file worker must try relay custody after a direct pending answer"
    );
    assert_eq!(
        recipient.file_progress(second_file).unwrap().unwrap().done,
        0
    );
    assert!(!sender.outgoing_file_progress(second).unwrap().delivered);
    recipient_task.abort();
    relay_task.abort();
    let _ = recipient_task.await;
    let _ = relay_task.await;
}

#[tokio::test]
async fn federated_file_consent_and_complete_bytes_need_no_direct_client_route() {
    use crate::postoffice::{PostOffice, federation::FederationDirectory};
    use std::collections::HashSet;

    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let alice_keys = alice.secret_bytes();
    let alice_account_key = aa.secret_bytes();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let bob_keys = bob.secret_bytes();
    // Neither client serves direct connections. The unavailable direct endpoint
    // closes before authentication; only the two relays can serve the protocol.
    let rejecting = RejectingDirectEndpoint::bind().await;
    let ap = Announce::new_with_account(&alice, &aa, "Alice", rejecting.address.port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", rejecting.address.port());
    let (a, mut messages, _) = node_with_files(ad.path(), alice, aa, &bp);
    let (b, _, mut offers) = node_with_files(bd.path(), bob, bob_account, &ap);
    a.set_bridge_discovery_enabled(true);
    b.set_bridge_discovery_enabled(true);
    a.replace_verified_dm_pull_peers(vec![bp.user_id.clone()]);
    b.replace_verified_dm_pull_peers(vec![ap.user_id.clone()]);

    let p = DeviceIdentity::generate();
    let q = DeviceIdentity::generate();
    let pl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ql = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let paddr = pl.local_addr().unwrap();
    let qaddr = ql.local_addr().unwrap();
    let duplicate = |id: &DeviceIdentity| {
        let (ed, x) = id.secret_bytes();
        DeviceIdentity::from_secret_bytes(ed, x)
    };
    let ps = Arc::new(Mutex::new(
        PostOffice::open(&pd.path().join("p"), "pw", duplicate(&p)).unwrap(),
    ));
    let qs = Arc::new(Mutex::new(
        PostOffice::open(&pd.path().join("q"), "pw", duplicate(&q)).unwrap(),
    ));
    let directory = FederationDirectory::new();
    let pt = tokio::spawn(
        super::postbox::run_federated_relay_accept_loop_with_directory(
            duplicate(&p),
            pl,
            ps.clone(),
            Arc::new(HashSet::from([q.user_id()])),
            FederationDirectory::new(),
        ),
    );
    let qt = tokio::spawn(
        super::postbox::run_federated_relay_accept_loop_with_directory(
            duplicate(&q),
            ql,
            qs.clone(),
            Arc::new(HashSet::from([p.user_id()])),
            FederationDirectory::new(),
        ),
    );
    a.roster.lock().unwrap().update(
        &Announce::new_post_office(&p, "P", paddr.port()),
        paddr.ip(),
        &a.user_id(),
    );
    b.roster.lock().unwrap().update(
        &Announce::new_post_office(&q, "Q", qaddr.port()),
        qaddr.ip(),
        &b.user_id(),
    );
    let source = ad.path().join("federated.bin");
    let bytes = vec![29; crate::file::CHUNK_SIZE + 53];
    std::fs::write(&source, &bytes).unwrap();
    let (card, file) = a
        .enqueue_file_to_account(&b.account_id(), &source, crate::file::FileKind::File)
        .await
        .unwrap();
    a.flush_delivery(card).await;
    directory
        .sync_with_relay(qaddr, &p, &q.public(), &ps)
        .await
        .unwrap();
    b.drain_from_post_office().await;
    assert_eq!(offers.try_recv().unwrap().file_conv, file);
    assert!(b.read_file(file).is_err());
    let identity = a.file_message_identity_bundle(card).unwrap().unwrap();
    let mut malformed = identity.ciphertext.clone();
    malformed.push(255);
    let malformed = crate::eventlog::Event::new(
        &a.identity,
        identity.conversation_id,
        identity.seq,
        identity.parents.clone(),
        identity.lamport,
        identity.wall_clock,
        identity.kind,
        malformed,
    );
    let identity_mailbox =
        crate::postoffice::opaque::RelayMailbox::federated(&a.identity, &bp.public()).unwrap();
    let malformed_outer = identity_mailbox.wrap(&malformed).unwrap();
    qs.lock().unwrap().accept(malformed_outer.clone()).unwrap();
    b.drain_from_post_office().await;
    assert!(
        b.opaque_receive_cache
            .lock()
            .unwrap()
            .seen(identity_mailbox.id(), &b.log.lock().unwrap())
            .unwrap()
            .contains(&malformed_outer.id),
        "malformed reserved identity controls must be consumed, not retained in the deferred queue"
    );
    assert!(!b.log.lock().unwrap().has(&malformed.id).unwrap());
    b.decide_attachment(file, true).unwrap();
    let pending_controls = b.file_consent_events(&ap.public()).unwrap();
    assert!(!pending_controls.is_empty());
    let mailbox =
        crate::postoffice::opaque::RelayMailbox::new(&b.identity, &ap.public(), &q.public())
            .unwrap();
    b.set_post_office_permissions(false, true);
    b.drain_from_post_office().await;
    for control in &pending_controls {
        assert!(
            !qs.lock().unwrap().has(&mailbox.wrap(control).unwrap().id),
            "receive-only must not publish signed file-consent controls"
        );
    }
    b.set_post_office_permissions(true, true);
    b.drain_from_post_office().await;
    for control in &pending_controls {
        assert!(
            qs.lock().unwrap().has(&mailbox.wrap(control).unwrap().id),
            "reenabling sends must publish retained consent controls"
        );
    }
    let binding = a
        .delivery
        .lock()
        .unwrap()
        .file_card(card)
        .unwrap()
        .destinations[0]
        .binding
        .event_id;
    let control = b
        .file_consent
        .lock()
        .unwrap()
        .published_control(binding)
        .unwrap()
        .clone();
    let original = super::file_consent::Decision::open(&control).unwrap();
    for wrong_binding in 0..5 {
        let mut payload = original.clone();
        match wrong_binding {
            0 => payload.file = ConversationId::new([93; 32]),
            1 => payload.manifest = EventId::new([94; 32]),
            2 => payload.sender_account = Some("c".repeat(32)),
            3 => payload.recipient_account = "d".repeat(32),
            _ => payload.completion = Some(EventId::new([95; 32])),
        }
        let unrelated = payload.event(&b.identity, 99).unwrap();
        assert!(
            a.accept_file_consent_event(&unrelated, &bp.public())
                .unwrap()
        );
        assert_eq!(a.outgoing_file_consent_status(card, binding), None);
    }
    a.delivery_suspended
        .store(true, std::sync::atomic::Ordering::Release);
    a.accept_file_consent_event(&control, &bp.public()).unwrap();
    assert_eq!(
        a.outgoing_file_consent_status(card, binding),
        None,
        "a retired account runtime must not accept new file controls"
    );
    a.delivery_suspended
        .store(false, std::sync::atomic::Ordering::Release);
    b.send_dm(&a.user_id(), b"primer").await.unwrap();
    b.send_dm(&a.user_id(), b"chat survives consent storage failure")
        .await
        .unwrap();
    directory
        .sync_with_relay(qaddr, &p, &q.public(), &ps)
        .await
        .unwrap();
    a.file_consent
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), a.drain_from_post_office())
        .await
        .expect("failed consent storage must defer retries to the next drain, not re-fetch the same blob thousands of times");
    let mut received = Vec::new();
    while let Ok(message) = messages.try_recv() {
        received.push(message.text);
    }
    assert!(
        received
            .iter()
            .any(|text| text == b"chat survives consent storage failure"),
        "failed consent persistence must not block unrelated chat events"
    );
    assert_eq!(a.outgoing_file_consent_status(card, binding), None);
    let retry_mailbox =
        crate::postoffice::opaque::RelayMailbox::federated(&a.identity, &bp.public()).unwrap();
    let deferred_control = retry_mailbox.wrap(&control).unwrap().id;
    assert!(
        !a.opaque_receive_cache
            .lock()
            .unwrap()
            .seen(retry_mailbox.id(), &a.log.lock().unwrap())
            .unwrap()
            .contains(&deferred_control),
        "failed controls must not enter the node-wide seen cache"
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        a.drain_from_post_office(),
    )
    .await
    .expect("a second drain during the outage must remain bounded");
    assert_eq!(a.outgoing_file_consent_status(card, binding), None);
    a.file_consent
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    a.drain_from_post_office().await;
    assert_eq!(
        a.outgoing_file_consents
            .lock()
            .unwrap()
            .get(&(card, binding))
            .copied(),
        Some(crate::eventlog::sync::FileConsentStatus::Accepted),
        "the exact recipient's consent must return through federation",
    );
    assert!(!a.log.lock().unwrap().has(&control.id).unwrap());
    assert!(!ps.lock().unwrap().has(&control.id));
    assert!(!qs.lock().unwrap().has(&control.id));
    b.decide_attachment(file, false).unwrap();
    b.decide_attachment(file, true).unwrap();
    let refreshed = b.file_consent_events(&ap.public()).unwrap();
    assert!(
        refreshed.iter().any(|event| event.seq > control.seq),
        "reapproval after an unpublished decline must issue a fresh revision"
    );
    let channel_manifest = {
        let id = DeviceIdentity::from_secret_bytes(alice_keys.0, alice_keys.1);
        let event = crate::eventlog::Event::new(
            &id,
            ConversationId::new([88; 32]),
            1,
            vec![],
            1,
            0,
            crate::eventlog::EventKind::FileManifest,
            vec![],
        );
        b.log.lock().unwrap().append(event.clone()).unwrap();
        let manifest = b.files.lock().unwrap().manifest(&file).unwrap().clone();
        b.files
            .lock()
            .unwrap()
            .record_event(event.id, event.author, manifest);
        event.id
    };
    assert!(
        b.file_consent_events(&ap.public())
            .unwrap()
            .iter()
            .all(
                |event| super::file_consent::Decision::open(event).unwrap().manifest
                    != channel_manifest
            ),
        "group offers must not publish private per-member consent controls"
    );
    drop(a);
    let (a, _, _) = node_with_files(
        ad.path(),
        DeviceIdentity::from_secret_bytes(alice_keys.0, alice_keys.1),
        Account::from_secret_bytes(alice_account_key),
        &bp,
    );
    a.set_bridge_discovery_enabled(true);
    a.replace_verified_dm_pull_peers(vec![bp.user_id.clone()]);
    a.roster.lock().unwrap().update(
        &Announce::new_post_office(&p, "P", paddr.port()),
        paddr.ip(),
        &a.user_id(),
    );
    assert!(a.outgoing_file_consents.lock().unwrap().is_empty());
    assert_eq!(
        a.outgoing_file_consent_status(card, binding),
        Some(crate::eventlog::sync::FileConsentStatus::Accepted),
        "sender restart must retain the authenticated decision"
    );
    a.flush_file_delivery(card).await;
    assert!(
        !a.outgoing_file_progress(card).unwrap().delivered,
        "relay custody cannot claim that the recipient received the file"
    );
    // Immediate flush is best effort. Exercise the production worker's retained
    // exchanges while the fixture drives the independent federation/drain stages.
    let worker = tokio::spawn(a.clone().run_delivery_loop());
    let received = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            directory
                .sync_with_relay(qaddr, &p, &q.public(), &ps)
                .await
                .unwrap();
            b.drain_from_post_office().await;
            if b.file_ready_to_save(file).unwrap() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    assert!(
        received.is_ok(),
        "federated bytes did not converge: recipient={:?}, sender={:?}",
        b.file_progress(file).unwrap(),
        a.outgoing_file_progress(card)
    );
    assert!(
        !a.outgoing_file_progress(card).unwrap().delivered,
        "relay custody before recipient save must not confirm completion"
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
    let identity = a.file_message_identity_bundle(binding).unwrap().unwrap();
    let host = super::conversation::account_conversation_id(&a.account_id(), &b.account_id());
    assert!(
        b.has_file_message_identity(host, binding).unwrap(),
        "source identity metadata must cross federation without a direct client route"
    );
    assert!(!b.log.lock().unwrap().has(&identity.id).unwrap());
    assert!(!ps.lock().unwrap().has(&identity.id));
    assert!(!qs.lock().unwrap().has(&identity.id));
    assert!(super::file_consent::Decision::open(&identity).is_none());
    assert!(
        b.accept_file_consent_event(&identity, &ap.public())
            .unwrap(),
        "legacy reserved-control handling consumes the new identity subtype without creating chat history"
    );
    let saved = bd.path().join("received.bin");
    b.save_file(file, &saved).unwrap();
    b.drain_from_post_office().await;
    directory
        .sync_with_relay(qaddr, &p, &q.public(), &ps)
        .await
        .unwrap();
    a.drain_from_post_office().await;
    a.flush_file_delivery(card).await;
    assert!(
        a.outgoing_file_progress(card).unwrap().delivered,
        "verified recipient completion must return to the sender through federation"
    );
    b.decide_attachment(file, false).unwrap();
    b.drain_from_post_office().await;
    directory
        .sync_with_relay(qaddr, &p, &q.public(), &ps)
        .await
        .unwrap();
    a.drain_from_post_office().await;
    assert!(a.outgoing_file_progress(card).unwrap().delivered);
    assert_eq!(
        a.outgoing_file_progress(card).unwrap().consent,
        None,
        "a later policy change must not replace completed delivery with an approval prompt"
    );
    let adopted_account = Account::generate();
    b.persist_account_adoption(
        &adopted_account.public(),
        || Ok(()),
        || Ok(adopted_account.public()),
    )
    .unwrap();
    drop(b);
    let (adopted, _, _) = node_with_files(
        bd.path(),
        DeviceIdentity::from_secret_bytes(bob_keys.0, bob_keys.1),
        adopted_account,
        &ap,
    );
    assert!(
        adopted
            .file_consent_events(&ap.public())
            .unwrap()
            .iter()
            .all(|event| super::file_consent::Decision::open(event)
                .unwrap()
                .completion
                .is_none()),
        "an adopted account must not attest to the previous account's saved file"
    );
    // Model an offer snapshot captured before erase commits, while its cached
    // FileBook/log entry is still visible to the publisher.
    let wire_scope =
        super::conversation::dm_conversation_id(&adopted.identity.public(), &ap.public());
    adopted
        .delivery
        .lock()
        .unwrap()
        .erase_file(wire_scope, binding)
        .unwrap();
    assert!(
        adopted
            .file_consent_events(&ap.public())
            .unwrap()
            .is_empty(),
        "a durable erase tombstone must suppress a stale publication snapshot"
    );
    pt.abort();
    qt.abort();
    let _ = pt.await;
    let _ = qt.await;
    rejecting.shutdown().await;
}

#[tokio::test]
async fn direct_file_approval_shares_signed_revision_with_relay_delivery() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "Bob",
        listener.local_addr().unwrap().port(),
    );
    let (a, _, _) = node_with_files(ad.path(), alice, aa, &bp);
    let (b, _, mut offers) = node_with_files(bd.path(), bob, bob_account, &ap);
    let server = tokio::spawn(b.clone().run_accept_loop(listener));
    let source = ad.path().join("direct.bin");
    std::fs::write(&source, b"signed direct consent").unwrap();
    let (card, file) = a
        .enqueue_file_to_account(&b.account_id(), &source, crate::file::FileKind::File)
        .await
        .unwrap();
    a.flush_delivery(card).await;
    assert_eq!(offers.try_recv().unwrap().file_conv, file);
    a.flush_file_delivery(card).await;
    b.decide_attachment(file, true).unwrap();
    a.file_consent
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    a.flush_file_delivery(card).await;
    assert!(
        b.read_file(file).is_err(),
        "failed consent persistence must not downgrade to automatic legacy transfer"
    );
    a.file_consent
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    a.flush_file_delivery(card).await;
    let manifest = a
        .delivery
        .lock()
        .unwrap()
        .file_card(card)
        .unwrap()
        .destinations[0]
        .binding
        .event_id;
    assert!(
        a.file_consent
            .lock()
            .unwrap()
            .received_control(manifest)
            .is_some(),
        "direct consent must establish the same signed watermark as mailbox consent"
    );
    assert_eq!(b.read_file(file).unwrap(), b"signed direct consent");
    server.abort();
}
