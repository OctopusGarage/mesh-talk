//! End-to-end file control and byte delivery through federated opaque mailboxes.
use super::test_support::node_with_files;
use crate::discovery::Announce;
use crate::eventlog::{ConversationId, EventId};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

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
    let ba = Account::generate();
    let bob_keys = bob.secret_bytes();
    // Neither client has a listener. Only the two relay endpoints are reachable.
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", 1);
    let (a, mut messages, _) = node_with_files(ad.path(), alice, aa, &bp);
    let (b, _, mut offers) = node_with_files(bd.path(), bob, ba, &ap);
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
    b.decide_attachment(file, true).unwrap();
    b.drain_from_post_office().await;
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
    a.drain_from_post_office().await;
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
    assert!(!a.log.lock().unwrap().has(&control.id));
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
    directory
        .sync_with_relay(qaddr, &p, &q.public(), &ps)
        .await
        .unwrap();
    b.drain_from_post_office().await;
    assert_eq!(b.read_file(file).unwrap(), bytes);
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
    b.delivery
        .lock()
        .unwrap()
        .prepare_profile_adoption(&adopted_account.account_id())
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
}

#[tokio::test]
async fn direct_file_approval_shares_signed_revision_with_relay_delivery() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", listener.local_addr().unwrap().port());
    let (a, _, _) = node_with_files(ad.path(), alice, aa, &bp);
    let (b, _, mut offers) = node_with_files(bd.path(), bob, ba, &ap);
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
