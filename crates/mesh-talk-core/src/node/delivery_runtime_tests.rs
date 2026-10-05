use super::*;
use crate::discovery::{roster::Roster, Announce};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, sync::mpsc};

#[test]
fn pending_accepted_intent_precedes_generic_own_sequence_allocation() {
    pending_accepted_intent_precedes_own_event(false);
}

#[test]
fn pending_accepted_intent_precedes_own_author_sync_backfill() {
    pending_accepted_intent_precedes_own_event(true);
}

fn pending_accepted_intent_precedes_own_event(backfill: bool) {
    use super::delivery_store::{DeliveryDestination, DeliveryTransaction, OutgoingDelivery};
    use crate::eventlog::{sync::SyncStore, Event, EventKind};
    {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let ba = Account::generate();
        let bp = Announce::new_with_account(&bob, &ba, "Bob", 9);
        let conv = super::conversation::dm_conversation_id(&alice.public(), &bob.public());
        let competing = Event::new(&alice, conv, 1, vec![], 1, 11, EventKind::React, vec![2]);
        let (a, _) = node(dir.path(), alice, Account::generate(), &bp);
        let (wire, prepared) = a
            .dm_ratchet
            .lock()
            .unwrap()
            .prepare_encrypt(&a.identity, &bob.public(), &[1])
            .unwrap();
        let original = Event::new(
            &a.identity,
            conv,
            1,
            vec![],
            1,
            10,
            EventKind::Message,
            wire,
        );
        a.delivery
            .lock()
            .unwrap()
            .begin(DeliveryTransaction::Outgoing {
                message: OutgoingDelivery {
                    logical_id: original.id,
                    sender_account: a.account_id(),
                    recipient_account: Some(ba.account_id()),
                    conversation: conv,
                    wall_clock: 10,
                    destinations: vec![DeliveryDestination {
                        device: bob.public(),
                        account: Some(ba.account_id()),
                        event: original.clone(),
                        receipt_eligible: true,
                    }],
                },
                sent: super::sentlog::SentEntry {
                    conversation: conv,
                    seq: 1,
                    wall_clock: 10,
                    plaintext: vec![1],
                },
                ratchets: vec![prepared],
            })
            .unwrap();
        if backfill {
            assert!(a
                .sync_store(&bob.public())
                .lock()
                .unwrap()
                .ingest(competing)
                .is_err());
        } else {
            assert_eq!(a.append_event(conv, EventKind::React, vec![2]).unwrap(), 2);
        }
        let mut delivery = a.delivery.lock().unwrap();
        a.recover_delivery(&mut delivery).unwrap();
        assert!(delivery.message(original.id).is_some());
        assert!(a
            .log
            .lock()
            .unwrap()
            .events(&conv)
            .iter()
            .any(|e| e.id == original.id));
    }
}

fn node(
    dir: &Path,
    identity: DeviceIdentity,
    account: Account,
    proof: &Announce,
) -> (Arc<Node>, mpsc::UnboundedReceiver<ReceivedDm>) {
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &identity.public().user_id(),
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    (
        Node::open_with_account(
            identity,
            account,
            roster,
            tx,
            ch,
            file,
            &dir.join("events.log"),
            &dir.join("sent.log"),
            "pw",
        )
        .unwrap(),
        rx,
    )
}

#[tokio::test]
async fn automatic_receipt_roundtrip_with_both_original_peers_stopped_in_turn() {
    automatic_offline_roundtrip(false).await;
}

#[tokio::test]
async fn automatic_receipt_reuses_exact_ack_after_relay_eviction() {
    automatic_offline_roundtrip(true).await;
}

#[tokio::test]
async fn private_post_office_projects_authorized_other_peer_receipt_pairs_only() {
    use crate::eventlog::{sync::SyncStore, Event, EventKind};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let po = DeviceIdentity::generate();
    let pa = Account::generate();
    let pp = Announce::new_post_office_with_account(&po, &pa, "PO", 9);
    let (p, _) = node(dir.path(), po, pa, &ap);
    p.roster
        .lock()
        .unwrap()
        .update(&bp, IpAddr::V4(Ipv4Addr::LOCALHOST), &p.user_id());
    p.configure_privacy(
        dir.path(),
        "pw",
        &pp,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    p.set_allowed(&aa.account_id(), true).await.unwrap();
    p.set_invisible(true).await.unwrap();
    let dm = super::conversation::dm_conversation_id(&alice.public(), &bob.public());
    let original = Event::new(&alice, dm, 1, vec![], 1, 10, EventKind::Message, vec![1]);
    let received = super::received_log::ReceivedEntry {
        event_id: original.id,
        conversation: dm,
        from: alice.public().user_id(),
        wall_clock: 10,
        plaintext: b"message".to_vec(),
    };
    let payload = super::delivery_receipt::ReceiptPayload::prepare(
        &bob.public(),
        &ba.account_id(),
        &ap,
        &original,
        &received,
        11,
    )
    .unwrap();
    let control = super::delivery_receipt::delivery_conversation_id(&alice.public(), &bob.public());
    let ack = Event::new(
        &bob,
        control,
        1,
        vec![],
        1,
        11,
        EventKind::Message,
        payload.seal(&bob).unwrap(),
    );
    assert!(p
        .sync_store(&alice.public())
        .lock()
        .unwrap()
        .ingest(ack.clone())
        .is_err());
    p.set_allowed(&ba.account_id(), true).await.unwrap();
    assert!(p
        .sync_store(&alice.public())
        .lock()
        .unwrap()
        .ingest(ack.clone())
        .is_ok());
    assert_eq!(
        p.sync_store(&bob.public())
            .lock()
            .unwrap()
            .event_ids(&control),
        vec![ack.id]
    );
    assert!(p
        .sync_store(&alice.public())
        .lock()
        .unwrap()
        .durable_have(&control, &ack.id));
    let outsider = DeviceIdentity::generate();
    let forged = Event::new(
        &outsider,
        control,
        1,
        vec![],
        2,
        12,
        EventKind::Message,
        ack.ciphertext.clone(),
    );
    assert!(p
        .sync_store(&alice.public())
        .lock()
        .unwrap()
        .ingest(forged)
        .is_err());
    p.set_allowed(&ba.account_id(), false).await.unwrap();
    assert!(p
        .sync_store(&alice.public())
        .lock()
        .unwrap()
        .event_ids(&control)
        .is_empty());
    assert!(!p
        .sync_store(&alice.public())
        .lock()
        .unwrap()
        .durable_have(&control, &ack.id));
}

#[tokio::test]
async fn automatic_worker_stalled_fanout_does_not_starve_other_messages_and_drop_cancels() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let extra = DeviceIdentity::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let ep = Announce::new_with_account(
        &extra,
        &aa,
        "Stalled own device",
        stalled.local_addr().unwrap().port(),
    );
    let (a, _) = node(ad.path(), alice, aa, &bp);
    a.roster
        .lock()
        .unwrap()
        .update(&ep, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
    let first = a
        .enqueue_to_account(&b.account_id(), b"first fair", None)
        .await
        .unwrap();
    let second = a
        .enqueue_to_account(&b.account_id(), b"second fair", None)
        .await
        .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
        while a.delivery_status(first) != Some(DeliveryStatus::Delivered)
            || a.delivery_status(second) != Some(DeliveryStatus::Delivered)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(b.account_history(&a.account_id(), 10).len(), 2);
    at.abort();
    let _ = at.await;
    let third = a
        .enqueue_to_account(&b.account_id(), b"worker stopped", None)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(700), rx.recv())
            .await
            .is_err()
    );
    assert_eq!(a.delivery_status(third), Some(DeliveryStatus::Awaiting));
    bt.abort();
    let _ = bt.await;
}

#[tokio::test]
async fn dormant_receipt_full_binding_and_local_clear_survive_restart_in_public_and_private() {
    use crate::eventlog::sync::SyncStore;
    for private in [false, true] {
        for clear in [false, true] {
            let ad = tempfile::tempdir().unwrap();
            let bd = tempfile::tempdir().unwrap();
            let alice = DeviceIdentity::generate();
            let aa = Account::generate();
            let akeys = alice.secret_bytes();
            let own = aa.account_id();
            let bob = DeviceIdentity::generate();
            let ba = Account::generate();
            let bkeys = bob.secret_bytes();
            let baccount = ba.secret_bytes();
            let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
            let bp = Announce::new_with_account(&bob, &ba, "Bob", 9);
            let (a, _) = node(ad.path(), alice, aa, &bp);
            let (b, mut rx) = node(bd.path(), bob, ba, &ap);
            b.configure_privacy(
                bd.path(),
                "pw",
                &bp,
                Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
            )
            .unwrap();
            b.set_allowed(&own, true).await.unwrap();
            b.set_invisible(private).await.unwrap();
            let id = a
                .enqueue_to_account(&b.account_id(), b"retained source", None)
                .await
                .unwrap();
            let original = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
                .event
                .clone();
            b.log
                .lock()
                .unwrap()
                .append_durable(original.clone())
                .unwrap();
            b.emit_new_messages(original.conversation_id);
            assert_eq!(rx.try_recv().unwrap().text, b"retained source");
            let receipt = b.delivery.lock().unwrap().retry_receipts(1)[0].clone();
            // Focused projection test installs the qualified-custody store stage;
            // automatic transport qualification is tested by real loopbacks.
            b.delivery
                .lock()
                .unwrap()
                .finish_receipt(receipt.conversation, original.id)
                .unwrap();
            drop(b);
            let (b, _) = node(
                bd.path(),
                DeviceIdentity::from_secret_bytes(bkeys.0, bkeys.1),
                Account::from_secret_bytes(baccount),
                &ap,
            );
            b.configure_privacy(
                bd.path(),
                "pw",
                &bp,
                Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
            )
            .unwrap();
            let control = receipt.destination.event.conversation_id;
            assert_eq!(
                b.sync_store(&ap.public())
                    .lock()
                    .unwrap()
                    .event_ids(&control),
                vec![receipt.destination.event.id]
            );
            if clear {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let accepting = a.clone();
                let server = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    accepting.privacy_accept(stream).await.unwrap()
                });
                let mut channel = b.privacy_dial(addr, &ap.public()).await.unwrap();
                let mut receiver = server.await.unwrap();
                b.prune_older_than(0).unwrap();
                b.delete_message(control, crate::eventlog::EventId::new([0; 32]), false)
                    .unwrap();
                channel.send(b"no-op erase keeps session").await.unwrap();
                assert_eq!(receiver.recv().await.unwrap(), b"no-op erase keeps session");
                b.clear_account_conversation(&own).unwrap();
                assert!(matches!(
                    channel.send(b"snapshotted ACK").await,
                    Err(crate::transport::TransportError::AdmissionDenied)
                ));
                assert!(b
                    .sync_store(&ap.public())
                    .lock()
                    .unwrap()
                    .event_ids(&control)
                    .is_empty());
                drop(b);
                let (b, _) = node(
                    bd.path(),
                    DeviceIdentity::from_secret_bytes(bkeys.0, bkeys.1),
                    Account::from_secret_bytes(baccount),
                    &ap,
                );
                b.configure_privacy(
                    bd.path(),
                    "pw",
                    &bp,
                    Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
                )
                .unwrap();
                assert!(b
                    .sync_store(&ap.public())
                    .lock()
                    .unwrap()
                    .event_ids(&control)
                    .is_empty());
            } else {
                if private {
                    b.set_allowed(&own, false).await.unwrap();
                    assert!(b
                        .sync_store(&ap.public())
                        .lock()
                        .unwrap()
                        .event_ids(&control)
                        .is_empty());
                    b.set_allowed(&own, true).await.unwrap();
                }
                let newaccount = Account::generate();
                let newproof = Announce::new_with_account(
                    &DeviceIdentity::from_secret_bytes(akeys.0, akeys.1),
                    &newaccount,
                    "Rebound Alice",
                    9,
                );
                b.roster.lock().unwrap().update(
                    &newproof,
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    &b.user_id(),
                );
                b.initiate_contact(&newaccount.account_id()).await.unwrap();
                assert!(b
                    .sync_store(&ap.public())
                    .lock()
                    .unwrap()
                    .event_ids(&control)
                    .is_empty());
                assert!(!b
                    .sync_store(&ap.public())
                    .lock()
                    .unwrap()
                    .durable_have(&control, &receipt.destination.event.id));
                let substituted = Announce::new_with_account(
                    &DeviceIdentity::from_secret_bytes(akeys.0, [77; 32]),
                    &newaccount,
                    "Substituted X",
                    9,
                );
                b.roster.lock().unwrap().update(
                    &substituted,
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    &b.user_id(),
                );
                assert!(b.initiate_contact(&newaccount.account_id()).await.is_err());
                assert!(b
                    .sync_store(&substituted.public())
                    .lock()
                    .unwrap()
                    .event_ids(&control)
                    .is_empty());
            }
        }
    }
}

async fn automatic_offline_roundtrip(evict: bool) {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let akeys = alice.secret_bytes();
    let aaccount = aa.secret_bytes();
    let bkeys = bob.secret_bytes();
    let baccount = ba.secret_bytes();
    let target = ba.account_id();
    let own = aa.account_id();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (initial_bob, _) = node(bd.path(), bob, ba, &ap);
    drop(initial_bob);
    drop(bl); // Bob's prior profile is closed and listener stopped before enqueue.
    let po = DeviceIdentity::generate();
    let pokeys = po.secret_bytes();
    let pl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pp = Announce::new_post_office(&po, "Relay", pl.local_addr().unwrap().port());
    let relay = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(&pd.path().join("relay"), "pw", po).unwrap(),
    ));
    let mut pt = Some(tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(pokeys.0, pokeys.1),
        pl,
        relay.clone(),
    )));
    let (a, _) = node(ad.path(), alice, aa, &bp);
    a.roster
        .lock()
        .unwrap()
        .update(&pp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let id = a
        .enqueue_to_account(&target, b"strict automatic offline", None)
        .await
        .unwrap();
    let original = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .id;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !relay.lock().unwrap().has(&original) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    at.abort();
    let _ = at.await;
    drop(a); // Alice stays stopped for ALL Bob recovery.
    let bl = TcpListener::bind(("127.0.0.1", bp.tcp_port)).await.unwrap();
    let (b, mut brx) = node(
        bd.path(),
        DeviceIdentity::from_secret_bytes(bkeys.0, bkeys.1),
        Account::from_secret_bytes(baccount),
        &ap,
    );
    b.roster
        .lock()
        .unwrap()
        .update(&pp, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    let mut bt = Some(tokio::spawn(b.clone().run_accept_loop(bl)));
    let received = tokio::time::timeout(std::time::Duration::from_secs(5), brx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.text, b"strict automatic offline");
    assert_eq!(b.account_history(&own, 10).len(), 1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !b.delivery.lock().unwrap().retry_receipts(1).is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(brx.try_recv().is_err());
    let control =
        super::delivery_receipt::delivery_conversation_id(&b.identity.public(), &ap.public());
    let ack = b.log.lock().unwrap().events(&control)[0].clone();
    if evict {
        let mut po = relay.lock().unwrap();
        po.set_retention_cap(0, 0);
        po.accept(crate::eventlog::Event::new(
            &b.identity,
            crate::eventlog::ConversationId::new([91; 32]),
            1,
            vec![],
            1,
            0,
            crate::eventlog::EventKind::Message,
            vec![1],
        ))
        .unwrap();
        assert!(!po.has(&ack.id));
        po.set_retention_cap(100_000, 100);
    }
    if evict {
        let task = pt.take().unwrap();
        task.abort();
        let _ = task.await;
    }
    let b = if evict {
        Some(b)
    } else {
        let task = bt.take().unwrap();
        task.abort();
        let _ = task.await;
        drop(b);
        None // Bob fully stops before Alice returns in strict isolation.
    };
    let al = TcpListener::bind(("127.0.0.1", ap.tcp_port)).await.unwrap();
    let (a, mut arx) = node(
        ad.path(),
        DeviceIdentity::from_secret_bytes(akeys.0, akeys.1),
        Account::from_secret_bytes(aaccount),
        &bp,
    );
    a.roster
        .lock()
        .unwrap()
        .update(&pp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(a.account_history(&target, 10)[0].id, id);
    assert_eq!(a.account_history(&target, 10).len(), 1);
    assert!(arx.try_recv().is_err());
    if let Some(b) = b {
        assert_eq!(b.log.lock().unwrap().events(&control), vec![&ack]);
    }
    assert!(brx.try_recv().is_err());
    at.abort();
    let _ = at.await;
    if let Some(task) = bt {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = pt {
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn durable_raw_send_offline_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let proof = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    assert!(a
        .send_dm(&bob.public().user_id(), b"offline intent")
        .await
        .is_ok());
}

#[tokio::test]
async fn real_post_office_acceptance_keeps_certified_account_delivery_awaiting() {
    let dir = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp =
        Announce::new_with_account(&bob, &ba, "Offline Bob", dead.local_addr().unwrap().port());
    drop(dead);
    let po = DeviceIdentity::generate();
    let po_secret = po.secret_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proof = Announce::new_post_office(&po, "Relay", listener.local_addr().unwrap().port());
    let relay = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(&pd.path().join("relay"), "pw", po).unwrap(),
    ));
    let task = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(po_secret.0, po_secret.1),
        listener,
        relay.clone(),
    ));
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &bp,
    );
    a.roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let id = a
        .enqueue_to_account(&ba.account_id(), b"relay is not recipient", None)
        .await
        .unwrap();
    let original = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .id;
    a.flush_delivery(id).await;
    assert!(relay.lock().unwrap().has(&original));
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn durable_enqueue_is_awaiting_without_transport_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let proof = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let secret = alice.secret_bytes();
    let acct = aa.secret_bytes();
    let target = ba.account_id();
    let (a, _) = node(dir.path(), alice, aa, &proof);
    let id = a
        .enqueue_to_account(&target, b"durable offline", None)
        .await
        .unwrap();
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.account_history(&target, 10)[0].id, id);
    drop(a);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(acct),
        &proof,
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.account_history(&target, 10).len(), 1);
}

#[tokio::test]
async fn actual_durable_receive_sends_control_receipt_without_chat_callback() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let target = ba.account_id();
    let own = aa.account_id();
    let sender_secret = alice.secret_bytes();
    let sender_account = aa.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, mut arx) = node(ad.path(), alice, aa, &bp);
    let (b, mut brx) = node(bd.path(), bob, ba, &ap);
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let id = a
        .enqueue_to_account(&target, b"confirmed", None)
        .await
        .unwrap();
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    tokio::time::timeout(std::time::Duration::from_secs(3), brx.recv())
        .await
        .unwrap()
        .unwrap();
    for _ in 0..100 {
        if a.delivery_status(id) == Some(DeliveryStatus::Delivered) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    assert_eq!(b.account_history(&own, 10)[0].text, b"confirmed");
    for _ in 0..200 {
        if b.delivery.lock().unwrap().retry_receipts(1).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        b.delivery.lock().unwrap().retry_receipts(1).is_empty(),
        "qualified durable custody retires receipt work"
    );
    assert!(arx.try_recv().is_err());
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    drop(a);
    drop(b);
    let (a, mut rx) = node(
        ad.path(),
        DeviceIdentity::from_secret_bytes(sender_secret.0, sender_secret.1),
        Account::from_secret_bytes(sender_account),
        &bp,
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    let control = super::delivery_receipt::delivery_conversation_id(&ap.public(), &bp.public());
    assert!(!a.log.lock().unwrap().events(&control).is_empty());
    a.emit_new_messages(control);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn malformed_routes_and_control_scopes_never_advance_live_ratchet_or_emit() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
    let before = std::fs::read(bd.path().join("ratchet.sessions")).unwrap();
    let conv = super::conversation::dm_conversation_id(&a.identity.public(), &b.identity.public());
    for (scope, body) in [
        (conv, b"MTDE1garbage".to_vec()),
        (
            conv,
            DmEnvelope::new(
                a.account_id(),
                "f".repeat(32),
                [1; 32],
                b"wrong recipient".to_vec(),
            )
            .encode(),
        ),
        (
            super::delivery_receipt::delivery_conversation_id(
                &a.identity.public(),
                &b.identity.public(),
            ),
            MessageBody::new(b"control is not chat".to_vec(), None).encode(),
        ),
    ] {
        let wire = a
            .dm_ratchet
            .lock()
            .unwrap()
            .encrypt(&a.identity, &b.identity.public(), &body)
            .unwrap();
        let event = a
            .append_event(scope, crate::eventlog::EventKind::Message, wire)
            .unwrap();
        let signed = a
            .log
            .lock()
            .unwrap()
            .events(&scope)
            .into_iter()
            .find(|e| e.seq == event)
            .unwrap()
            .clone();
        b.log.lock().unwrap().append_durable(signed).unwrap();
        b.emit_new_messages(scope);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            std::fs::read(bd.path().join("ratchet.sessions")).unwrap(),
            before
        );
        assert!(b.delivery.lock().unwrap().retry_receipts(8).is_empty());
    }
}

#[tokio::test]
async fn accepted_install_failure_keeps_stable_history_and_blocks_later_ratchets() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let target = ba.account_id();
    let proof = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let sidecar_dir = dir.path().join("sidecar");
    let moved = dir.path().join("moved");
    *a.sentlog.lock().unwrap() =
        super::sentlog::SentLog::open(&sidecar_dir.join("sent"), "pw").unwrap();
    std::fs::rename(&sidecar_dir, &moved).unwrap();
    let id = a
        .enqueue_to_account(&target, b"accepted once", None)
        .await
        .unwrap();
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.account_history(&target, 10).len(), 1);
    assert_eq!(a.account_history(&target, 10)[0].id, id);
    assert_eq!(a.account_history(&target, 10)[0].text, b"accepted once");
    let sessions = std::fs::read(dir.path().join("ratchet.sessions")).unwrap();
    let conv = super::conversation::dm_conversation_id(&a.identity.public(), &bob.public());
    let versions = a.log.lock().unwrap().version_vector(&conv);
    assert!(a
        .append_event(conv, crate::eventlog::EventKind::React, vec![2])
        .is_err());
    assert_eq!(a.log.lock().unwrap().version_vector(&conv), versions);
    assert!(a
        .enqueue_to_account(&target, b"must wait", None)
        .await
        .is_err());
    assert_eq!(
        std::fs::read(dir.path().join("ratchet.sessions")).unwrap(),
        sessions
    );
    let worker = tokio::spawn(a.clone().run_delivery_loop());
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert!(!a.delivery.lock().unwrap().pending_transactions().is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    std::fs::rename(&moved, &sidecar_dir).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !a.delivery.lock().unwrap().pending_transactions().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    let _ = worker.await;
    assert_eq!(a.account_history(&target, 10).len(), 1);
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
    assert_eq!(a.delete_account_message(&target, id).unwrap(), 1);
    assert_eq!(a.delivery_status(id), None);
    assert!(a.account_history(&target, 10).is_empty());
}

#[tokio::test]
async fn oversized_enqueue_rejects_before_durable_ratchet_or_history() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let proof = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let sessions = std::fs::read(dir.path().join("ratchet.sessions")).unwrap();
    assert!(a
        .enqueue_to_account(&ba.account_id(), &vec![7; 256 * 1024], None)
        .await
        .is_err());
    assert_eq!(
        std::fs::read(dir.path().join("ratchet.sessions")).unwrap(),
        sessions
    );
    assert!(a.account_history(&ba.account_id(), 10).is_empty());
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
}

#[tokio::test]
async fn failed_account_save_preserves_queue_and_successful_adoption_suspends_old_node() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let secret = alice.secret_bytes();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let proof = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let (a, _) = node(dir.path(), alice, aa, &proof);
    let id = a
        .enqueue_to_account(&ba.account_id(), b"keep on failed adoption", None)
        .await
        .unwrap();
    let new_account = Account::generate();
    let blocked_keystore = dir.path().join("blocked.keystore");
    std::fs::create_dir(&blocked_keystore).unwrap();
    assert!(a
        .persist_account_adoption(&new_account.account_id(), || {
            crate::identity::account_keystore::save(&blocked_keystore, "pw", &new_account)
                .map_err(|e| NodeError::Channel(e.to_string()))
        })
        .is_err());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.delivery.lock().unwrap().retry_messages(10).len(), 1);
    a.persist_account_adoption(&new_account.account_id(), || Ok(()))
        .unwrap();
    assert!(a
        .enqueue_to_account(&ba.account_id(), b"old runtime must stop", None)
        .await
        .is_err());
    drop(a);
    let (adopted, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        new_account,
        &proof,
    );
    assert_eq!(adopted.delivery_status(id), None);
    assert!(adopted
        .delivery
        .lock()
        .unwrap()
        .retry_messages(10)
        .is_empty());
}

#[tokio::test]
async fn adoption_marker_before_account_save_reopens_old_queue_and_unmarked_nonempty_rebind_rejects(
) {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let secret = alice.secret_bytes();
    let aa = Account::generate();
    let own_secret = aa.secret_bytes();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let proof = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let (a, _) = node(dir.path(), alice, aa, &proof);
    let id = a
        .enqueue_to_account(&ba.account_id(), b"pre-save crash", None)
        .await
        .unwrap();
    let new_account = Account::generate();
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .bind_profile(
            &dir.path().join("events.log"),
            "pw",
            &a.identity.public(),
            &new_account.account_id()
        )
        .is_err());
    a.delivery
        .lock()
        .unwrap()
        .prepare_profile_adoption(&new_account.account_id())
        .unwrap();
    drop(a);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(own_secret),
        &proof,
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.delivery.lock().unwrap().retry_messages(8).len(), 1);
    assert_eq!(a.account_history(&ba.account_id(), 8)[0].id, id);
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .bind_profile(
            &dir.path().join("events.log"),
            "pw",
            &a.identity.public(),
            &new_account.account_id()
        )
        .is_err());
}

#[test]
fn same_directory_delivery_logs_are_isolated_and_foreign_owner_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let mut sa =
        super::delivery_store::DeliveryStore::open_for_log(&dir.path().join("a.log"), "pw")
            .unwrap();
    let mut sb =
        super::delivery_store::DeliveryStore::open_for_log(&dir.path().join("b.log"), "pw")
            .unwrap();
    sa.bind_profile(
        &dir.path().join("a.log"),
        "pw",
        &a.public(),
        &"a".repeat(32),
    )
    .unwrap();
    sb.bind_profile(
        &dir.path().join("b.log"),
        "pw",
        &b.public(),
        &"b".repeat(32),
    )
    .unwrap();
    drop(sa);
    drop(sb);
    let mut reopened =
        super::delivery_store::DeliveryStore::open_for_log(&dir.path().join("a.log"), "pw")
            .unwrap();
    assert!(reopened
        .bind_profile(
            &dir.path().join("a.log"),
            "pw",
            &b.public(),
            &"a".repeat(32)
        )
        .is_err());
    assert!(reopened
        .bind_profile(
            &dir.path().join("a.log"),
            "pw",
            &a.public(),
            &"a".repeat(32)
        )
        .is_ok());
}

#[tokio::test]
async fn receiver_write_failure_keeps_plaintext_intent_until_restart_and_no_early_receipt() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let secret = bob.secret_bytes();
    let account = ba.secret_bytes();
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
    let id = a
        .enqueue_to_account(&b.account_id(), b"recover recipient", None)
        .await
        .unwrap();
    let event = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    let sidecar = bd.path().join("sidecar");
    let moved = bd.path().join("moved");
    *b.received.lock().unwrap() = ReceivedLog::open(&sidecar.join("received"), "pw").unwrap();
    std::fs::rename(&sidecar, &moved).unwrap();
    b.emit_new_messages(event.conversation_id);
    assert!(rx.try_recv().is_err());
    assert!(b.delivery.lock().unwrap().retry_receipts(10).is_empty());
    assert_eq!(b.delivery.lock().unwrap().pending_transactions().len(), 1);
    std::fs::rename(&moved, &sidecar).unwrap();
    drop(b);
    let (b, mut rx) = node(
        bd.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account),
        &ap,
    );
    assert_eq!(
        b.account_history(&a.account_id(), 10)[0].text,
        b"recover recipient"
    );
    assert_eq!(b.delivery.lock().unwrap().retry_receipts(10).len(), 1);
    b.emit_new_messages(event.conversation_id);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn early_recipient_receipt_preserves_offline_own_fanout_across_sender_restart() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let cd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let other = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let secret = alice.secret_bytes();
    let account = aa.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let cp =
        Announce::new_with_account(&other, &aa, "Other Alice", cl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    a.roster
        .lock()
        .unwrap()
        .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let (b, mut brx) = node(bd.path(), bob, ba, &ap);
    let (c, mut crx) = node(cd.path(), other, Account::from_secret_bytes(account), &ap);
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let id = a
        .enqueue_to_account(&b.account_id(), b"fanout remains", None)
        .await
        .unwrap();
    let bob_peer = a.routing_peer(&b.user_id()).unwrap();
    let conv = super::conversation::dm_conversation_id(&a.identity.public(), &b.identity.public());
    a.deliver_direct(&bob_peer, conv).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), brx.recv())
        .await
        .unwrap()
        .unwrap();
    b.flush_delivery_receipts(8).await;
    for _ in 0..100 {
        if a.delivery_status(id) == Some(DeliveryStatus::Delivered) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    drop(a);
    drop(b);
    let (a, _) = node(
        ad.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account),
        &bp,
    );
    a.roster
        .lock()
        .unwrap()
        .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let ct = tokio::spawn(c.clone().run_accept_loop(cl));
    a.flush_delivery(id).await;
    let received = tokio::time::timeout(std::time::Duration::from_secs(1), crx.recv()).await;
    assert!(
        received.is_ok(),
        "Delivered must retain unfinished own-device copy after restart"
    );
    assert!(c.delivery.lock().unwrap().retry_receipts(8).is_empty());
    ct.abort();
    let _ = ct.await;
}

#[tokio::test]
async fn private_control_receipt_obeys_current_permission_and_queued_account_binding() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let bob_secret = bob.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, mut arx) = node(ad.path(), alice, aa, &bp);
    let (b, mut brx) = node(bd.path(), bob, ba, &ap);
    a.configure_privacy(
        ad.path(),
        "pw",
        &ap,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    b.configure_privacy(
        bd.path(),
        "pw",
        &bp,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    a.set_allowed(&b.account_id(), true).await.unwrap();
    b.set_allowed(&a.account_id(), true).await.unwrap();
    a.set_invisible(true).await.unwrap();
    b.set_invisible(true).await.unwrap();
    let id = a
        .enqueue_to_account(&b.account_id(), b"private accepted", None)
        .await
        .unwrap();
    a.flush_delivery(id).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), brx.recv())
        .await
        .unwrap()
        .unwrap();
    b.flush_delivery_receipts(8).await;
    for _ in 0..100 {
        if a.delivery_status(id) == Some(DeliveryStatus::Delivered) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    assert!(arx.try_recv().is_err());
    // Synthetic queued revoke/rebind checks must not race the automatic worker.
    at.abort();
    let _ = at.await;
    let blocked = a
        .enqueue_to_account(&b.account_id(), b"revoked before transfer", None)
        .await
        .unwrap();
    a.set_allowed(&b.account_id(), false).await.unwrap();
    a.flush_delivery(blocked).await;
    assert!(brx.try_recv().is_err());
    assert_eq!(a.delivery_status(blocked), Some(DeliveryStatus::Awaiting));
    let bound = a
        .delivery
        .lock()
        .unwrap()
        .message(blocked)
        .unwrap()
        .destinations[0]
        .clone();
    a.set_allowed(&b.account_id(), true).await.unwrap();
    let new_account = Account::generate();
    let new_proof = Announce::new_with_account(
        &DeviceIdentity::from_secret_bytes(bob_secret.0, bob_secret.1),
        &new_account,
        "Rebound Bob",
        bp.tcp_port,
    );
    a.roster
        .lock()
        .unwrap()
        .update(&new_proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    a.initiate_contact(&new_account.account_id()).await.unwrap();
    assert!(!a.delivery_destination_allowed(&bound));
    a.flush_delivery(blocked).await;
    assert!(brx.try_recv().is_err());
    assert_eq!(a.delivery_status(blocked), Some(DeliveryStatus::Awaiting));
    bt.abort();
    let _ = bt.await;
}

#[tokio::test]
async fn private_receipts_remain_projectable_after_clear_restart_and_account_adoption() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let bob_secret = bob.secret_bytes();
    let bob_account = ba.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
    a.configure_privacy(
        ad.path(),
        "pw",
        &ap,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    b.configure_privacy(
        bd.path(),
        "pw",
        &bp,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    a.set_allowed(&b.account_id(), true).await.unwrap();
    b.set_allowed(&a.account_id(), true).await.unwrap();
    a.set_invisible(true).await.unwrap();
    b.set_invisible(true).await.unwrap();
    let first = a
        .enqueue_to_account(&b.account_id(), b"before clear", None)
        .await
        .unwrap();
    a.flush_delivery(first).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    b.flush_delivery_receipts(8).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(a.delivery_status(first), Some(DeliveryStatus::Delivered));
    b.clear_account_conversation(&a.account_id()).unwrap();
    bt.abort();
    let _ = bt.await;
    drop(b);
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob = DeviceIdentity::from_secret_bytes(bob_secret.0, bob_secret.1);
    let ba = Account::from_secret_bytes(bob_account);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
    b.configure_privacy(
        bd.path(),
        "pw",
        &bp,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    a.roster
        .lock()
        .unwrap()
        .update(&bp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let second = a
        .enqueue_to_account(&b.account_id(), b"after clear restart", None)
        .await
        .unwrap();
    a.flush_delivery(second).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    b.flush_delivery_receipts(8).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(a.delivery_status(second), Some(DeliveryStatus::Delivered));
    let adopted = Account::generate();
    b.persist_account_adoption(&adopted.account_id(), || {
        crate::identity::account_keystore::save(&bd.path().join("account.keystore"), "pw", &adopted)
            .map_err(|e| NodeError::Channel(e.to_string()))
    })
    .unwrap();
    bt.abort();
    let _ = bt.await;
    drop(b);
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob = DeviceIdentity::from_secret_bytes(bob_secret.0, bob_secret.1);
    let bp = Announce::new_with_account(
        &bob,
        &adopted,
        "Adopted Bob",
        bl.local_addr().unwrap().port(),
    );
    let (b, mut rx) = node(bd.path(), bob, adopted, &ap);
    b.configure_privacy(
        bd.path(),
        "pw",
        &bp,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    a.roster
        .lock()
        .unwrap()
        .update(&bp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    a.initiate_contact_locally(&b.account_id()).await.unwrap();
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let third = a
        .enqueue_to_account(&b.account_id(), b"after account adoption", None)
        .await
        .unwrap();
    a.flush_delivery(third).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    b.flush_delivery_receipts(8).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(a.delivery_status(third), Some(DeliveryStatus::Delivered));
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
}
