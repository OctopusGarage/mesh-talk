use super::*;
use crate::discovery::Announce;
use crate::eventlog::{ConversationId, EventId, LogError};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, sync::mpsc};

#[tokio::test]
async fn production_node_tcp_sockets_enable_keepalive() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", addr.port());
    let (sender, _) = node(ad.path(), alice, aa, &bp);
    let (recipient, _) = node(bd.path(), bob, bob_account, &ap);
    let accept = async {
        let (stream, _) = listener.accept().await.unwrap();
        recipient.privacy_accept(stream).await.unwrap()
    };
    let expected = bp.public();
    let (outbound, inbound) = tokio::join!(sender.privacy_dial(addr, &expected), accept);
    assert!(outbound.unwrap().tcp_keepalive_enabled());
    assert!(inbound.tcp_keepalive_enabled());
}

#[tokio::test]
async fn durable_dm_retries_a_reused_socket_closed_after_checkout() {
    let sender_dir = tempfile::tempdir().unwrap();
    let recipient_dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let alice_account = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ap = Announce::new_with_account(&alice, &alice_account, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", addr.port());
    let (sender, _) = node(sender_dir.path(), alice, alice_account, &bp);
    let (recipient, _) = node(recipient_dir.path(), bob, bob_account, &ap);
    for (node, dir, own) in [
        (&sender, sender_dir.path(), &ap),
        (&recipient, recipient_dir.path(), &bp),
    ] {
        node.configure_privacy(
            dir,
            "pw",
            own,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    let id = sender
        .enqueue_to_account(
            &bp.account_id().unwrap(),
            b"recover without worker backoff",
            None,
        )
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .clone();
    let peer = sender
        .roster
        .lock()
        .unwrap()
        .get(&recipient.user_id())
        .unwrap()
        .clone();
    let responder = recipient.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = responder.privacy_accept(stream).await.unwrap();
        let store = responder.sync_store(channel.peer_identity());
        // A matched empty fingerprint completes the warm-up exchange. Close
        // only after the next request arrives, beyond the checkout probe.
        super::session::serve_one(&mut channel, &store)
            .await
            .unwrap();
        channel.recv().await.unwrap();
        drop(channel);
        let (stream, _) = listener.accept().await.unwrap();
        let channel = responder.privacy_accept(stream).await.unwrap();
        responder.serve_connection(channel).await;
    });
    let mut warm = sender.privacy_sync_dial(addr, &bp.public()).await.unwrap();
    super::session::request_round(
        &mut warm,
        &sender.sync_store(&bp.public()),
        ConversationId::new([219; 32]),
    )
    .await
    .unwrap();
    sender.recycle_sync_channel(warm);
    let held = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        sender.transfer_delivery_destination(
            &peer,
            &destination,
            destination.event.conversation_id,
        ),
    )
    .await
    .unwrap();
    server.abort();
    assert!(
        held,
        "stale pooled socket must recover in this delivery attempt"
    );
    assert!(
        recipient
            .log
            .lock()
            .unwrap()
            .has(&destination.event.id)
            .unwrap()
    );
}

#[tokio::test]
async fn confirmed_destination_does_not_open_another_relay_connection() {
    let dir = tempfile::tempdir().unwrap();
    let recipient = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_with_account(&recipient, &account, "recipient", 9);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = DeviceIdentity::generate();
    let relay_proof = Announce::new_post_office_with_account(
        &relay,
        &Account::generate(),
        "relay",
        listener.local_addr().unwrap().port(),
    );
    sender.roster.lock().unwrap().update(
        &relay_proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &sender.user_id(),
    );
    let id = sender
        .enqueue_to_account(&account.account_id(), b"already confirmed", None)
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .clone();
    // This store operation is invoked only after exact authenticated receipt
    // validation; its authorization is exercised by the existing receipt tests.
    sender
        .delivery
        .lock()
        .unwrap()
        .mark_delivered_for(id, destination.event.id)
        .unwrap();
    assert_eq!(sender.delivery_status(id), Some(DeliveryStatus::Delivered));
    let (result, incoming) = tokio::join!(
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            sender.replicate_delivery_destination(&destination)
        ),
        tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()),
    );
    assert!(
        incoming.is_err(),
        "confirmed destination opened an unnecessary relay connection"
    );
    assert!(matches!(result, Ok(false)));
}

/// Hold exact receipts for a separate authenticated connection, while still
/// accepting messages and confirming their durable storage on the direct path.
struct DeferredReceiptStore<S> {
    inner: S,
    control: ConversationId,
    control_seen: Arc<std::sync::atomic::AtomicBool>,
}

impl<S: crate::eventlog::sync::SyncStore> crate::eventlog::sync::SyncStore
    for DeferredReceiptStore<S>
{
    fn admission_denied(&self) -> bool {
        self.inner.admission_denied()
    }
    fn durable_have(
        &self,
        conversation: &ConversationId,
        id: &EventId,
    ) -> Result<bool, crate::eventlog::LogError> {
        self.inner.durable_have(conversation, id)
    }
    fn event_ids(
        &self,
        conversation: &ConversationId,
    ) -> Result<Vec<EventId>, crate::eventlog::LogError> {
        if *conversation == self.control {
            self.control_seen
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Vec::new())
        } else {
            self.inner.event_ids(conversation)
        }
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &std::collections::HashSet<EventId>,
    ) -> Result<Vec<crate::eventlog::Event>, crate::eventlog::LogError> {
        if *conversation == self.control {
            Ok(Vec::new())
        } else {
            self.inner.events_excluding(conversation, have)
        }
    }
    fn ingest(
        &mut self,
        event: crate::eventlog::Event,
    ) -> Result<crate::eventlog::AppendOutcome, crate::eventlog::LogError> {
        self.inner.ingest(event)
    }
}

#[tokio::test]
async fn late_exact_receipt_releases_peer_lane_before_a_stalled_relay_finishes() {
    use std::{sync::atomic::Ordering, time::Duration};
    let sender_dir = tempfile::tempdir().unwrap();
    let recipient_dir = tempfile::tempdir().unwrap();
    let relay_dir = tempfile::tempdir().unwrap();
    let sender_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let recipient_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sender_identity = DeviceIdentity::generate();
    let recipient_identity = DeviceIdentity::generate();
    let relay_identity = DeviceIdentity::generate();
    let sender_account = Account::generate();
    let recipient_account = Account::generate();
    let relay_account = Account::generate();
    let sender_proof = Announce::new_with_account(
        &sender_identity,
        &sender_account,
        "sender",
        sender_listener.local_addr().unwrap().port(),
    );
    let recipient_address = recipient_listener.local_addr().unwrap();
    let recipient_proof = Announce::new_with_account(
        &recipient_identity,
        &recipient_account,
        "recipient",
        recipient_address.port(),
    );
    let relay_proof = Announce::new_post_office_with_account(
        &relay_identity,
        &relay_account,
        "held relay",
        relay_listener.local_addr().unwrap().port(),
    );
    let (sender, _) = node(
        sender_dir.path(),
        sender_identity,
        sender_account,
        &recipient_proof,
    );
    let (recipient, _) = node(
        recipient_dir.path(),
        recipient_identity,
        recipient_account,
        &sender_proof,
    );
    let (relay, _) = node(
        relay_dir.path(),
        relay_identity,
        relay_account,
        &sender_proof,
    );
    for (node, dir, own) in [
        (&sender, sender_dir.path(), &sender_proof),
        (&recipient, recipient_dir.path(), &recipient_proof),
    ] {
        node.configure_privacy(
            dir,
            "pw",
            own,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    relay
        .configure_privacy(
            relay_dir.path(),
            "pw",
            &relay_proof,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    sender.roster.lock().unwrap().update(
        &relay_proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &sender.user_id(),
    );
    let control = super::delivery_receipt::delivery_conversation_id(
        &sender.identity.public(),
        &recipient.identity.public(),
    );
    let control_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // JoinSet owns every listener, socket and worker, including assertion failure.
    let mut tasks = tokio::task::JoinSet::new();
    let receiver = recipient.clone();
    let seen = control_seen.clone();
    tasks.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = recipient_listener.accept().await.unwrap();
            let node = receiver.clone();
            let seen = seen.clone();
            connections.spawn(async move {
                let Ok(mut channel) = node.privacy_accept(stream).await else {
                    return;
                };
                let store = Mutex::new(DeferredReceiptStore {
                    inner: node
                        .sync_store(channel.peer_identity())
                        .into_inner()
                        .unwrap(),
                    control,
                    control_seen: seen,
                });
                while let Ok(super::session::Served::Handled(conversation)) =
                    super::session::serve_one(&mut channel, &store).await
                {
                    node.emit_new_messages(conversation);
                }
            });
        }
    });
    let (relay_waiting, relay_started) = tokio::sync::oneshot::channel();
    let (relay_closed, mut relay_closed_rx) = tokio::sync::oneshot::channel();
    tasks.spawn(async move {
        let (stream, _) = relay_listener.accept().await.unwrap();
        let mut channel = relay.privacy_accept(stream).await.unwrap();
        channel.recv().await.unwrap();
        let _ = relay_waiting.send(());
        // The test never releases this response. Correct delivery cancellation
        // must drop its client socket instead of waiting for a relay timeout.
        assert!(
            channel.recv().await.is_err(),
            "held relay receives client EOF on cancellation"
        );
        let _ = relay_closed.send(());
    });
    let first = sender
        .enqueue_to_account(&recipient.account_id(), b"first", None)
        .await
        .unwrap();
    tasks.spawn(sender.clone().run_delivery_loop());
    tokio::time::timeout(Duration::from_secs(5), relay_started)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if control_seen.load(Ordering::SeqCst)
                && let Some(channel) = sender
                    .checkout_reusable_channel(
                        super::session_pool::SessionProtocol::Sync,
                        recipient_address,
                        &recipient_proof.public(),
                    )
                    .await
                    .unwrap()
            {
                sender.recycle_sync_channel(channel);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("direct message and empty receipt round complete before the late receipt");
    assert_eq!(
        sender.delivery_status(first),
        Some(DeliveryStatus::Awaiting),
        "durable custody alone is not recipient delivery"
    );
    assert!(
        !recipient
            .delivery
            .lock()
            .unwrap()
            .retry_receipts(1)
            .is_empty()
    );
    sender.delivery_status_notify.notify_waiters();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut relay_closed_rx)
            .await
            .is_err(),
        "an unrelated status wake must not retire this pending destination"
    );
    sync_delivery_test_stage(&recipient, &sender, &sender_listener, control).await;
    wait_for_durable_delivery(&sender, first).await;
    let second = sender
        .enqueue_to_account(&recipient.account_id(), b"second", None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if recipient
                .account_history(&sender.account_id(), 10)
                .unwrap()
                .iter()
                .any(|row| row.id == second)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second message reaches healthy recipient while relay response stays withheld");
    tokio::time::timeout(Duration::from_secs(3), relay_closed_rx)
        .await
        .expect("retired attempt releases its stalled relay socket")
        .unwrap();
    tasks.shutdown().await;
}

async fn owner_invalidated_while_waiting_for_local_grant(kind: &str) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let alice_secret = alice.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let bob = DeviceIdentity::generate();
    let remote = Account::generate();
    let proof = Announce::new_with_account(&bob, &remote, "Bob", 1);
    let (node, _) = node(dir.path(), alice, account, &proof);
    let own = node.signed_announce("Alice", 1);
    node.configure_privacy(
        dir.path(),
        "pw",
        &own,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let snapshot = |node: &Node| {
        let state = node.privacy.state.read().unwrap();
        let state = state.as_ref().unwrap();
        (
            state.policy.snapshot(),
            state.proofs.announcements(),
            state.proofs.account_for(&proof.public()),
            state.routes.routes(),
        )
    };
    let persisted = || {
        ["privacy.policy", "peer-proofs", "peer-routes"]
            .map(|name| std::fs::read(dir.path().join(name)).unwrap())
    };
    let before = snapshot(&node);
    let before_disk = persisted();
    let producer_files = || {
        [
            "events.log",
            "sent.log",
            "ratchet.sessions",
            "received_files.log",
            "delivery-transactions.log",
            "delivery-outbox.log",
        ]
        .map(|name| match std::fs::read(dir.path().join(name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("producer snapshot read failed: {error}"),
        })
    };
    let before_work = producer_files();
    let path = dir.path().join("unstaged.txt");
    std::fs::write(&path, b"must not be staged by an invalid owner").unwrap();
    let gate = node.privacy.gate.clone().write_owned().await;
    let owner = Arc::new(Mutex::new(true));
    let progress = Arc::new(AtomicUsize::new(0));
    let (entered, observed) = tokio::sync::oneshot::channel();
    let mut entered = Some(entered);
    let worker = node.clone();
    let active = owner.clone();
    let produced = progress.clone();
    let target = remote.account_id();
    let kind = kind.to_owned();
    let pending = tokio::spawn(async move {
        let authorize = move |operation: &mut dyn FnMut() -> Result<(), NodeError>| {
            let owner = active.lock().unwrap();
            if !*owner {
                return Err(NodeError::Authorization("owner generation changed".into()));
            }
            let result = operation();
            if result.is_ok()
                && let Some(entered) = entered.take()
            {
                let _ = entered.send(());
            }
            result
        };
        match kind.as_str() {
            "text" => worker
                .enqueue_to_account_if(&target, b"pending grant", None, authorize)
                .await
                .map(|_| ()),
            "sticker" => worker
                .enqueue_sticker_to_account_if(&target, "wave", b"hello", authorize)
                .await
                .map(|_| ()),
            "file" => worker
                .enqueue_file_to_account_progress_if(
                    &target,
                    &path,
                    crate::file::FileKind::File,
                    move |_| {
                        produced.fetch_add(1, Ordering::SeqCst);
                    },
                    authorize,
                )
                .await
                .map(|_| ()),
            _ => unreachable!(),
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), observed)
        .await
        .unwrap()
        .unwrap();
    *owner.lock().unwrap() = false;
    drop(gate);
    let result = pending.await.unwrap();
    let unchanged = snapshot(&node) == before;
    let disk_unchanged = persisted() == before_disk;
    let work_unchanged = producer_files() == before_work;
    let staged = progress.load(Ordering::SeqCst);
    let log_empty = node.log.lock().unwrap().conversations().unwrap().is_empty();
    assert!(matches!(result, Err(NodeError::Authorization(_))));
    assert!(
        node.delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    assert!(
        node.delivery
            .lock()
            .unwrap()
            .next_destination(None)
            .is_none()
    );
    assert!(
        node.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_none()
    );
    assert!(
        node.account_history(&remote.account_id(), 10)
            .unwrap()
            .is_empty()
    );
    assert!(node.sentlog.lock().unwrap().conversations().is_empty());
    drop(node);
    let (reopened, _) = self::node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(alice_secret.0, alice_secret.1),
        Account::from_secret_bytes(account_secret),
        &proof,
    );
    reopened
        .configure_privacy(
            dir.path(),
            "pw",
            &own,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    eprintln!(
        "denied grant: memory_unchanged={unchanged}, disk_unchanged={disk_unchanged}, staged_callbacks={staged}, log_empty={log_empty}"
    );
    assert!(
        unchanged,
        "invalid owner must not change policy, certified binding or routes"
    );
    assert!(
        disk_unchanged,
        "invalid owner must not persist local grant metadata"
    );
    assert_eq!(snapshot(&reopened), before);
    assert!(
        work_unchanged,
        "invalid owner must not append events, ratchet state, file rows or delivery journals"
    );
    assert_eq!(persisted(), before_disk);
    assert_eq!(
        staged, 0,
        "file staging must not start after owner loss before the grant"
    );
    assert!(
        log_empty
            && reopened
                .log
                .lock()
                .unwrap()
                .conversations()
                .unwrap()
                .is_empty()
    );
    assert!(reopened.files.lock().unwrap().file_convs().is_empty());
    assert!(
        reopened
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
}

#[tokio::test]
async fn owner_invalidated_at_privacy_gate_cannot_grant_text() {
    owner_invalidated_while_waiting_for_local_grant("text").await;
}
#[tokio::test]
async fn owner_invalidated_at_privacy_gate_cannot_grant_sticker() {
    owner_invalidated_while_waiting_for_local_grant("sticker").await;
}
#[tokio::test]
async fn owner_invalidated_at_privacy_gate_cannot_grant_or_stage_file() {
    owner_invalidated_while_waiting_for_local_grant("file").await;
}

#[tokio::test]
async fn account_acceptance_does_not_keep_an_unbound_sibling_forgotten_at_privacy_gate() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let remote = Account::generate();
    let bob = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&bob, &remote, "Bob", 1);
    let (node, _) = node(dir.path(), DeviceIdentity::generate(), account, &proof);
    let sibling = DeviceIdentity::generate();
    let sibling_proof = Announce::new_with_account(&sibling, &node.account, "Sibling", 2);
    node.roster.lock().unwrap().update(
        &sibling_proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    node.configure_privacy(
        dir.path(),
        "pw",
        &node.signed_announce("Alice", 3),
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    assert!(
        node.historical_author(&sibling.public().ed25519_pub)
            .is_none()
    );
    let gate = node.privacy.gate.clone().write_owned().await;
    let (entered, observed) = tokio::sync::oneshot::channel();
    let worker = node.clone();
    let target = remote.account_id();
    let pending = tokio::spawn(async move {
        let mut entered = Some(entered);
        worker
            .enqueue_to_account_if(
                &target,
                b"current bound destinations only",
                None,
                |commit| {
                    let result = commit();
                    if result.is_ok()
                        && let Some(entered) = entered.take()
                    {
                        let _ = entered.send(());
                    }
                    result
                },
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), observed)
        .await
        .unwrap()
        .unwrap();
    node.roster.lock().unwrap().forget(&sibling.public());
    drop(gate);
    let id = pending.await.unwrap().unwrap();
    let delivery = node.delivery.lock().unwrap();
    let destinations = &delivery.message(id).unwrap().destinations;
    assert_eq!(
        destinations.len(),
        1,
        "forgotten uncached siblings must not become unbound accepted destinations"
    );
    assert_eq!(destinations[0].device, bob.public());
    assert_eq!(
        destinations[0].account.as_deref(),
        Some(remote.account_id().as_str())
    );
    assert!(
        node.historical_author(&sibling.public().ed25519_pub)
            .is_none(),
        "acceptance must not resurrect the forgotten discovery proof"
    );
}

#[tokio::test]
async fn host_enqueue_authorizes_before_privacy_and_inside_final_wal_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 1);
    let (a, _) = node(dir.path(), alice, aa, &bp);
    a.configure_privacy(
        dir.path(),
        "pw",
        &a.signed_announce("Alice", 1),
        Arc::new(crate::discovery::visibility::DiscoveryVisibility::new(
            false,
        )),
    )
    .unwrap();
    let denied = a
        .enqueue_to_account_if(
            &bob_account.account_id(),
            b"before permission",
            None,
            |_| Err(NodeError::Authorization("host replaced".into())),
        )
        .await;
    assert!(matches!(denied, Err(NodeError::Authorization(_))));
    assert!(a.privacy_snapshot().allowed_accounts.is_empty());
    let before_sessions = std::fs::read(dir.path().join("ratchet.sessions")).unwrap();
    let mut checks = 0;
    let denied = a
        .enqueue_to_account_if(&bob_account.account_id(), b"deny WAL", None, |accept| {
            checks += 1;
            if checks <= 2 {
                accept()
            } else {
                Err(NodeError::Authorization("host replaced after grant".into()))
            }
        })
        .await;
    assert!(matches!(denied, Err(NodeError::Authorization(_))));
    assert_eq!(checks, 3);
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
    assert!(a.sentlog.lock().unwrap().conversations().is_empty());
    assert!(a.log.lock().unwrap().conversations().unwrap().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("ratchet.sessions")).unwrap(),
        before_sessions
    );
    let path = dir.path().join("staged.txt");
    std::fs::write(&path, b"real staged bytes").unwrap();
    let authorized = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let during_staging = authorized.clone();
    let denied = a
        .enqueue_file_to_account_progress_if(
            &bob_account.account_id(),
            &path,
            crate::file::FileKind::File,
            move |_| during_staging.store(false, std::sync::atomic::Ordering::Release),
            |accept| {
                if authorized.load(std::sync::atomic::Ordering::Acquire) {
                    accept()
                } else {
                    Err(NodeError::Authorization(
                        "host replaced during staging".into(),
                    ))
                }
            },
        )
        .await;
    assert!(matches!(denied, Err(NodeError::Authorization(_))));
    assert!(
        a.account_history(&bob_account.account_id(), 10)
            .unwrap()
            .is_empty()
    );
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
    // Chunk staging was real, but no local file card/WAL transaction was accepted.
    assert!(!a.log.lock().unwrap().conversations().unwrap().is_empty());
}

#[tokio::test]
async fn certified_account_rekey_bounds_unadvertised_receipt_sync_without_claiming_custody() {
    use crate::eventlog::sync::SyncStore;
    use crate::transport::SecureChannel;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let secret = bob.secret_bytes();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 1);
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
    let id = a
        .enqueue_to_account(&b.account_id(), b"before rekey", None)
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
    assert_eq!(rx.try_recv().unwrap().text, b"before rekey");
    let receipt = b.delivery.lock().unwrap().retry_receipts(1)[0]
        .destination
        .event
        .clone();
    let control = receipt.conversation_id;
    a.process_delivery_control(&receipt, &bp, &mut a.delivery.lock().unwrap());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    let pending = a
        .enqueue_to_account(&b.account_id(), b"still awaiting at rekey", None)
        .await
        .unwrap();
    assert_eq!(a.delivery_status(pending), Some(DeliveryStatus::Awaiting));
    let adopted = Account::generate();
    b.persist_account_adoption(&adopted.public(), || Ok(()), || Ok(b.account.public()))
        .unwrap();
    drop(b);
    let bob = DeviceIdentity::from_secret_bytes(secret.0, secret.1);
    let new_proof = Announce::new_with_account(&bob, &adopted, "Rekeyed Bob", 1);
    let (b, _) = node(bd.path(), bob, adopted, &ap);
    a.roster
        .lock()
        .unwrap()
        .update(&new_proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    assert!(
        super::delivery_receipt::open_receipt(&a.identity, &a.account_id(), &new_proof, &receipt)
            .is_none()
    );
    assert!(a.log.lock().unwrap().has(&receipt.id).unwrap());
    assert!(b.log.lock().unwrap().has(&receipt.id).unwrap());
    assert_eq!(
        a.sync_store(&new_proof.public())
            .lock()
            .unwrap()
            .event_ids(&control)
            .unwrap(),
        vec![receipt.id]
    );
    assert!(
        b.sync_store(&ap.public())
            .lock()
            .unwrap()
            .event_ids(&control)
            .unwrap()
            .is_empty()
    );
    assert!(
        !b.sync_store(&ap.public())
            .lock()
            .unwrap()
            .durable_have(&control, &receipt.id)
            .unwrap()
    );
    let (aio, bio) = tokio::io::duplex(65536);
    let responder = b.clone();
    let server = tokio::spawn(async move {
        let mut channel = SecureChannel::accept(bio, &responder.identity)
            .await
            .unwrap();
        let store = responder.sync_store(channel.peer_identity());
        // Diagnostic cutoff makes the old 10,000-round stall a deterministic failure.
        let mut handled = 0;
        while handled < 9 {
            match super::session::serve_one(&mut channel, &store)
                .await
                .unwrap()
            {
                super::session::Served::Closed => break,
                super::session::Served::Handled(_) => handled += 1,
            }
        }
        handled
    });
    let mut channel = SecureChannel::connect(aio, &a.identity, Some(&new_proof.public()))
        .await
        .unwrap();
    let peer = channel.peer_identity().clone();
    let result = super::session::request_round(&mut channel, &a.sync_store(&peer), control).await;
    drop(channel);
    let handled = server.await.unwrap();
    assert!(matches!(
        result,
        Err(super::session::SessionError::NoProgress)
    ));
    assert!(handled < 9, "sync must stop before the diagnostic cutoff");
    assert!(
        !b.sync_store(&ap.public())
            .lock()
            .unwrap()
            .durable_have(&control, &receipt.id)
            .unwrap()
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    assert_eq!(a.delivery_status(pending), Some(DeliveryStatus::Awaiting));
}

#[tokio::test]
async fn account_file_own_copy_first_stays_canonical_and_never_confirms_target() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let cd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let source_secret = alice.secret_bytes();
    let source_account = aa.secret_bytes();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let other = DeviceIdentity::generate();
    let ca = Account::from_secret_bytes(aa.secret_bytes());
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let cp = Announce::new_with_account(&other, &ca, "Own copy", cl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    a.roster
        .lock()
        .unwrap()
        .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let (b, _) = node(bd.path(), bob, bob_account, &ap);
    let (c, _) = node(cd.path(), other, ca, &ap);
    let path = ad.path().join("空文件.txt");
    std::fs::write(&path, []).unwrap();
    let (manifest, file) = a
        .stage_file(&path, crate::file::FileKind::File, |_| {})
        .unwrap();
    let peers = {
        let roster = a.roster.lock().unwrap();
        [cp.public(), bp.public()].map(|public| {
            roster
                .peers()
                .into_iter()
                .find(|peer| peer.public == public)
                .unwrap()
                .clone()
        })
    };
    let id = a
        .accept_staged_manifest(
            &peers,
            super::conversation::account_conversation_id(&a.account_id(), &b.account_id()),
            Some(b.account_id()),
            &manifest,
        )
        .unwrap();
    let message = a.delivery.lock().unwrap().message(id).unwrap().clone();
    assert_eq!(message.destinations[0].device, cp.public());
    assert_eq!(message.destinations[0].event.id, id);
    let actual = message
        .destinations
        .iter()
        .find(|d| d.device == bp.public())
        .unwrap()
        .event
        .id;
    assert_ne!(actual, id);
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let ct = tokio::spawn(c.clone().run_accept_loop(cl));
    let own_received = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while c.file_progress(file).unwrap().is_none() || c.read_file(file).is_err() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(own_received.is_ok());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert!(c.delivery.lock().unwrap().retry_receipts(1).is_empty());
    c.save_file(file, &cd.path().join("own-empty.txt")).unwrap();
    let final_chunk = a
        .delivery
        .lock()
        .unwrap()
        .file_card(id)
        .unwrap()
        .final_chunk
        .unwrap();
    assert!(
        c.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let confirmed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered)
            || a.delivery
                .lock()
                .unwrap()
                .next_file_destination(None)
                .is_some()
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    at.abort();
    bt.abort();
    ct.abort();
    let _ = at.await;
    let _ = bt.await;
    let _ = ct.await;
    assert!(confirmed.is_ok());
    let history = a.account_history(&b.account_id(), 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert_eq!(
        b.account_history(&a.account_id(), 10).unwrap()[0].id,
        actual
    );
    assert_eq!(a.delivery_status(actual), None);
    assert_eq!(b.read_file(file).unwrap(), Vec::<u8>::new());
    drop(a);
    let (a, _) = node(
        ad.path(),
        DeviceIdentity::from_secret_bytes(source_secret.0, source_secret.1),
        Account::from_secret_bytes(source_account),
        &bp,
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    assert_eq!(a.account_history(&b.account_id(), 10).unwrap()[0].id, id);
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_none()
    );
}

#[tokio::test]
async fn file_enqueue_progress_never_waits_for_stalled_contact_tcp() {
    for account_addressed in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bp = Announce::new_with_account(
            &bob,
            &bob_account,
            "Bob",
            stalled.local_addr().unwrap().port(),
        );
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
        let (a, _) = node(dir.path(), alice, aa, &bp);
        a.configure_privacy(
            dir.path(),
            "pw",
            &ap,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, []).unwrap();
        // Windows runners may pause a local disk/KDF task for more than a second
        // under the full workspace suite. The separate accept assertion below
        // still proves enqueue never waits for or initiates contact TCP.
        let local_budget = if cfg!(windows) { 5 } else { 1 };
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(local_budget), async {
            if account_addressed {
                a.enqueue_file_to_account_progress(
                    &bob_account.account_id(),
                    &path,
                    crate::file::FileKind::File,
                    |_| {},
                )
                .await
            } else {
                a.enqueue_file_dm_progress(
                    &bob.user_id(),
                    &path,
                    crate::file::FileKind::File,
                    |_| {},
                )
                .await
            }
        })
        .await;
        let (id, _) = accepted
            .expect("enqueue must be local despite stalled contact listener")
            .unwrap();
        assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), stalled.accept())
                .await
                .is_err(),
            "enqueue must not initiate a contact connection"
        );
    }
}

#[tokio::test]
async fn private_file_chunks_resume_immutably_after_early_card_ack_and_sender_restart() {
    private_file_restart(FilePeerDelay::None).await;
}

#[tokio::test]
async fn private_file_restart_progresses_when_protocol_stages_fit_but_total_exceeds_operation_budget()
 {
    private_file_restart(FilePeerDelay::Aggregate).await;
}

#[tokio::test]
async fn private_file_restart_reconnects_after_a_partial_chunk_round_timeout() {
    private_file_restart(FilePeerDelay::PartialChunk).await;
}

#[tokio::test]
async fn private_file_restart_yields_between_individually_bounded_exchanges() {
    private_file_restart(FilePeerDelay::Exchange).await;
}

#[tokio::test]
async fn private_file_restart_advances_twenty_four_pending_scopes_fairly() {
    private_file_restart_batch(FilePeerDelay::Aggregate, 24).await;
}

#[derive(Clone, Copy, PartialEq)]
enum FilePeerDelay {
    None,
    Aggregate,
    Exchange,
    PartialChunk,
}

#[derive(Default)]
struct FilePeerObservation {
    interrupted: std::sync::atomic::AtomicBool,
    interrupted_connection: std::sync::atomic::AtomicUsize,
    next_connection: std::sync::atomic::AtomicUsize,
    fresh_completion: std::sync::atomic::AtomicBool,
    active_file_channels: std::sync::atomic::AtomicUsize,
    max_file_channels: std::sync::atomic::AtomicUsize,
    delayed_response_exchanges: std::sync::atomic::AtomicUsize,
}

struct ObservedFileChannel(Arc<FilePeerObservation>);

#[derive(Default)]
struct NegativeCustodyObservation {
    file_connections: std::sync::atomic::AtomicUsize,
    completed_attempts: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
}

// Actual guarded event storage, but a peer declining qualified file custody
// and withholding its receipt controls. Ordinary file fingerprints still match.
struct NegativeCustodyStore<S> {
    inner: S,
    file: ConversationId,
    dm: ConversationId,
    probes: std::sync::atomic::AtomicUsize,
    observation: Arc<NegativeCustodyObservation>,
}

impl<S: crate::eventlog::sync::SyncStore> crate::eventlog::sync::SyncStore
    for NegativeCustodyStore<S>
{
    fn file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<crate::eventlog::sync::FileConsentStatus, LogError> {
        self.inner.file_consent(file, manifest)
    }
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        if *conversation != self.file {
            return self.inner.durable_have(conversation, id);
        }
        let probe = self
            .probes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if probe == 0 {
            self.observation
                .file_connections
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        } else if probe == 1 {
            self.observation
                .completed_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.observation.changed.notify_waiters();
        }
        Ok(false)
    }
    fn admission_denied(&self) -> bool {
        self.inner.admission_denied()
    }
    fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
        Ok(if *conversation == self.file || *conversation == self.dm {
            self.inner.event_ids(conversation)?
        } else {
            Vec::new()
        })
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &std::collections::HashSet<EventId>,
    ) -> Result<Vec<crate::eventlog::Event>, LogError> {
        Ok(if *conversation == self.file || *conversation == self.dm {
            self.inner.events_excluding(conversation, have)?
        } else {
            Vec::new()
        })
    }
    fn ingest(
        &mut self,
        event: crate::eventlog::Event,
    ) -> Result<crate::eventlog::AppendOutcome, crate::eventlog::LogError> {
        self.inner.ingest(event)
    }
}

#[tokio::test]
async fn file_terminal_negative_custody_retries_on_cadence_without_empty_cache_self_wake() {
    use crate::eventlog::sync::SyncStore;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, bob_account, &ap);
    for (node, dir, proof) in [(&a, ad.path(), &ap), (&b, bd.path(), &bp)] {
        node.configure_privacy(
            dir,
            "pw",
            proof,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.set_invisible(true).await.unwrap();
    }
    b.initiate_contact_locally(&a.account_id()).await.unwrap();
    let path = ad.path().join("matching-chunks.bin");
    let bytes = [7; 32];
    std::fs::write(&path, bytes).unwrap();
    let (id, file) = a
        .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let dm = super::conversation::dm_conversation_id(&a.identity.public(), &b.identity.public());
    // Each independent sync needs its own bound: a busy Windows runner can
    // spend several seconds on one authenticated handshake.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        sync_delivery_test_stage(&a, &b, &bl, dm),
    )
    .await
    .expect("DM setup sync completes");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        sync_delivery_test_stage(&a, &b, &bl, file),
    )
    .await
    .expect("file setup sync completes");
    let ids = a
        .sync_store(&bp.public())
        .lock()
        .unwrap()
        .event_ids(&file)
        .unwrap();
    assert_eq!(
        b.sync_store(&ap.public())
            .lock()
            .unwrap()
            .event_ids(&file)
            .unwrap(),
        ids
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    let observation = Arc::new(NegativeCustodyObservation::default());
    let responder = b.clone();
    let observed = observation.clone();
    let mut workers = tokio::task::JoinSet::new();
    workers.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = bl.accept().await.unwrap();
            let node = responder.clone();
            let observation = observed.clone();
            connections.spawn(async move {
                let Ok(mut channel) = node.privacy_accept(stream).await else {
                    return;
                };
                let store = Mutex::new(NegativeCustodyStore {
                    inner: node
                        .sync_store(channel.peer_identity())
                        .into_inner()
                        .unwrap(),
                    file,
                    dm,
                    probes: std::sync::atomic::AtomicUsize::new(0),
                    observation,
                });
                while matches!(
                    super::session::serve_one(&mut channel, &store).await,
                    Ok(super::session::Served::Handled(_))
                ) {}
            });
            while connections.try_join_next().is_some() {}
        }
    });
    workers.spawn(a.clone().run_accept_loop(al));
    let attempted = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let changed = observation.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if observation
                .completed_attempts
                .load(std::sync::atomic::Ordering::SeqCst)
                >= 2
            {
                break;
            }
            changed.await;
        }
    })
    .await;
    workers.shutdown().await;
    assert!(
        attempted.is_ok(),
        "two actual authenticated negative-custody attempts occur"
    );
    assert!(
        observation
            .file_connections
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2
    );
    assert_eq!(
        a.empty_file_cache_self_wakes
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "terminal failure removes the last slot, so only normal cadence may readmit it"
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_some()
    );
    assert_eq!(
        a.sync_store(&bp.public())
            .lock()
            .unwrap()
            .event_ids(&file)
            .unwrap(),
        ids
    );
}

impl Drop for ObservedFileChannel {
    fn drop(&mut self) {
        self.0
            .active_file_channels
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn retained_file_transfer_rechecks_permission_and_exact_account_binding() {
    for rebind in [false, true] {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_keys = bob.secret_bytes();
        let aa = Account::generate();
        let bob_account = Account::generate();
        let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
        let bp =
            Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
        let (a, _) = node(ad.path(), alice, aa, &bp);
        let (b, _) = node(bd.path(), bob, bob_account, &ap);
        for (node, dir, own) in [(&a, ad.path(), &ap), (&b, bd.path(), &bp)] {
            node.configure_privacy(
                dir,
                "pw",
                own,
                Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
            )
            .unwrap();
            node.set_invisible(true).await.unwrap();
        }
        b.initiate_contact_locally(&a.account_id()).await.unwrap();
        let path = ad.path().join("private.bin");
        std::fs::write(&path, [9; 32]).unwrap();
        let (id, file) = a
            .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let upload = a
            .outgoing_file_progress(id)
            .expect("accepted file progress");
        assert_eq!((upload.done, upload.total, upload.delivered), (0, 1, false));
        let mut workers = tokio::task::JoinSet::new();
        workers.spawn(b.clone().run_accept_loop(bl));
        let card = a.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let mut transfers = std::collections::HashMap::new();
        assert!(!a.retry_file_step(&card, &destination, &mut transfers).await);
        assert_eq!(
            transfers.len(),
            1,
            "the real authenticated channel is retained between phases"
        );
        if rebind {
            let account = Account::generate();
            let proof = Announce::new_with_account(
                &DeviceIdentity::from_secret_bytes(bob_keys.0, bob_keys.1),
                &account,
                "Rebound",
                bp.tcp_port,
            );
            a.roster
                .lock()
                .unwrap()
                .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
            a.initiate_contact_locally(&account.account_id())
                .await
                .unwrap();
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                a.set_allowed(&b.account_id(), false),
            )
            .await
            .unwrap()
            .unwrap();
        }
        assert!(!a.retry_file_step(&card, &destination, &mut transfers).await);
        assert!(
            transfers.is_empty(),
            "stale channels cannot advance a later phase"
        );
        assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
        assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
        workers.shutdown().await;
    }
}

#[tokio::test]
async fn completed_file_custody_recycles_a_usable_sync_channel() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", addr.port());
    let (sender, _) = node(ad.path(), alice, aa, &bp);
    let (recipient, _) = node(bd.path(), bob, bob_account, &ap);
    for (node, dir, own) in [(&sender, ad.path(), &ap), (&recipient, bd.path(), &bp)] {
        node.configure_privacy(
            dir,
            "pw",
            own,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_accepted = accepted.clone();
    let responder = recipient.clone();
    let server = tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let responder = responder.clone();
            sessions.spawn(async move {
                let channel = responder.privacy_accept(stream).await.unwrap();
                responder.serve_connection(channel).await;
            });
        }
    });
    for (name, contents) in [
        ("first.bin", b"first exact durable file custody".as_slice()),
        (
            "second.bin",
            b"second exact durable file custody".as_slice(),
        ),
    ] {
        let path = ad.path().join(name);
        std::fs::write(&path, contents).unwrap();
        let (id, file) = sender
            .enqueue_file_to_account(&recipient.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let card = sender
            .delivery
            .lock()
            .unwrap()
            .file_card(id)
            .unwrap()
            .clone();
        let destination = card.destinations[0].clone();
        let mut transfers = std::collections::HashMap::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !sender
                .retry_file_step(&card, &destination, &mut transfers)
                .await
            {}
        })
        .await
        .expect("file reaches exact durable custody");
        assert_eq!(recipient.read_file(file).unwrap(), contents);
    }
    assert_eq!(
        accepted.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "sequential files should share the completed authenticated session"
    );
    let (mut channel, reused) = sender
        .privacy_sync_dial_with_reuse(addr, &bp.public())
        .await
        .unwrap();
    assert!(
        reused,
        "completed file should return its authenticated channel"
    );
    super::session::request_round(
        &mut channel,
        &sender.sync_store(&bp.public()),
        ConversationId::new([218; 32]),
    )
    .await
    .unwrap();
    server.abort();
}

async fn private_file_restart(delay: FilePeerDelay) {
    private_file_restart_batch(delay, 1).await;
}

// Grant the original Manual permissions in order while their best-effort
// presence publications have real inbound handshakes to finish. No application
// traffic exists yet, and these borrowed futures cannot outlive this setup.
async fn grant_file_restart_permissions(a: &Node, b: &Node, al: &TcpListener, bl: &TcpListener) {
    async fn receive_presence(node: &Node, listener: &TcpListener) {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            // The first publication can legitimately be denied before the
            // reverse Manual grant. Reject/drop it without serving sync frames.
            let _ = node.privacy_accept(stream).await;
        }
    }
    tokio::select! {
        _ = async {
            a.set_allowed(&b.account_id(), true).await.unwrap();
            b.set_allowed(&a.account_id(), true).await.unwrap();
        } => {}
        _ = receive_presence(a, al) => unreachable!("presence setup accept loop does not return"),
        _ = receive_presence(b, bl) => unreachable!("presence setup accept loop does not return"),
    }
    // select! has dropped both scoped accept/handshake futures and their
    // channels before file creation or the separately driven card ACK rounds.
}

async fn private_file_restart_batch(delay: FilePeerDelay, count: usize) {
    let mut case = super::delivery_runtime::TestTiming::new("file-restart-case-setup");
    use crate::eventlog::sync::SyncStore;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let secret = alice.secret_bytes();
    let account_secret = aa.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, bob_account, &ap);
    for (node, dir, own) in [(&a, ad.path(), &ap), (&b, bd.path(), &bp)] {
        node.configure_privacy(
            dir,
            "pw",
            own,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.set_invisible(true).await.unwrap();
    }
    case.phase("file-restart-publish-permissions");
    Box::pin(tokio::time::timeout(
        super::transport::HANDSHAKE_TIMEOUT,
        grant_file_restart_permissions(&a, &b, &al, &bl),
    ))
    .await
    .expect("permission publication completes against live fixture listeners");
    case.phase("file-restart-permissions-published");
    let bytes = vec![9; crate::file::CHUNK_SIZE * 2 + 1];
    let path = ad.path().join("断点续传.bin");
    std::fs::write(&path, &bytes).unwrap();
    let (id, file) = a
        .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let ids = a
        .log
        .lock()
        .unwrap()
        .events(&file)
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect::<Vec<_>>();
    let mut originals = vec![(id, file, ids.clone())];
    // Reproduce missing volatile scope: DM card/control remain authorized, while
    // chunk disclosure is blocked until durable scope import on reopen.
    for (_, file, _) in &originals {
        a.withheld_file_scopes.lock().unwrap().insert(*file);
    }
    case.phase("file-restart-card-ack-preparation");
    let confirmed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        // Drive the two authorized conversations independently. A missing chunk
        // scope must not make card acknowledgement depend on a chunk attempt.
        let manifest_conv = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
            .event
            .conversation_id;
        sync_delivery_test_stage(&a, &b, &bl, manifest_conv).await;
        assert!(b.read_file(file).is_err());
        let control = super::delivery_receipt::delivery_conversation_id(
            &a.identity.public(),
            &b.identity.public(),
        );
        sync_delivery_test_stage(&b, &a, &al, control).await;
    })
    .await;
    if count > 1 {
        assert!(confirmed.is_ok());
        // Prepare independent already-acknowledged cards. The original single
        // card's five-second setup budget is not an aggregate 24-card budget.
        for card_index in 1..count {
            if std::env::var("MESH_TALK_TEST_TIMING").as_deref() == Ok("1") {
                eprintln!("test-timing batch_card_index={card_index} phase=enqueue-and-card-ack");
            }
            let (id, file) = a
                .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
                .await
                .unwrap();
            let ids = a
                .log
                .lock()
                .unwrap()
                .events(&file)
                .unwrap()
                .iter()
                .map(|e| e.id)
                .collect();
            a.withheld_file_scopes.lock().unwrap().insert(file);
            // Each card still has its own deadline. Windows CI can briefly
            // stall a handshake while running the full workspace in parallel.
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let manifest_conv = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
                    .event
                    .conversation_id;
                sync_delivery_test_stage(&a, &b, &bl, manifest_conv).await;
                assert!(b.read_file(file).is_err());
                let control = super::delivery_receipt::delivery_conversation_id(
                    &a.identity.public(),
                    &b.identity.public(),
                );
                sync_delivery_test_stage(&b, &a, &al, control).await;
            })
            .await
            .expect("each independent card ACK precedes chunk-scope restoration");
            originals.push((id, file, ids));
        }
    }
    case.phase("file-restart-all-cards-acknowledged");
    drop(al);
    drop(bl);
    assert!(confirmed.is_ok());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    for (id, file, _) in &originals {
        assert_eq!(a.delivery_status(*id), Some(DeliveryStatus::Delivered));
        assert!(b.read_file(*file).is_err(), "every card precedes download");
    }
    assert!(
        !a.has_file_scope(file).unwrap(),
        "card ACK is installed before the missing chunk scope is restored"
    );
    assert!(
        b.read_file(file).is_err(),
        "card delivery is independent of complete download"
    );
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_some()
    );
    case.phase("file-restart-reopen");
    drop(a);
    let al = TcpListener::bind(("127.0.0.1", ap.tcp_port)).await.unwrap();
    let bl = TcpListener::bind(("127.0.0.1", bp.tcp_port)).await.unwrap();
    let (a, _) = node(
        ad.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
        &bp,
    );
    a.configure_privacy(
        ad.path(),
        "pw",
        &ap,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    if count == 1 {
        assert_eq!(a.account_history(&b.account_id(), 10).unwrap()[0].id, id);
    } else {
        assert!(
            a.account_history(&b.account_id(), 100)
                .unwrap()
                .iter()
                .any(|entry| entry.id == id)
        );
    }
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    assert_eq!(
        a.log
            .lock()
            .unwrap()
            .events(&file)
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        a.sync_store(&bp.public())
            .lock()
            .unwrap()
            .event_ids(&file)
            .unwrap(),
        ids,
        "restart restores the original authorized chunk projection"
    );
    for (_, file, ids) in &originals {
        assert_eq!(
            a.sync_store(&bp.public())
                .lock()
                .unwrap()
                .event_ids(file)
                .unwrap(),
            *ids
        );
    }
    case.phase("file-restart-restored-start-workers");
    let mut workers = tokio::task::JoinSet::new();
    workers.spawn(a.clone().run_accept_loop(al));
    let observation = Arc::new(FilePeerObservation::default());
    if delay != FilePeerDelay::None {
        workers.spawn(delayed_private_file_peer(
            b.clone(),
            bl,
            file,
            delay,
            observation.clone(),
        ));
    } else {
        workers.spawn(b.clone().run_accept_loop(bl));
    }
    let first_progress = if count > 1 {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while !originals.iter().any(|(_, file, _)| {
                b.file_progress(*file)
                    .unwrap()
                    .is_some_and(|p| p.done == p.total)
            }) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok()
    } else {
        true
    };
    case.phase("file-restart-first-progress-observed");
    let mut slowest_poll = std::time::Duration::ZERO;
    let completed = tokio::time::timeout(
        // This observes full file and receipt convergence, including a retry
        // after the last chunk. Windows CI can take over five seconds here
        // even after the receiver has verified every byte.
        std::time::Duration::from_secs(if delay == FilePeerDelay::PartialChunk {
            30
        } else if count == 1 {
            10
        } else {
            60
        }),
        async {
            if !first_progress {
                return;
            }
            loop {
                let started = std::time::Instant::now();
                let exact = originals.iter().all(|(_, file, _)| {
                    if count == 1 {
                        b.read_file(*file)
                            .as_ref()
                            .is_ok_and(|actual| actual == &bytes)
                    } else {
                        // Repeated debug AEAD/checksum reads across 24 files
                        // measured 534ms and blocked this single-thread
                        // executor. Verify all original bytes once below.
                        b.file_progress(*file)
                            .unwrap()
                            .is_some_and(|p| p.done == p.total)
                    }
                });
                slowest_poll = slowest_poll.max(started.elapsed());
                if exact
                    && a.delivery
                        .lock()
                        .unwrap()
                        .next_file_destination(None)
                        .is_none()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    case.phase("file-restart-converged-stop-workers");
    workers.shutdown().await;
    case.phase("file-restart-final-invariants");
    if !first_progress {
        let completed = originals
            .iter()
            .filter(|(_, file, _)| b.read_file(*file).is_ok())
            .count();
        eprintln!(
            "file-backlog failure completed={completed}/{count} original-scopes-restored=true"
        );
    }
    assert!(
        first_progress,
        "a bounded active set must advance despite the backlog"
    );
    if completed.is_err() {
        eprintln!(
            "file-backlog observer slowest-poll-ms={} completed={}/{} accepted={} max-file-channels={}",
            slowest_poll.as_millis(),
            originals
                .iter()
                .filter(|(_, file, _)| b
                    .file_progress(*file)
                    .unwrap()
                    .is_some_and(|p| p.done == p.total))
                .count(),
            count,
            observation
                .next_connection
                .load(std::sync::atomic::Ordering::SeqCst),
            observation
                .max_file_channels
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        let final_chunk = *ids.last().unwrap();
        let exact_bytes = b
            .read_file(file)
            .as_ref()
            .is_ok_and(|actual| actual == &bytes);
        let counts = b.file_progress(file).unwrap().map(|p| (p.done, p.total));
        let held = b
            .sync_store(&ap.public())
            .lock()
            .unwrap()
            .durable_have(&file, &final_chunk)
            .unwrap();
        let pending = a
            .delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_some();
        let immutable = a
            .log
            .lock()
            .unwrap()
            .events(&file)
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>()
            == ids;
        eprintln!(
            "file-restart failure chunks={counts:?} exact-bytes={exact_bytes} final-authorized-have={held} pending-work={pending} immutable-ids={immutable}"
        );
    }
    assert!(
        completed.is_ok(),
        "restart must restore immutable chunk work and private scope"
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
    if delay == FilePeerDelay::Exchange {
        let delayed = observation
            .delayed_response_exchanges
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            delayed >= 5,
            "{delayed} delayed 100ms exchanges must exceed one 400ms operation budget"
        );
    }
    if count > 1 {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while observation
                .active_file_channels
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned cancelled responders must finish dropping their sockets");
        assert_eq!(
            a.file_transfer_peak
                .load(std::sync::atomic::Ordering::SeqCst),
            8,
            "actual client retained cache reaches but never exceeds its eight slots"
        );
        assert_eq!(
            observation
                .active_file_channels
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "owned responder channels are dropped on shutdown"
        );
    }
    for (_, file, ids) in &originals {
        assert_eq!(b.read_file(*file).unwrap(), bytes);
        assert_eq!(
            a.log
                .lock()
                .unwrap()
                .events(file)
                .unwrap()
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            *ids
        );
        assert!(
            b.sync_store(&ap.public())
                .lock()
                .unwrap()
                .durable_have(file, ids.last().unwrap())
                .unwrap()
        );
    }
    if delay == FilePeerDelay::PartialChunk {
        assert!(
            observation
                .interrupted
                .load(std::sync::atomic::Ordering::SeqCst),
            "actual partial chunk round was interrupted"
        );
        assert!(
            observation
                .fresh_completion
                .load(std::sync::atomic::Ordering::SeqCst),
            "remaining original chunks arrived over a fresh authenticated connection"
        );
    }
    let saved = bd.path().join("saved.bin");
    b.save_file(file, &saved).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), bytes);
    drop(a);
    let (a, _) = node(
        ad.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
        &bp,
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    if count == 1 {
        assert_eq!(a.account_history(&b.account_id(), 10).unwrap()[0].id, id);
    } else {
        assert!(
            a.account_history(&b.account_id(), 100)
                .unwrap()
                .iter()
                .any(|entry| entry.id == id)
        );
    }
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_none()
    );
    assert!(a.delivery.lock().unwrap().file_card(id).is_some());
    case.finish();
}

async fn delayed_private_file_peer(
    node: Arc<Node>,
    listener: TcpListener,
    file: ConversationId,
    delay: FilePeerDelay,
    observation: Arc<FilePeerObservation>,
) {
    let accepts = async {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let node = node.clone();
            let observation = observation.clone();
            connections.spawn(async move {
                let mut timing =
                    super::delivery_runtime::TestTiming::new("delayed-peer-tcp-accepted");
                timing.socket(&stream);
                // Each protocol phase takes less than the unchanged 400ms
                // operation budget. Their aggregate deterministically exceeds it.
                if delay == FilePeerDelay::Aggregate {
                    timing.phase("delayed-peer-before-accept-150ms");
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                timing.phase("delayed-peer-privacy-accept");
                let Ok(mut channel) = node.privacy_accept(stream).await else {
                    return;
                };
                let connection = observation
                    .next_connection
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                let peer = channel.peer_identity().clone();
                let store = node.sync_store(&peer);
                let mut file_channel = None;
                while let Ok(bytes) = channel.recv().await {
                    timing.phase(if super::session::requests_response(&bytes) {
                        "delayed-peer-response-request"
                    } else {
                        "delayed-peer-followup"
                    });
                    if file_channel.is_none()
                        && super::session::round_request_conversation(&bytes)
                            .is_some_and(|conv| node.file_progress(conv).unwrap().is_some())
                    {
                        let active = observation
                            .active_file_channels
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                            + 1;
                        observation
                            .max_file_channels
                            .fetch_max(active, std::sync::atomic::Ordering::SeqCst);
                        file_channel = Some(ObservedFileChannel(observation.clone()));
                    }
                    if delay == FilePeerDelay::Aggregate
                        || (delay == FilePeerDelay::Exchange
                            && super::session::requests_response(&bytes))
                    {
                        if delay == FilePeerDelay::Exchange {
                            observation
                                .delayed_response_exchanges
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    } else if delay == FilePeerDelay::PartialChunk
                        && super::session::is_followup(&bytes, file)
                        && node
                            .file_progress(file)
                            .unwrap()
                            .is_some_and(|progress| progress.done == 1)
                        && !observation
                            .interrupted
                            .swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        observation
                            .interrupted_connection
                            .store(connection, std::sync::atomic::Ordering::SeqCst);
                        // One original chunk is already admitted. A burst's next
                        // Followup now arrives in the same reconciliation; stall it
                        // past the exchange deadline so retries abandon this socket.
                        tokio::time::sleep(
                            super::delivery_runtime::FILE_EXCHANGE_TIMEOUT
                                + std::time::Duration::from_millis(50),
                        )
                        .await;
                    }
                    timing.phase("delayed-peer-serve-exchange");
                    match super::session::serve_wire_bytes(&mut channel, &store, &bytes).await {
                        Ok(super::session::Served::Handled(conv)) => {
                            timing.phase("delayed-peer-process-events");
                            node.emit_new_messages(conv);
                            node.process_file_events(conv);
                            if conv == file
                                && observation
                                    .interrupted
                                    .load(std::sync::atomic::Ordering::SeqCst)
                                && connection
                                    != observation
                                        .interrupted_connection
                                        .load(std::sync::atomic::Ordering::SeqCst)
                                && node
                                    .file_progress(file)
                                    .unwrap()
                                    .is_some_and(|p| p.done == p.total)
                            {
                                observation
                                    .fresh_completion
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
                        _ => break,
                    }
                }
                timing.phase("delayed-peer-receive-ended");
                timing.finish();
            });
            while connections.try_join_next().is_some() {}
        }
    };
    tokio::select! {
        _ = accepts => {},
        _ = node.clone().run_delivery_loop() => {},
    }
}

#[test]
fn erased_static_file_manifest_does_not_resurrect_after_restart_or_sync() {
    erased_static_manifest_reopen(false);
}

#[test]
fn erased_static_file_manifest_stays_erased_after_authorized_account_adoption() {
    erased_static_manifest_reopen(true);
}

fn erased_static_manifest_reopen(adopt: bool) {
    use crate::eventlog::{Event, EventKind};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let secret = bob.secret_bytes();
    let mut account_secret = bob_account.secret_bytes();
    let (b, _) = node(dir.path(), bob, bob_account, &ap);
    let conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
    let manifest = crate::file::FileManifest {
        name: "erase.txt".into(),
        size: 0,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(&[]),
        file_key: [3; 32],
        file_conv: crate::eventlog::ConversationId::new([4; 32]),
        chunk_count: 1,
    };
    let event = Event::new(
        &alice,
        conv,
        1,
        vec![],
        1,
        100,
        EventKind::FileManifest,
        crate::dm::seal(&alice, &b.identity.public().x25519_pub, &manifest.encode()).unwrap(),
    );
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.process_file_events(conv);
    assert_eq!(b.account_history(&aa.account_id(), 10).unwrap().len(), 1);
    assert_eq!(
        b.delete_account_message(&aa.account_id(), event.id)
            .unwrap(),
        1
    );
    if adopt {
        let account = Account::generate();
        b.persist_account_adoption(&account.public(), || Ok(()), || Ok(b.account.public()))
            .unwrap();
        account_secret = account.secret_bytes();
    }
    drop(b);
    let (b, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
        &ap,
    );
    b.process_file_events(conv);
    assert!(
        b.account_history(&aa.account_id(), 10).unwrap().is_empty(),
        "static manifest must stay locally erased"
    );
    assert!(b.delivery.lock().unwrap().retry_receipts(1).is_empty());
    assert!(
        b.files
            .lock()
            .unwrap()
            .manifest(manifest.file_conv)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn automatic_file_card_receipt_and_chunk_resume_without_manual_flush() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, bob_account, &ap);
    let bytes = vec![5; crate::file::CHUNK_SIZE + 1];
    let path = ad.path().join("boundary.bin");
    std::fs::write(&path, &bytes).unwrap();
    let (id, file) = a
        .enqueue_file_dm(&bp.public().user_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let result = tokio::time::timeout(std::time::Duration::from_secs(6), async {
        loop {
            if a.delivery_status(id) == Some(DeliveryStatus::Delivered)
                && b.read_file(file)
                    .as_ref()
                    .is_ok_and(|actual| actual == &bytes)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    assert!(
        result.is_ok(),
        "automatic immutable chunks must continue after card confirmation"
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
}

#[tokio::test]
async fn protected_image_loopback_retains_policy_and_blocks_ordinary_receive_reads() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _, mut files) = node_with_files(bd.path(), bob, bob_account, &ap);
    let bytes = b"\x89PNG\r\n\x1a\nprivate image bytes".to_vec();
    let path = ad.path().join("sensitive-name.png");
    std::fs::write(&path, &bytes).unwrap();
    let (id, file) = a
        .enqueue_protected_file_to_account_progress_expiring_if(
            &b.account_id(),
            &path,
            crate::file::FileKind::Media,
            |_| {},
            None,
            MessageProtection::Authenticate,
            |accept| accept(),
        )
        .await
        .unwrap();
    let reference = SearchReference::Account {
        account_id: a.account_id(),
        message_id: Some(id),
    };
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            loop {
                if a.delivery_status(id) == Some(DeliveryStatus::Delivered)
                    && b.protected_image_content(&reference).unwrap().is_some_and(
                        |(mode, actual)| mode == MessageProtection::Authenticate && actual == bytes,
                    )
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    assert!(
        result.is_ok(),
        "automatic immutable chunks must continue after card confirmation"
    );
    let received = files.try_recv().unwrap();
    assert_eq!(received.protection, Some(MessageProtection::Authenticate));
    assert_eq!(received.name, "Protected image");
    assert!(!format!("{received:?}").contains("sensitive-name"));
    assert!(b.read_file(file).is_err());
    assert!(b.read_media(file).is_none());
    assert_eq!(
        b.account_history(&a.account_id(), 10).unwrap()[0].protection,
        Some(MessageProtection::Authenticate)
    );
    a.recall_account(&b.account_id(), id).await.unwrap();
    assert!(
        a.protected_image_content(&SearchReference::Account {
            account_id: b.account_id(),
            message_id: Some(id)
        })
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn accepted_channel_file_sidecar_failure_recovers_same_identity_and_protection() {
    for protection in [
        None,
        Some(MessageProtection::Click),
        Some(MessageProtection::Authenticate),
    ] {
        for reopen in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let proof = Announce::new_with_account(
                &DeviceIdentity::generate(),
                &Account::generate(),
                "Peer",
                9,
            );
            let identity = DeviceIdentity::generate();
            let account = Account::generate();
            let keys = identity.secret_bytes();
            let account_keys = account.secret_bytes();
            let (mut node, _) = node(dir.path(), identity, account, &proof);
            let channel = node.create_channel("images", vec![]).await.unwrap();
            node.received_files
                .lock()
                .unwrap()
                .fail_appends_for_test(true)
                .unwrap();
            let bytes = b"\x89PNG\r\n\x1a\nchannel image";
            let path = dir.path().join("image.png");
            std::fs::write(&path, bytes).unwrap();
            let mut checks = 0;
            let mut authorize = |accept: &mut dyn FnMut() -> Result<(), NodeError>| {
                checks += 1;
                if checks <= 2 {
                    accept()
                } else {
                    Err(NodeError::Authorization(
                        "locked after durable acceptance".into(),
                    ))
                }
            };
            let accepted = if let Some(mode) = protection {
                node.enqueue_protected_file_channel_progress_expiring_if(
                    channel,
                    &path,
                    crate::file::FileKind::Media,
                    |_| {},
                    None,
                    mode,
                    &mut authorize,
                )
                .await
            } else {
                node.enqueue_file_channel_progress_expiring_if(
                    channel,
                    &path,
                    crate::file::FileKind::Media,
                    |_| {},
                    None,
                    &mut authorize,
                )
                .await
            };
            let (id, file) =
                accepted.expect("durable acceptance must not become a retryable rejection");
            assert_eq!(checks, 2, "no authorization after WAL acceptance");
            let history = node.channel_history(channel, 10).unwrap();
            assert_eq!(history.len(), 1, "pending WAL card remains visible");
            assert_eq!(history[0].id, id);
            assert_eq!(history[0].protection, protection);
            assert_eq!(history[0].file.as_ref().unwrap().file_conv, file);
            assert_eq!(
                node.delivery.lock().unwrap().pending_transactions().len(),
                1
            );
            node.received_files
                .lock()
                .unwrap()
                .fail_appends_for_test(false)
                .unwrap();
            if reopen {
                drop(node);
                node = self::node(
                    dir.path(),
                    DeviceIdentity::from_secret_bytes(keys.0, keys.1),
                    Account::from_secret_bytes(account_keys),
                    &proof,
                )
                .0;
            } else {
                node.recover_delivery(&mut node.delivery.lock().unwrap())
                    .unwrap();
            }
            let history = node.channel_history(channel, 10).unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].id, id);
            assert_eq!(history[0].protection, protection);
            assert!(
                node.delivery
                    .lock()
                    .unwrap()
                    .pending_transactions()
                    .is_empty()
            );
            let log = node.log.lock().unwrap();
            let manifests: Vec<_> = log
                .events(&channel)
                .unwrap()
                .into_iter()
                .filter(|event| event.kind == crate::eventlog::EventKind::FileManifest)
                .collect();
            assert_eq!(manifests.len(), 1);
            assert_eq!(manifests[0].id, id);
            drop(log);
            if let Some(protection) = protection {
                assert!(node.read_file(file).is_err());
                assert!(node.read_media(file).is_none());
                assert_eq!(
                    node.protected_image_content(&SearchReference::Channel {
                        channel_id: channel,
                        message_id: Some(id),
                    })
                    .unwrap(),
                    Some((protection, bytes.to_vec()))
                );
            } else {
                assert_eq!(node.read_file(file).unwrap(), bytes);
            }
        }
    }
}

#[tokio::test]
async fn channel_file_authorization_rejects_before_staging_or_wal_acceptance() {
    for reject_at in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "Peer",
            9,
        );
        let (node, _) = node(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = node.create_channel("images", vec![]).await.unwrap();
        let before = node.log.lock().unwrap().conversations().unwrap().len();
        let path = dir.path().join("image.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nchannel image").unwrap();
        let mut checks = 0;
        let result = node
            .enqueue_protected_file_channel_progress_expiring_if(
                channel,
                &path,
                crate::file::FileKind::Media,
                |_| {},
                None,
                MessageProtection::Authenticate,
                |accept| {
                    checks += 1;
                    if checks == reject_at {
                        Err(NodeError::Authorization("locked before acceptance".into()))
                    } else {
                        accept()
                    }
                },
            )
            .await;
        assert!(matches!(result, Err(NodeError::Authorization(_))));
        assert_eq!(checks, reject_at);
        assert!(node.channel_history(channel, 10).unwrap().is_empty());
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        assert!(
            node.log
                .lock()
                .unwrap()
                .events(&channel)
                .unwrap()
                .iter()
                .all(|event| event.kind != crate::eventlog::EventKind::FileManifest)
        );
        if reject_at == 1 {
            assert_eq!(
                node.log.lock().unwrap().conversations().unwrap().len(),
                before
            );
        }
    }
}

#[tokio::test]
async fn accepted_file_card_sidecar_failure_keeps_same_pending_history_id() {
    for reopen in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let bob = DeviceIdentity::generate();
        let bp = Announce::new_with_account(&bob, &Account::generate(), "Bob", 9);
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let keys = identity.secret_bytes();
        let account_keys = account.secret_bytes();
        let (mut a, _) = node(dir.path(), identity, account, &bp);
        a.received_files
            .lock()
            .unwrap()
            .fail_appends_for_test(true)
            .unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, []).unwrap();
        let (id, file) = a
            .enqueue_file_dm(&bob.user_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let history = a.dm_history(&bob.public(), 10).unwrap();
        assert_eq!(
            history.len(),
            1,
            "durably accepted pending card remains visible"
        );
        assert_eq!(history[0].id, id);
        assert_eq!(history[0].file.as_ref().unwrap().file_conv, file);
        assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
        a.received_files
            .lock()
            .unwrap()
            .fail_appends_for_test(false)
            .unwrap();
        if reopen {
            drop(a);
            a = node(
                dir.path(),
                DeviceIdentity::from_secret_bytes(keys.0, keys.1),
                Account::from_secret_bytes(account_keys),
                &bp,
            )
            .0;
        } else {
            let mut store = a.delivery.lock().unwrap();
            a.recover_delivery(&mut store).unwrap();
        }
        assert_eq!(a.dm_history(&bob.public(), 10).unwrap()[0].id, id);
        assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
        assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
        assert!(a.files.lock().unwrap().manifest(file).unwrap().is_some());
    }
}

#[tokio::test]
async fn oversized_text_is_rejected_before_acceptance_and_ratchet_install() {
    let dir = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(dir.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
    let sessions = std::fs::read(dir.path().join("ratchet.sessions")).unwrap();
    let result = a
        .enqueue_to_account(&bp.account_id().unwrap(), &vec![b'x'; 128 * 1024], None)
        .await;
    assert!(
        result.is_err(),
        "oversized transport event cannot be durably accepted"
    );
    assert!(
        result
            .as_ref()
            .unwrap_err()
            .to_string()
            .starts_with("invalid input:"),
        "local frame rejection must not be diagnosed as corrupt storage"
    );
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
    assert!(a.sentlog.lock().unwrap().conversations().is_empty());
    assert!(a.log.lock().unwrap().conversations().unwrap().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("ratchet.sessions")).unwrap(),
        sessions
    );
    let id = a
        .enqueue_to_account(&b.account_id(), b"small after rejection", None)
        .await
        .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let received = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    let delivered = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    assert_eq!(received.unwrap().unwrap().text, b"small after rejection");
    assert!(delivered.is_ok());
}

#[tokio::test]
async fn live_receive_recovery_emits_exactly_one_callback_after_durable_install() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
    let id = a
        .enqueue_to_account(&b.account_id(), b"live recovery", None)
        .await
        .unwrap();
    let event = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.received
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    b.emit_new_messages(event.conversation_id);
    assert!(rx.try_recv().is_err());
    assert!(b.delivery.lock().unwrap().retry_receipts(1).is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    b.received
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let task = tokio::spawn(b.clone().run_accept_loop(bl));
    let message = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    let delivered = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    task.abort();
    at.abort();
    let _ = task.await;
    let _ = at.await;
    assert_eq!(
        message
            .expect("worker must surface the recovered receive")
            .unwrap()
            .text,
        b"live recovery"
    );
    assert!(
        delivered.is_ok(),
        "source must receive the authenticated recovered ACK"
    );
    b.emit_new_messages(event.conversation_id);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn incoming_file_sidecar_failure_recovers_live_once_or_silently_on_reopen_and_delivers_ack() {
    for reopen in [false, true] {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let secret = bob.secret_bytes();
        let account_secret = bob_account.secret_bytes();
        let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
        let bp =
            Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
        let (a, _) = node(ad.path(), alice, aa, &bp);
        let (mut b, _, mut files) = node_with_files(bd.path(), bob, bob_account, &ap);
        let reception_start = b.reception_head().unwrap();
        let path = ad.path().join("empty.txt");
        std::fs::write(&path, []).unwrap();
        let (id, file) = a
            .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let event = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
            .event
            .clone();
        b.log.lock().unwrap().append_durable(event.clone()).unwrap();
        b.received_files
            .lock()
            .unwrap()
            .fail_appends_for_test(true)
            .unwrap();
        b.process_file_events(event.conversation_id);
        assert!(files.try_recv().is_err());
        assert!(b.delivery.lock().unwrap().retry_receipts(1).is_empty());
        assert!(b.files.lock().unwrap().manifest(file).unwrap().is_none());
        assert_eq!(b.reception_head().unwrap(), reception_start);
        b.received_files
            .lock()
            .unwrap()
            .fail_appends_for_test(false)
            .unwrap();
        if reopen {
            drop(b);
            let (opened, _, incoming) = node_with_files(
                bd.path(),
                DeviceIdentity::from_secret_bytes(secret.0, secret.1),
                Account::from_secret_bytes(account_secret),
                &ap,
            );
            b = opened;
            files = incoming;
            assert!(files.try_recv().is_err(), "startup replay must stay silent");
        }
        let at = tokio::spawn(a.clone().run_accept_loop(al));
        let bt = tokio::spawn(b.clone().run_accept_loop(bl));
        let delivered = tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        at.abort();
        bt.abort();
        let _ = at.await;
        let _ = bt.await;
        assert!(delivered.is_ok());
        if !reopen {
            assert_eq!(files.try_recv().unwrap().file_conv, file);
        }
        assert!(files.try_recv().is_err());
        b.process_file_events(event.conversation_id);
        assert!(files.try_recv().is_err());
        assert_eq!(b.account_history(&a.account_id(), 10).unwrap().len(), 1);
        assert!(b.files.lock().unwrap().manifest(file).unwrap().is_some());
        let page = b
            .reception_page(reception_start, None, Default::default())
            .unwrap();
        assert_eq!(
            page.receipts.len(),
            1,
            "the durable host effect survives live retry and restart"
        );
        let super::HostReception::File(received) = &page.receipts[0] else {
            panic!("expected file reception");
        };
        assert_eq!(received.file_conv, file);
        assert_eq!(received.arrival_kind, "live");
        assert!(!received.own_device);
        assert_eq!(received.batch_size, 1);
        assert!(received.batch_complete);
        assert!(received.batch_id.is_none());
    }
}

#[test]
fn incomplete_incoming_file_card_durably_schedules_receipt() {
    use crate::eventlog::{Event, EventKind};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let (b, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &ap,
    );
    let conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
    let manifest = crate::file::FileManifest {
        name: "missing.txt".into(),
        size: 3,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(b"abc"),
        file_key: [3; 32],
        file_conv: crate::eventlog::ConversationId::new([4; 32]),
        chunk_count: 1,
    };
    let event = Event::new(
        &alice,
        conv,
        1,
        vec![],
        1,
        100,
        EventKind::FileManifest,
        crate::dm::seal(&alice, &b.identity.public().x25519_pub, &manifest.encode()).unwrap(),
    );
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.process_file_events(conv);
    assert!(b.read_file(manifest.file_conv).is_err());
    let receipts = b.delivery.lock().unwrap().retry_receipts_after(None, 1);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].original_event_id, event.id);
}

#[test]
fn incoming_manifest_rejects_malformed_public_conversation_and_live_key_collision() {
    use crate::eventlog::{ConversationId, Event, EventKind};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let ap = Announce::new_with_account(&alice, &Account::generate(), "Alice", 9);
    let (b, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &ap,
    );
    let conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
    let mut manifest = crate::file::FileManifest {
        name: "live.txt".into(),
        size: 0,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(&[]),
        file_key: [3; 32],
        file_conv: ConversationId::new([4; 32]),
        chunk_count: 1,
    };
    let seal = |manifest: &crate::file::FileManifest, conversation, seq, parents| {
        Event::new(
            &alice,
            conversation,
            seq,
            parents,
            seq,
            100,
            EventKind::FileManifest,
            crate::dm::seal(&alice, &b.identity.public().x25519_pub, &manifest.encode()).unwrap(),
        )
    };
    manifest.chunk_count = 2;
    let malformed = seal(&manifest, conv, 1, vec![]);
    b.log.lock().unwrap().append_durable(malformed).unwrap();
    b.process_file_events(conv);
    assert!(b.received_files.lock().unwrap().conversations().is_empty());
    manifest.chunk_count = 1;
    let public = ConversationId::new([5; 32]);
    let misplaced = seal(&manifest, public, 1, vec![]);
    b.log.lock().unwrap().append_durable(misplaced).unwrap();
    b.process_file_events(public);
    assert!(b.received_files.lock().unwrap().conversations().is_empty());
    let valid = seal(&manifest, conv, 2, vec![]);
    b.log.lock().unwrap().append_durable(valid.clone()).unwrap();
    b.process_file_events(conv);
    assert_eq!(
        b.account_history(&ap.account_id().unwrap(), 10)
            .unwrap()
            .len(),
        1
    );
    manifest.file_key = [7; 32];
    let collision = seal(&manifest, conv, 3, vec![valid.id]);
    b.log.lock().unwrap().append_durable(collision).unwrap();
    b.process_file_events(conv);
    assert_eq!(
        b.account_history(&ap.account_id().unwrap(), 10)
            .unwrap()
            .len(),
        1,
        "conflicting file key must not be accepted"
    );
    assert_eq!(b.delivery.lock().unwrap().retry_receipts(10).len(), 1);
}

#[test]
fn incoming_manifest_requires_dense_signed_chunk_prefix_before_card_acceptance() {
    use crate::eventlog::{Event, EventKind};
    for shape in ["empty", "prefix", "second-root", "two-roots", "gap"] {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let ap = Announce::new_with_account(&alice, &Account::generate(), "Alice", 9);
        let (b, _, mut callbacks) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &ap,
        );
        let conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
        let count = if shape == "gap" { 3 } else { 2 };
        let manifest = crate::file::FileManifest {
            name: "prefix.bin".into(),
            size: crate::file::CHUNK_SIZE as u64 * u64::from(count - 1) + 1,
            mime: "application/octet-stream".into(),
            checksum: [1; 32],
            file_key: [3; 32],
            file_conv: ConversationId::new([42; 32]),
            chunk_count: count,
        };
        let chunk = |seq, parents| {
            Event::new(
                &alice,
                manifest.file_conv,
                seq,
                parents,
                seq,
                99,
                EventKind::Message,
                crate::file::seal_chunk(
                    &crate::file::FileKey::from_bytes(manifest.file_key),
                    b"valid encrypted chunk",
                )
                .unwrap(),
            )
        };
        if shape == "second-root" {
            b.log
                .lock()
                .unwrap()
                .append_durable(chunk(2, vec![]))
                .unwrap();
        } else if shape != "empty" {
            let first = chunk(1, vec![]);
            b.log.lock().unwrap().append_durable(first.clone()).unwrap();
            if shape == "two-roots" {
                b.log
                    .lock()
                    .unwrap()
                    .append_durable(chunk(2, vec![]))
                    .unwrap();
            } else if shape == "gap" {
                b.log
                    .lock()
                    .unwrap()
                    .append_durable(chunk(3, vec![first.id]))
                    .unwrap();
            }
        }
        let event = Event::new(
            &alice,
            conv,
            1,
            vec![],
            1,
            100,
            EventKind::FileManifest,
            crate::dm::seal(&alice, &b.identity.public().x25519_pub, &manifest.encode()).unwrap(),
        );
        let row = ReceivedEntry {
            event_id: event.id,
            conversation: conv,
            from: alice.user_id(),
            wall_clock: 100,
            plaintext: manifest.encode(),
        };
        let accepted = b.accept_manifest_event(&event, &ap, row).is_ok();
        assert_eq!(
            accepted,
            matches!(shape, "empty" | "prefix"),
            "shape {shape}"
        );
        assert_eq!(
            b.files
                .lock()
                .unwrap()
                .manifest(manifest.file_conv)
                .unwrap()
                .is_some(),
            accepted
        );
        assert_eq!(
            b.delivery.lock().unwrap().retry_receipts(10).len(),
            usize::from(accepted)
        );
        assert_eq!(
            b.received_files.lock().unwrap().entries(&conv).len(),
            usize::from(accepted)
        );
        assert_eq!(callbacks.try_recv().is_ok(), accepted);
        assert!(b.delivery.lock().unwrap().pending_transactions().is_empty());
    }
}

#[test]
fn file_book_rebuild_requires_valid_manifest_row_and_retained_signed_original() {
    use crate::eventlog::{Event, EventKind};
    for shape in [
        "valid-account-host",
        "missing",
        "message",
        "wrong-from",
        "wrong-time",
        "invalid-metadata",
        "wire-collision",
        "host-collision",
        "conflicting-alias",
    ] {
        for reopen in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let alice = DeviceIdentity::generate();
            let ap = Announce::new_with_account(&alice, &Account::generate(), "Alice", 9);
            let identity = DeviceIdentity::generate();
            let secret = identity.secret_bytes();
            let account = Account::generate();
            let account_secret = account.secret_bytes();
            let (b, _, _) = node_with_files(dir.path(), identity, account, &ap);
            let wire =
                super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
            let host = super::conversation::account_conversation_id(
                &b.account_id(),
                &ap.account_id().unwrap(),
            );
            let mut manifest = crate::file::FileManifest {
                name: "retained.txt".into(),
                size: 0,
                mime: "text/plain".into(),
                checksum: crate::file::file_checksum(&[]),
                file_key: [3; 32],
                file_conv: ConversationId::new([43; 32]),
                chunk_count: 1,
            };
            if shape == "invalid-metadata" {
                manifest.chunk_count = 2;
            }
            if shape == "wire-collision" {
                manifest.file_conv = wire;
            }
            if shape == "host-collision" {
                manifest.file_conv = host;
            }
            let event = Event::new(
                &alice,
                wire,
                1,
                vec![],
                1,
                100,
                if shape == "message" {
                    EventKind::Message
                } else {
                    EventKind::FileManifest
                },
                crate::dm::seal(&alice, &b.identity.public().x25519_pub, &manifest.encode())
                    .unwrap(),
            );
            if shape != "missing" {
                b.log.lock().unwrap().append_durable(event.clone()).unwrap();
            }
            let row = ReceivedEntry {
                event_id: event.id,
                conversation: host,
                from: if shape == "wrong-from" {
                    b.user_id()
                } else {
                    alice.user_id()
                },
                wall_clock: if shape == "wrong-time" { 101 } else { 100 },
                plaintext: manifest.encode(),
            };
            b.received_files
                .lock()
                .unwrap()
                .record_durable(&row)
                .unwrap();
            let mut conflicting_id = None;
            if shape == "conflicting-alias" {
                let mut alias = manifest.clone();
                alias.file_key = [9; 32];
                let second = Event::new(
                    &alice,
                    wire,
                    2,
                    vec![event.id],
                    2,
                    101,
                    EventKind::FileManifest,
                    crate::dm::seal(&alice, &b.identity.public().x25519_pub, &alias.encode())
                        .unwrap(),
                );
                b.log
                    .lock()
                    .unwrap()
                    .append_durable(second.clone())
                    .unwrap();
                b.received_files
                    .lock()
                    .unwrap()
                    .record_durable(&ReceivedEntry {
                        event_id: second.id,
                        conversation: host,
                        from: alice.user_id(),
                        wall_clock: 101,
                        plaintext: alias.encode(),
                    })
                    .unwrap();
                conflicting_id = Some(second.id);
            }
            let (b, mut callbacks) = if reopen {
                drop(b);
                let (b, _, rx) = node_with_files(
                    dir.path(),
                    DeviceIdentity::from_secret_bytes(secret.0, secret.1),
                    Account::from_secret_bytes(account_secret),
                    &ap,
                );
                (b, Some(rx))
            } else {
                b.reseed_live_file_book(&b.delivery.lock().unwrap())
                    .unwrap();
                (b, None)
            };
            let book = b.files.lock().unwrap();
            let expected = matches!(shape, "valid-account-host" | "conflicting-alias");
            assert_eq!(
                book.manifest(manifest.file_conv).unwrap().is_some(),
                expected,
                "{shape}, reopen={reopen}"
            );
            assert_eq!(
                book.is_emitted(&event.id),
                expected,
                "{shape}, reopen={reopen}"
            );
            if let Some(id) = conflicting_id {
                assert!(
                    !book.is_emitted(&id),
                    "conflicting alias must not suppress reprocessing"
                );
                assert!(super::files::same_file_transfer(
                    &book.manifest(manifest.file_conv).unwrap().unwrap(),
                    &crate::file::AnyManifest::V1(manifest.clone())
                ));
            }
            drop(book);
            if let Some(rx) = callbacks.as_mut() {
                assert!(rx.try_recv().is_err(), "startup must stay silent");
            }
        }
    }
}

#[test]
fn file_book_rebuild_does_not_restore_outgoing_card_scope_from_mismatched_row() {
    let dir = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let path = dir.path().join("source.txt");
    std::fs::write(&path, b"source").unwrap();
    let (manifest, file) = a
        .stage_file(&path, crate::file::FileKind::File, |_| {})
        .unwrap();
    let target = a.roster.lock().unwrap().peers()[0].clone();
    let host =
        super::conversation::account_conversation_id(&a.account_id(), &proof.account_id().unwrap());
    let id = a
        .accept_staged_manifest(&[target], host, proof.account_id(), &manifest)
        .unwrap();
    let mut row = a
        .received_files
        .lock()
        .unwrap()
        .entry(id)
        .unwrap()
        .unwrap()
        .clone();
    row.wall_clock += 1;
    {
        let mut rows = a.received_files.lock().unwrap();
        rows.remove_where(|_| true).unwrap();
        rows.record_durable(&row).unwrap();
    }
    a.configure_privacy(
        dir.path(),
        "pw",
        &a.signed_announce("Source", 9),
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    assert!(a.files.lock().unwrap().manifest(file).unwrap().is_none());
    assert!(!a.has_file_scope(file).unwrap());
}

#[test]
fn durable_file_erase_removes_live_key_even_when_sidecar_rewrite_fails() {
    use crate::eventlog::{ConversationId, Event, EventKind};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let ap = Announce::new_with_account(&alice, &Account::generate(), "Alice", 9);
    let (b, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &ap,
    );
    let conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
    let manifest = crate::file::FileManifest {
        name: "erase.txt".into(),
        size: 0,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(&[]),
        file_key: [3; 32],
        file_conv: ConversationId::new([4; 32]),
        chunk_count: 1,
    };
    let event = Event::new(
        &alice,
        conv,
        1,
        vec![],
        1,
        100,
        EventKind::FileManifest,
        crate::dm::seal(&alice, &b.identity.public().x25519_pub, &manifest.encode()).unwrap(),
    );
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.process_file_events(conv);
    b.received_files
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    assert!(
        b.delete_account_message(&ap.account_id().unwrap(), event.id)
            .is_err()
    );
    assert!(
        b.account_history(&ap.account_id().unwrap(), 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        b.files
            .lock()
            .unwrap()
            .manifest(manifest.file_conv)
            .unwrap()
            .is_none(),
        "durable erase must remove key despite later sidecar failure"
    );
}

#[test]
fn legacy_file_erasure_capacity_refusal_preserves_durable_row_and_key() {
    use super::delivery_store::{DeliveryLimits, DeliveryStore};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let ap = Announce::new_with_account(&alice, &Account::generate(), "Alice", 9);
    let (b, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &ap,
    );
    let host =
        super::conversation::account_conversation_id(&b.account_id(), &ap.account_id().unwrap());
    let manifest = crate::file::FileManifest {
        name: "legacy.txt".into(),
        size: 0,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(&[]),
        file_key: [3; 32],
        file_conv: crate::eventlog::ConversationId::new([4; 32]),
        chunk_count: 1,
    };
    let wire_conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
    let event = crate::eventlog::Event::new(
        &alice,
        wire_conv,
        1,
        vec![],
        1,
        1,
        crate::eventlog::EventKind::FileManifest,
        crate::dm::seal(&alice, &b.identity.public().x25519_pub, &manifest.encode()).unwrap(),
    );
    let id = event.id;
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.received_files
        .lock()
        .unwrap()
        .record_file_with_reception(
            &ReceivedEntry {
                event_id: id,
                conversation: host,
                from: alice.user_id(),
                wall_clock: 1,
                plaintext: manifest.encode(),
            },
            Some(&event),
            None,
        )
        .unwrap();
    let mut store = DeliveryStore::open_with_limits(
        &dir.path().join("tight"),
        "pw",
        DeliveryLimits {
            completed_receipts: 0,
            ..DeliveryLimits::default()
        },
    )
    .unwrap();
    store
        .bind_profile(
            &dir.path().join("events.log"),
            "pw",
            &b.identity.public(),
            &b.account_id(),
        )
        .unwrap();
    b.reseed_live_file_book(&store).unwrap();
    *b.delivery.lock().unwrap() = store;
    assert!(
        b.delete_account_message(&ap.account_id().unwrap(), id)
            .is_err()
    );
    assert!(
        !b.account_suppression
            .lock()
            .unwrap()
            .contains(host, id)
            .unwrap(),
        "a refused erase must not commit a visibility tombstone"
    );
    assert_eq!(
        b.account_history(&ap.account_id().unwrap(), 10).unwrap()[0].id,
        id
    );
    assert!(
        b.files
            .lock()
            .unwrap()
            .manifest(manifest.file_conv)
            .unwrap()
            .is_some()
    );
    assert_eq!(b.received_files.lock().unwrap().entries(&host).len(), 1);
}

#[tokio::test]
async fn deleting_one_file_alias_keeps_other_host_live_then_bulk_clear_removes_last_key() {
    use crate::eventlog::{ConversationId, Event, EventKind};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let carol = DeviceIdentity::generate();
    let ca = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let cp = Announce::new_with_account(&carol, &ca, "Carol", 9);
    let (b, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &ap,
    );
    let secret = b.identity.secret_bytes();
    let account_secret = b.account.secret_bytes();
    b.roster
        .lock()
        .unwrap()
        .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    b.configure_privacy(
        dir.path(),
        "pw",
        &b.signed_announce("Bob", 9),
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    b.set_invisible(true).await.unwrap();
    b.initiate_contact_locally(&aa.account_id()).await.unwrap();
    b.initiate_contact_locally(&ca.account_id()).await.unwrap();
    b.set_attachment_global_auto(false).unwrap();
    let manifest = crate::file::FileManifest {
        name: "alias.txt".into(),
        size: 0,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(&[]),
        file_key: [3; 32],
        file_conv: ConversationId::new([4; 32]),
        chunk_count: 1,
    };
    let mut originals = Vec::new();
    for signer in [&alice, &carol] {
        let conv = super::conversation::dm_conversation_id(&signer.public(), &b.identity.public());
        let event = Event::new(
            signer,
            conv,
            1,
            vec![],
            1,
            100,
            EventKind::FileManifest,
            crate::dm::seal(signer, &b.identity.public().x25519_pub, &manifest.encode()).unwrap(),
        );
        b.log.lock().unwrap().append_durable(event.clone()).unwrap();
        b.process_file_events(conv);
        originals.push(event);
    }
    assert_eq!(b.account_history(&aa.account_id(), 10).unwrap().len(), 1);
    assert_eq!(b.account_history(&ca.account_id(), 10).unwrap().len(), 1);
    assert_eq!(
        b.attachment_consent_status(manifest.file_conv).unwrap(),
        Some(crate::node::ReceiveDecision::Pending)
    );
    b.set_attachment_contact_auto(&ca.account_id(), Some(true))
        .unwrap();
    assert_eq!(
        b.attachment_consent_status(manifest.file_conv).unwrap(),
        Some(crate::node::ReceiveDecision::Pending),
        "one automatic alias must not hide the other sender's approval controls"
    );
    b.decide_attachment(manifest.file_conv, true).unwrap();
    assert_eq!(
        b.attachment_consent_status(manifest.file_conv).unwrap(),
        Some(crate::node::ReceiveDecision::Accepted)
    );
    b.delete_account_message(&aa.account_id(), originals[0].id)
        .unwrap();
    {
        let scopes = b
            .manifest_scopes(manifest.file_conv)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            scopes.iter().all(|scope| scope.event != originals[0].id),
            "erased alias scope must be removed"
        );
        assert!(scopes.iter().any(|scope| scope.event == originals[1].id));
    }
    assert!(
        b.files
            .lock()
            .unwrap()
            .manifest(manifest.file_conv)
            .unwrap()
            .is_some()
    );
    assert_eq!(b.delivery.lock().unwrap().retry_receipts(10).len(), 1);
    drop(b);
    let (b, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
        &cp,
    );
    b.roster
        .lock()
        .unwrap()
        .update(&ap, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    b.configure_privacy(
        dir.path(),
        "pw",
        &b.signed_announce("Bob", 9),
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    b.process_file_events(originals[0].conversation_id);
    assert!(b.account_history(&aa.account_id(), 10).unwrap().is_empty());
    assert_eq!(
        b.account_history(&ca.account_id(), 10).unwrap()[0].id,
        originals[1].id
    );
    assert!(
        b.files
            .lock()
            .unwrap()
            .manifest(manifest.file_conv)
            .unwrap()
            .is_some()
    );
    assert_eq!(b.prune_older_than(101).unwrap(), 1);
    assert!(
        b.files
            .lock()
            .unwrap()
            .manifest(manifest.file_conv)
            .unwrap()
            .is_none()
    );
    assert!(b.delivery.lock().unwrap().retry_receipts(10).is_empty());
}

#[tokio::test]
async fn offline_file_card_has_durable_awaiting_id_and_no_text_history_row() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let bp = Announce::new_with_account(&bob, &Account::generate(), "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &bp,
    );
    let path = dir.path().join("empty.txt");
    std::fs::write(&path, []).unwrap();
    let file_conv = a
        .send_file_dm(&bob.user_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let history = a.dm_history(&bob.public(), 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].file.as_ref().unwrap().file_conv, file_conv);
    assert_eq!(
        a.delivery_status(history[0].id),
        Some(DeliveryStatus::Awaiting)
    );
    assert!(a.sentlog.lock().unwrap().conversations().is_empty());
}

#[test]
fn large_manifest_uses_compact_wire_format_before_staging_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let bp = Announce::new_with_account(&bob, &Account::generate(), "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &bp,
    );
    let size = 4 * 1024 * 1024 * 1024;
    let prospective = crate::file::FileManifestV3 {
        v2: crate::file::FileManifestV2 {
            name: "large-folder.tar".into(),
            size,
            mime: crate::file::directory::MIME.into(),
            checksum: [0; 32],
            file_key: [0; 32],
            file_nonce: [0; 8],
            file_conv: ConversationId::new([0; 32]),
            chunk_size: crate::file::CHUNK_SIZE as u32,
            chunk_count: crate::file::chunk_count_for(size),
            chunk_hashes: vec![],
        },
        kind: crate::file::FileKind::File,
    };
    let before = a.log.lock().unwrap().conversations().unwrap();
    assert!(super::files::compact_manifest_needed(&a.identity, &prospective).unwrap());
    assert!(
        super::files::compact_manifest_needed(
            &a.identity,
            &crate::file::FileManifestV3 {
                v2: crate::file::FileManifestV2 {
                    size: 1024,
                    chunk_count: 1,
                    ..prospective.v2.clone()
                },
                ..prospective.clone()
            }
        )
        .is_ok_and(|compact| !compact)
    );
    assert_eq!(a.log.lock().unwrap().conversations().unwrap(), before);
}

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
    use crate::eventlog::{Event, EventKind, sync::SyncStore};
    {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
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
                    recipient_account: Some(bob_account.account_id()),
                    conversation: conv,
                    wall_clock: 10,
                    destinations: vec![DeliveryDestination {
                        device: bob.public(),
                        account: Some(bob_account.account_id()),
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
            assert!(
                a.sync_store(&bob.public())
                    .lock()
                    .unwrap()
                    .ingest(competing)
                    .is_err()
            );
        } else {
            assert_eq!(a.append_event(conv, EventKind::React, vec![2]).unwrap(), 2);
        }
        let mut delivery = a.delivery.lock().unwrap();
        a.recover_delivery(&mut delivery).unwrap();
        assert!(delivery.message(original.id).is_some());
        assert!(
            a.log
                .lock()
                .unwrap()
                .events(&conv)
                .unwrap()
                .iter()
                .any(|e| e.id == original.id)
        );
    }
}

#[tokio::test]
async fn accepted_account_file_card_carries_fixed_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let remote = Account::generate();
    let proof = Announce::new_with_account(&DeviceIdentity::generate(), &remote, "Bob", 1);
    let (mut node, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let path = dir.path().join("secret.txt");
    std::fs::write(&path, b"secret").unwrap();
    let deadline = super::node::now_millis() + 300_000;
    let (id, _) = node
        .enqueue_file_to_account_progress_expiring_if(
            &remote.account_id(),
            &path,
            crate::file::FileKind::File,
            |_| {},
            Some(deadline),
            |accept| accept(),
        )
        .await
        .unwrap();
    let row = node
        .received_files
        .lock()
        .unwrap()
        .entry(id)
        .unwrap()
        .unwrap()
        .clone();
    assert_eq!(crate::expiry::deadline(&row.plaintext), Some(deadline));
    assert!(crate::file::decode_manifest(&row.plaintext).is_some());
    assert_eq!(
        node.account_history(&remote.account_id(), 10).unwrap()[0].expires_at_ms,
        Some(deadline)
    );
    Arc::get_mut(&mut node).unwrap().expiry_clock_start = (std::time::Instant::now(), deadline + 1);
    assert!(
        node.account_history(&remote.account_id(), 10)
            .unwrap()
            .is_empty()
    );
    assert!(node.prune_expired_received().unwrap() > 0);
    assert!(
        node.received_files
            .lock()
            .unwrap()
            .entry(id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn expired_pending_acceptance_erases_text_and_file_before_sidecar_recovery() {
    for file in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let remote = Account::generate();
        let proof = Announce::new_with_account(&DeviceIdentity::generate(), &remote, "Bob", 9);
        let (mut sender, _) = node(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let deadline = super::node::now_millis() + 300_000;
        if file {
            sender
                .received_files
                .lock()
                .unwrap()
                .fail_appends_for_test(true)
                .unwrap();
        } else {
            sender
                .sentlog
                .lock()
                .unwrap()
                .fail_appends_for_test(true)
                .unwrap();
        }
        let id = if file {
            let path = dir.path().join("pending.txt");
            std::fs::write(&path, b"pending attachment").unwrap();
            sender
                .enqueue_file_to_account_progress_expiring_if(
                    &remote.account_id(),
                    &path,
                    crate::file::FileKind::File,
                    |_| {},
                    Some(deadline),
                    |accept| accept(),
                )
                .await
                .unwrap()
                .0
        } else {
            sender
                .enqueue_to_account_expiring_if(
                    &remote.account_id(),
                    b"pending text",
                    None,
                    Some(deadline),
                    |accept| accept(),
                )
                .await
                .unwrap()
        };
        assert_eq!(
            sender.delivery.lock().unwrap().pending_transactions().len(),
            1
        );
        assert_eq!(
            sender.account_history(&remote.account_id(), 10).unwrap()[0].id,
            id
        );
        sender
            .received_files
            .lock()
            .unwrap()
            .fail_appends_for_test(false)
            .unwrap();
        sender
            .sentlog
            .lock()
            .unwrap()
            .fail_appends_for_test(false)
            .unwrap();
        // The deadline passes while the accepted WAL is still the only plaintext copy.
        Arc::get_mut(&mut sender).unwrap().expiry_clock_start =
            (std::time::Instant::now(), deadline + 1);
        assert!(sender.prune_expired_received().unwrap() > 0);
        assert!(
            sender
                .account_history(&remote.account_id(), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(sender.delivery_status(id), None);
        assert!(
            sender
                .delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        assert!(sender.sentlog.lock().unwrap().conversations().is_empty());
        assert!(
            sender
                .received_files
                .lock()
                .unwrap()
                .conversations()
                .is_empty()
        );
        assert!(sender.files.lock().unwrap().file_convs().is_empty());
        assert_eq!(sender.prune_expired_received().unwrap(), 0);
    }
}

#[test]
fn outgoing_expiry_rejects_past_and_unbounded_deadlines() {
    use super::expiry_policy::validate_outgoing;
    let now = super::node::now_millis();
    assert!(validate_outgoing(None).is_ok());
    assert!(validate_outgoing(Some(now + 300_000)).is_ok());
    assert!(
        matches!(validate_outgoing(Some(0)), Err(NodeError::InvalidInput(reason)) if reason == "expiration time has passed")
    );
    assert!(
        matches!(validate_outgoing(Some(u64::MAX)), Err(NodeError::InvalidInput(reason)) if reason == "expiration time is too far away")
    );
}

#[tokio::test]
async fn expiring_account_message_arrives_over_loopback_and_expires_on_both_sides() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (mut a, _) = node(ad.path(), alice, aa, &bp);
    let (mut b, mut incoming) = node(bd.path(), bob, bob_account, &ap);
    let deadline = super::node::now_millis() + 300_000;
    let id = a
        .enqueue_to_account_expiring_if(
            &b.account_id(),
            b"expires on both sides",
            None,
            Some(deadline),
            |accept| accept(),
        )
        .await
        .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let received = tokio::time::timeout(std::time::Duration::from_secs(8), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.text, b"expires on both sides");
    assert_eq!(
        a.account_history(&b.account_id(), 10).unwrap()[0].expires_at_ms,
        Some(deadline)
    );
    assert_eq!(
        b.account_history(&a.account_id(), 10).unwrap()[0].expires_at_ms,
        Some(deadline)
    );
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    // JoinSet abort requests are asynchronous, and a blocking cleanup may still
    // own the node after the accept-loop future has finished. Wait for teardown
    // before changing the test clock through exclusive ownership.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while Arc::strong_count(&a) != 1 || Arc::strong_count(&b) != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("connection and cleanup tasks must release both nodes");
    Arc::get_mut(&mut a).unwrap().expiry_clock_start = (std::time::Instant::now(), deadline + 1);
    Arc::get_mut(&mut b).unwrap().expiry_clock_start = (std::time::Instant::now(), deadline + 1);
    assert!(a.account_history(&b.account_id(), 10).unwrap().is_empty());
    assert!(b.account_history(&a.account_id(), 10).unwrap().is_empty());
    assert!(a.prune_expired_received().unwrap() > 0);
    assert!(b.prune_expired_received().unwrap() > 0);
    let conv = super::conversation::account_conversation_id(&a.account_id(), &b.account_id());
    assert!(a.sent_entries_for_history(conv).is_empty());
    assert!(b.received.lock().unwrap().entries(&conv).is_empty());
    assert!(
        a.account_suppression
            .lock()
            .unwrap()
            .contains(conv, id)
            .unwrap()
    );
}

#[tokio::test]
async fn protected_account_message_arrives_redacted_and_exact_reveal_expires() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (mut a, _) = node(ad.path(), alice, aa, &bp);
    let (mut b, mut incoming) = node(bd.path(), bob, bob_account, &ap);
    let deadline = super::node::now_millis() + 300_000;
    let id = a
        .enqueue_protected_to_account_expiring_if(
            &b.account_id(),
            b"expires on both sides",
            None,
            Some(deadline),
            super::MessageProtection::Authenticate,
            |accept| accept(),
        )
        .await
        .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let received = tokio::time::timeout(std::time::Duration::from_secs(8), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(received.text.is_empty());
    assert_eq!(
        received.protection,
        Some(super::MessageProtection::Authenticate)
    );
    let ar = super::SearchReference::Account {
        account_id: b.account_id(),
        message_id: Some(id),
    };
    let br = super::SearchReference::Account {
        account_id: a.account_id(),
        message_id: Some(id),
    };
    for (node, reference) in [(&a, &ar), (&b, &br)] {
        assert_eq!(
            node.protected_message_content(reference).unwrap(),
            Some((
                super::MessageProtection::Authenticate,
                b"expires on both sides".to_vec()
            ))
        );
        assert!(node.search("expires").unwrap().is_empty());
        assert!(node.search_recent("expires").unwrap().hits.is_empty());
        assert!(
            node.search_context(reference).unwrap().unwrap().messages[0]
                .text
                .is_empty()
        );
    }
    assert_eq!(
        a.account_history(&b.account_id(), 10).unwrap()[0].expires_at_ms,
        Some(deadline)
    );
    assert_eq!(
        b.account_history(&a.account_id(), 10).unwrap()[0].expires_at_ms,
        Some(deadline)
    );
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    // JoinSet abort requests are asynchronous, and a blocking cleanup may still
    // own the node after the accept-loop future has finished. Wait for teardown
    // before changing the test clock through exclusive ownership.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while Arc::strong_count(&a) != 1 || Arc::strong_count(&b) != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("connection and cleanup tasks must release both nodes");
    Arc::get_mut(&mut a).unwrap().expiry_clock_start = (std::time::Instant::now(), deadline + 1);
    Arc::get_mut(&mut b).unwrap().expiry_clock_start = (std::time::Instant::now(), deadline + 1);
    assert!(a.protected_message_content(&ar).unwrap().is_none());
    assert!(b.protected_message_content(&br).unwrap().is_none());
    assert!(a.account_history(&b.account_id(), 10).unwrap().is_empty());
    assert!(b.account_history(&a.account_id(), 10).unwrap().is_empty());
    assert!(a.prune_expired_received().unwrap() > 0);
    assert!(b.prune_expired_received().unwrap() > 0);
    let conv = super::conversation::account_conversation_id(&a.account_id(), &b.account_id());
    assert!(a.sent_entries_for_history(conv).is_empty());
    assert!(b.received.lock().unwrap().entries(&conv).is_empty());
    assert!(
        a.account_suppression
            .lock()
            .unwrap()
            .contains(conv, id)
            .unwrap()
    );
}

#[tokio::test]
async fn accepted_account_message_carries_fixed_deadline_inside_encrypted_body() {
    let dir = tempfile::tempdir().unwrap();
    let remote = Account::generate();
    let proof = Announce::new_with_account(&DeviceIdentity::generate(), &remote, "Bob", 1);
    let (node, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let deadline = super::node::now_millis() + 300_000;
    let id = node
        .enqueue_to_account_expiring_if(
            &remote.account_id(),
            b"secret",
            None,
            Some(deadline),
            |accept| accept(),
        )
        .await
        .unwrap();
    let conv =
        super::conversation::account_conversation_id(&node.account_id(), &remote.account_id());
    let sent = node.sentlog.lock().unwrap().entries(&conv);
    let env = super::dm_envelope::DmEnvelope::decode(&sent[0].plaintext).unwrap();
    assert_eq!(crate::expiry::deadline(&env.body), Some(deadline));
    assert_eq!(
        super::queries::decode_account_entry(&sent[0].plaintext).0,
        id
    );
}

fn node(
    dir: &Path,
    identity: DeviceIdentity,
    account: Account,
    proof: &Announce,
) -> (Arc<Node>, mpsc::UnboundedReceiver<ReceivedDm>) {
    let (node, messages, _) = node_with_files(dir, identity, account, proof);
    (node, messages)
}

async fn sync_delivery_test_stage(
    source: &Arc<Node>,
    destination: &Arc<Node>,
    listener: &TcpListener,
    conversation: ConversationId,
) {
    // Joining borrowed futures keeps cancellation owned by the enclosing deadline.
    let mut timing = super::delivery_runtime::TestTiming::new("card-control-stage");
    let accept = async {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            // Permission publication may have left a closed best-effort dial
            // queued before this independently driven stage starts accepting.
            if let Ok(channel) = destination.privacy_accept(stream).await {
                destination.serve_connection(channel).await;
                break;
            }
        }
    };
    let send = async {
        let mut timing = super::delivery_runtime::TestTiming::new("card-control-dial");
        let mut channel = source
            .privacy_dial(
                listener.local_addr().unwrap(),
                &destination.identity.public(),
            )
            .await
            .unwrap();
        timing.phase("card-control-request-round");
        super::session::request_round(
            &mut channel,
            &source.sync_store(&destination.identity.public()),
            conversation,
        )
        .await
        .unwrap();
        drop(channel);
        source.emit_new_messages(conversation);
        timing.finish();
    };
    tokio::join!(accept, send);
    timing.finish();
}

async fn wait_for_durable_delivery(node: &Node, id: EventId) {
    let mut timing = super::delivery_runtime::TestTiming::new("receipt-durable-wait-3s");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let notified = node.delivery_status_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if node.delivery_status(id) == Some(DeliveryStatus::Delivered) {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("authenticated receipt durably installed within 3s");
    timing.finish();
}

fn node_with_files(
    dir: &Path,
    identity: DeviceIdentity,
    account: Account,
    proof: &Announce,
) -> (
    Arc<Node>,
    mpsc::UnboundedReceiver<ReceivedDm>,
    mpsc::UnboundedReceiver<ReceivedFile>,
) {
    let (node, rx, files) = super::test_support::node_with_files(dir, identity, account, proof);
    // Legacy delivery tests exercise the previous automatic-transfer contract.
    // The consent-specific loopback test keeps the new manual default.
    node.set_attachment_global_auto(true).unwrap();
    (node, rx, files)
}

#[tokio::test]
async fn automatic_receipt_roundtrip_with_both_original_peers_stopped_in_turn() {
    automatic_offline_roundtrip(false, false).await;
}

#[tokio::test]
async fn automatic_receipt_reuses_exact_ack_after_relay_eviction() {
    automatic_offline_roundtrip(true, true).await;
}

#[tokio::test]
async fn private_post_office_projects_authorized_other_peer_receipt_pairs_only() {
    use crate::eventlog::{Event, EventKind, sync::SyncStore};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
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
        &bob_account.account_id(),
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
    assert!(
        p.sync_store(&alice.public())
            .lock()
            .unwrap()
            .ingest(ack.clone())
            .is_err()
    );
    p.set_allowed(&bob_account.account_id(), true)
        .await
        .unwrap();
    assert!(
        p.sync_store(&alice.public())
            .lock()
            .unwrap()
            .ingest(ack.clone())
            .is_ok()
    );
    assert_eq!(
        p.sync_store(&bob.public())
            .lock()
            .unwrap()
            .event_ids(&control)
            .unwrap(),
        vec![ack.id]
    );
    assert!(
        p.sync_store(&alice.public())
            .lock()
            .unwrap()
            .durable_have(&control, &ack.id)
            .unwrap()
    );
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
    assert!(
        p.sync_store(&alice.public())
            .lock()
            .unwrap()
            .ingest(forged)
            .is_err()
    );
    p.set_allowed(&bob_account.account_id(), false)
        .await
        .unwrap();
    assert!(
        p.sync_store(&alice.public())
            .lock()
            .unwrap()
            .event_ids(&control)
            .unwrap()
            .is_empty()
    );
    assert!(
        !p.sync_store(&alice.public())
            .lock()
            .unwrap()
            .durable_have(&control, &ack.id)
            .unwrap()
    );
}

#[tokio::test]
async fn automatic_worker_stalled_fanout_does_not_starve_other_messages_and_drop_cancels() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let extra = DeviceIdentity::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
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
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
    let first = a
        .enqueue_to_account(&b.account_id(), b"first fair", None)
        .await
        .unwrap();
    let second = a
        .enqueue_to_account(&b.account_id(), b"second fair", None)
        .await
        .unwrap();
    // Prime each message's destination cursor so both first turns target the
    // stalled sibling. Random event IDs must not hide same-peer lane starvation.
    {
        let mut store = a.delivery.lock().unwrap();
        for (id, predecessor) in [(first, second), (second, first)] {
            let mut recipient_selected = false;
            for _ in 0..2 {
                let (selected, destination) = store.next_destination(Some(predecessor)).unwrap();
                assert_eq!(selected, id);
                if destination.device == b.identity.public() {
                    recipient_selected = true;
                    break;
                }
            }
            assert!(recipient_selected);
        }
    }
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
    assert_eq!(b.account_history(&a.account_id(), 10).unwrap().len(), 2);
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
            let bob_account = Account::generate();
            let bkeys = bob.secret_bytes();
            let baccount = bob_account.secret_bytes();
            let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
            let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
            let (a, _) = node(ad.path(), alice, aa, &bp);
            let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
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
                    .event_ids(&control)
                    .unwrap(),
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
                assert!(
                    b.sync_store(&ap.public())
                        .lock()
                        .unwrap()
                        .event_ids(&control)
                        .unwrap()
                        .is_empty()
                );
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
                assert!(
                    b.sync_store(&ap.public())
                        .lock()
                        .unwrap()
                        .event_ids(&control)
                        .unwrap()
                        .is_empty()
                );
            } else {
                if private {
                    b.set_allowed(&own, false).await.unwrap();
                    assert!(
                        b.sync_store(&ap.public())
                            .lock()
                            .unwrap()
                            .event_ids(&control)
                            .unwrap()
                            .is_empty()
                    );
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
                assert!(
                    b.sync_store(&ap.public())
                        .lock()
                        .unwrap()
                        .event_ids(&control)
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    !b.sync_store(&ap.public())
                        .lock()
                        .unwrap()
                        .durable_have(&control, &receipt.destination.event.id)
                        .unwrap()
                );
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
                assert!(
                    b.sync_store(&substituted.public())
                        .lock()
                        .unwrap()
                        .event_ids(&control)
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }
}

async fn automatic_offline_roundtrip(evict: bool, bridge_enabled: bool) {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let akeys = alice.secret_bytes();
    let aaccount = aa.secret_bytes();
    let bkeys = bob.secret_bytes();
    let baccount = bob_account.secret_bytes();
    let target = bob_account.account_id();
    let own = aa.account_id();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (initial_bob, _) = node(bd.path(), bob, bob_account, &ap);
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
    a.set_bridge_discovery_enabled(bridge_enabled);
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
        .clone();
    let mailbox =
        crate::postoffice::opaque::RelayMailbox::new(&a.identity, &bp.public(), &pp.public())
            .unwrap();
    let outer_id = mailbox.wrap(&original).unwrap().id;
    let federated_outer_id =
        crate::postoffice::opaque::RelayMailbox::federated(&a.identity, &bp.public())
            .unwrap()
            .wrap(&original)
            .unwrap()
            .id;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !relay.lock().unwrap().has(&outer_id)
            || (bridge_enabled && !relay.lock().unwrap().has(&federated_outer_id))
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        relay.lock().unwrap().has(&federated_outer_id),
        bridge_enabled
    );
    assert!(!relay.lock().unwrap().has(&original.id));
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
    b.replace_verified_dm_pull_peers(vec![ap.public().user_id()]);
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
    assert_eq!(b.account_history(&own, 10).unwrap().len(), 1);
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
    let ack = b.log.lock().unwrap().events(&control).unwrap()[0].clone();
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
    a.replace_verified_dm_pull_peers(vec![bp.public().user_id()]);
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
    assert_eq!(a.account_history(&target, 10).unwrap()[0].id, id);
    assert_eq!(a.account_history(&target, 10).unwrap().len(), 1);
    assert!(arx.try_recv().is_err());
    if let Some(b) = b {
        assert_eq!(
            b.log.lock().unwrap().events(&control).unwrap(),
            vec![ack.clone()]
        );
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
    let bob_account = Account::generate();
    let proof = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    assert!(
        a.send_dm(&bob.public().user_id(), b"offline intent")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn real_post_office_acceptance_keeps_certified_account_delivery_awaiting() {
    let dir = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "Offline Bob",
        dead.local_addr().unwrap().port(),
    );
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
        .enqueue_to_account(&bob_account.account_id(), b"relay is not recipient", None)
        .await
        .unwrap();
    let original = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    a.flush_delivery(id).await;
    let mailbox =
        crate::postoffice::opaque::RelayMailbox::new(&a.identity, &bob.public(), &proof.public())
            .unwrap();
    assert!(
        relay
            .lock()
            .unwrap()
            .has(&mailbox.wrap(&original).unwrap().id)
    );
    assert!(!relay.lock().unwrap().has(&original.id));
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn file_post_office_waits_for_recipient_consent_after_early_card_ack() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp_port = bl.local_addr().unwrap().port();
    drop(bl);
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bp_port);
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, bob_account, &ap);
    let path = ad.path().join("resume.bin");
    let bytes = vec![6; crate::file::CHUNK_SIZE + 1];
    std::fs::write(&path, &bytes).unwrap();
    let (id, file) = a
        .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let manifest = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    b.log
        .lock()
        .unwrap()
        .append_durable(manifest.clone())
        .unwrap();
    b.process_file_events(manifest.conversation_id);
    let ack = b.delivery.lock().unwrap().retry_receipts(1)[0]
        .destination
        .event
        .clone();
    let control_conv = ack.conversation_id;
    a.log.lock().unwrap().append_durable(ack).unwrap();
    a.emit_new_messages(control_conv);
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    let upload = a.outgoing_file_progress(id).unwrap();
    assert_eq!((upload.done, upload.total, upload.delivered), (0, 2, false));
    assert!(b.read_file(file).is_err());
    let final_chunk = a
        .delivery
        .lock()
        .unwrap()
        .file_card(id)
        .unwrap()
        .final_chunk
        .unwrap();
    let po = DeviceIdentity::generate();
    let keys = po.secret_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proof = Announce::new_post_office(&po, "Relay", listener.local_addr().unwrap().port());
    let relay = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(&pd.path().join("relay"), "pw", po).unwrap(),
    ));
    let task = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        listener,
        relay.clone(),
    ));
    a.roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    a.flush_file_delivery(id).await;
    assert!(
        !relay.lock().unwrap().has(&final_chunk),
        "offline relay must not receive attachment chunks before recipient consent"
    );
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_some(),
        "qualified relay custody cannot retire target chunk work"
    );
    task.abort();
    let _ = task.await;
    drop(relay);
    let bl = TcpListener::bind(("127.0.0.1", bp_port)).await.unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let resumed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !b
            .read_file(file)
            .as_ref()
            .is_ok_and(|actual| actual == &bytes)
            || a.delivery
                .lock()
                .unwrap()
                .next_file_destination(None)
                .is_some()
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    assert!(resumed.is_ok());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    assert_eq!(
        a.outgoing_file_progress(id).unwrap(),
        super::OutgoingFileProgress {
            done: 2,
            total: 2,
            delivered: true,
            consent: None,
        }
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
}

#[tokio::test]
async fn consented_file_chunks_fall_back_to_live_relay_before_recipient_reconnects() {
    let mut timing = super::delivery_runtime::TestTiming::new("relay-file-fixture-setup");
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _, mut offers) = node_with_files(bd.path(), bob, bob_account, &ap);
    b.set_attachment_global_auto(false).unwrap();
    b.replace_verified_dm_pull_peers(vec![ap.public().user_id()]);

    let po = DeviceIdentity::generate();
    let keys = po.secret_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proof = Announce::new_post_office(&po, "Relay", listener.local_addr().unwrap().port());
    let relay = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(&pd.path().join("relay"), "pw", po).unwrap(),
    ));
    let relay_task = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        listener,
        relay.clone(),
    ));
    for peer in [&a, &b] {
        peer.roster.lock().unwrap().update(
            &proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &peer.user_id(),
        );
    }

    let source = ad.path().join("offline-file.bin");
    let bytes = vec![7; crate::file::CHUNK_SIZE + 17];
    std::fs::write(&source, &bytes).unwrap();
    timing.phase("relay-file-enqueue-card");
    let (card, file) = a
        .enqueue_file_to_account(&b.account_id(), &source, crate::file::FileKind::File)
        .await
        .unwrap();
    timing.phase("relay-file-flush-card");
    // The recipient does not serve application frames yet: both original
    // direct card/receipt attempts must fail, leaving the live relay to carry
    // the offer. Complete real Noise handshakes and close immediately instead
    // of stalling at listeners until two unrelated authentication deadlines.
    let setup_eof = std::cell::Cell::new(0usize);
    tokio::select! {
        _ = a.flush_delivery(card) => {}
        _ = async {
            loop {
                let (stream, _) = bl.accept().await.unwrap();
                let channel = b.privacy_accept(stream).await.unwrap();
                assert_eq!(channel.peer_identity(), &a.identity.public());
                drop(channel);
                setup_eof.set(setup_eof.get() + 1);
            }
        } => unreachable!("the scoped authenticated-EOF responder does not return"),
    }
    assert_eq!(
        setup_eof.get(),
        2,
        "both direct best-effort legs closed before the relay supplies the offer"
    );
    timing.phase("relay-file-drain-offer");
    b.drain_from_post_office().await;
    let offer = tokio::time::timeout(std::time::Duration::from_secs(5), offers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offer.file_conv, file);
    b.decide_attachment(file, true).unwrap();

    timing.phase("relay-file-consent-exchange");
    let recipient_task = tokio::spawn(b.clone().run_accept_loop(bl));
    let recipient = a.routing_peer(&b.user_id()).unwrap();
    let mut channel = a
        .privacy_dial(recipient.addr, &recipient.public)
        .await
        .unwrap();
    let binding = a
        .delivery
        .lock()
        .unwrap()
        .file_card(card)
        .unwrap()
        .destinations[0]
        .binding
        .event_id;
    let status = super::session::request_file_consent(&mut channel, file, binding)
        .await
        .unwrap();
    assert_eq!(status, crate::eventlog::sync::FileConsentStatus::Accepted);
    a.outgoing_file_consents
        .lock()
        .unwrap()
        .insert((card, binding), status);
    drop(channel);
    recipient_task.abort();
    let _ = recipient_task.await;

    // A stale preferred relay must not strand consented chunks while another
    // authenticated relay on this LAN is accepting mailbox rounds.
    let offline_relay = DeviceIdentity::generate();
    let offline_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let offline_proof = Announce::new_post_office(
        &offline_relay,
        "Offline preferred",
        offline_listener.local_addr().unwrap().port(),
    );
    drop(offline_listener);
    a.roster.lock().unwrap().update(
        &offline_proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &a.user_id(),
    );
    a.set_bridge_hub_id(Some(&offline_relay.user_id())).unwrap();

    assert!(
        b.read_file(file).is_err(),
        "recipient was offline during upload"
    );
    let final_chunk = a
        .delivery
        .lock()
        .unwrap()
        .file_card(card)
        .unwrap()
        .final_chunk
        .unwrap();
    let mailbox = crate::postoffice::opaque::RelayMailbox::new(
        &a.identity,
        &b.identity.public(),
        &proof.public(),
    )
    .unwrap();
    let outer_final = mailbox
        .wrap(&a.log.lock().unwrap().get(&final_chunk).unwrap().unwrap())
        .unwrap()
        .id;
    // The immediate flush API is one bounded best-effort turn. Exercise the
    // retained production worker until the live fallback actually owns the bytes.
    timing.phase("relay-file-closed-endpoint-fallback-custody");
    let worker = tokio::spawn(a.clone().run_delivery_loop());
    // This asserts eventual exact custody, not a five-second latency contract.
    // Windows can spend two seconds rejecting each closed loopback endpoint.
    let custody = tokio::time::timeout(super::route_timing::MAX_AUTHENTICATION_DEADLINE, async {
        while !relay.lock().unwrap().has(&outer_final) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    timing.phase("relay-file-custody-stop-worker");
    worker.abort();
    let _ = worker.await;
    assert!(
        custody.is_ok(),
        "live fallback must retain the exact encrypted final chunk"
    );
    assert!(
        b.read_file(file).is_err(),
        "recipient remains offline throughout upload"
    );
    assert!(
        !a.outgoing_file_progress(card).unwrap().delivered,
        "relay custody is not recipient completion"
    );
    assert!(!relay.lock().unwrap().has(&final_chunk));

    timing.phase("relay-file-recipient-drain");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            b.drain_from_post_office().await;
            if b.read_file(file)
                .as_ref()
                .is_ok_and(|actual| actual == &bytes)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    timing.phase("relay-file-stop-relay");
    relay_task.abort();
    let _ = relay_task.await;
    timing.finish();
}

/// Real LAN custody tests require the installed relay's signed device ID.
/// The sender has no accept loop, so its file worker cannot race the
/// controlled consent and offline handoffs below.
#[tokio::test]
#[ignore = "requires installed mac2015 post office on the same LAN"]
async fn mac2015_post_office_delivers_file_bytes_after_both_peers_go_offline() {
    let service = std::env::var("MESH_TALK_E2E_MAC2015_SERVICE").ok();
    installed_post_office_delivers_file_bytes_after_both_peers_go_offline(
        "MESH_TALK_E2E_MAC2015_RELAY_ID",
        "mac2015",
        MacRelayService::parse(service.as_deref()).restart_command(),
    )
    .await;
}

#[derive(Debug, PartialEq)]
enum MacRelayService {
    LegacyAgent,
    System,
}

impl MacRelayService {
    fn parse(value: Option<&str>) -> Self {
        match value {
            None | Some("legacy-agent") => Self::LegacyAgent,
            Some("system") => Self::System,
            Some(other) => panic!(
                "invalid MESH_TALK_E2E_MAC2015_SERVICE={other:?}; use system or legacy-agent"
            ),
        }
    }

    fn restart_command(&self) -> &'static str {
        match self {
            Self::LegacyAgent => {
                "launchctl kickstart -k gui/$(id -u)/com.octopusgarage.mesh-talk.post-office"
            }
            Self::System => {
                "sudo -n launchctl kickstart -k system/com.octopusgarage.mesh-talk.post-office"
            }
        }
    }
}

#[test]
fn mac_relay_service_selector_preserves_legacy_and_requires_explicit_system() {
    assert_eq!(MacRelayService::parse(None), MacRelayService::LegacyAgent);
    assert_eq!(
        MacRelayService::parse(Some("legacy-agent")),
        MacRelayService::LegacyAgent
    );
    assert_eq!(
        MacRelayService::parse(Some("system")),
        MacRelayService::System
    );
    assert!(
        MacRelayService::System
            .restart_command()
            .starts_with("sudo -n launchctl")
    );
    assert!(std::panic::catch_unwind(|| MacRelayService::parse(Some("typo"))).is_err());
}

#[tokio::test]
#[ignore = "requires installed windows10 post office on the same LAN"]
async fn windows10_post_office_delivers_file_bytes_after_both_peers_go_offline() {
    installed_post_office_delivers_file_bytes_after_both_peers_go_offline(
        "MESH_TALK_E2E_WINDOWS10_RELAY_ID",
        "windows10",
        "powershell -NoProfile -Command \"Stop-ScheduledTask -TaskName MeshTalk-PostOffice; Start-Sleep -Seconds 2; Start-ScheduledTask -TaskName MeshTalk-PostOffice\"",
    )
    .await;
}

async fn installed_post_office_delivers_file_bytes_after_both_peers_go_offline(
    relay_id_env: &str,
    relay_host: &str,
    restart_command: &str,
) {
    use crate::discovery::announce::decode;

    let relay_id = std::env::var(relay_id_env)
        .unwrap_or_else(|_| panic!("set {relay_id_env} to the installed relay device ID"));
    let socket = crate::transport::net::discovery_socket(47474).unwrap();
    let (relay_proof, relay_ip) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut buf = [0u8; 2048];
        loop {
            let (len, source) = socket.recv_from(&mut buf).await.unwrap();
            if let Some(proof) = decode(&buf[..len])
                && proof.verify()
                && proof.post_office
                && proof.user_id == relay_id
            {
                break (proof, source.ip());
            }
        }
    })
    .await
    .expect("installed relay's signed LAN announce was not received");

    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "File Alice", 1);
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "File Bob",
        bl.local_addr().unwrap().port(),
    );
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _, mut offers) = node_with_files(bd.path(), bob, bob_account, &ap);
    b.set_attachment_global_auto(false).unwrap();
    b.replace_verified_dm_pull_peers(vec![ap.public().user_id()]);
    for peer in [&a, &b] {
        peer.roster
            .lock()
            .unwrap()
            .update(&relay_proof, relay_ip, &peer.user_id());
        assert_eq!(
            super::postbox::elected_post_office(&peer.roster.lock().unwrap())
                .unwrap()
                .public
                .user_id(),
            relay_id
        );
    }

    let source = ad.path().join("remote-relay-file.bin");
    let bytes = (0..(crate::file::CHUNK_SIZE + 37))
        .map(|n| (n % 251) as u8)
        .collect::<Vec<_>>();
    std::fs::write(&source, &bytes).unwrap();
    let (card, file) = a
        .enqueue_file_to_account(&b.account_id(), &source, crate::file::FileKind::File)
        .await
        .unwrap();
    // Bob has no listener yet. The offer must reach the remote relay.
    a.flush_delivery(card).await;
    b.drain_from_post_office().await;
    let offer = tokio::time::timeout(std::time::Duration::from_secs(10), offers.recv())
        .await
        .expect("recipient did not receive offline offer from installed relay")
        .expect("file offer channel closed");
    assert_eq!(offer.file_conv, file);
    assert!(!b.file_ready_to_save(file).unwrap());
    b.decide_attachment(file, true).unwrap();

    // Authenticate a live recipient's consent, then close their only listener.
    // No file chunk is sent on the consent channel.
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let recipient = a.routing_peer(&b.user_id()).unwrap();
    let mut channel = a
        .privacy_dial(recipient.addr, &recipient.public)
        .await
        .unwrap();
    let binding = a
        .delivery
        .lock()
        .unwrap()
        .file_card(card)
        .unwrap()
        .destinations[0]
        .binding
        .event_id;
    let status = super::session::request_file_consent(&mut channel, file, binding)
        .await
        .unwrap();
    assert_eq!(status, crate::eventlog::sync::FileConsentStatus::Accepted);
    a.outgoing_file_consents
        .lock()
        .unwrap()
        .insert((card, binding), status);
    drop(channel);
    bt.abort();
    let _ = bt.await;
    assert!(!b.file_ready_to_save(file).unwrap());
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());

    // Confirm every encrypted chunk at the relay before removing the only sender.
    // A best-effort flush turn alone is not a complete-upload barrier.
    let final_chunk = a
        .delivery
        .lock()
        .unwrap()
        .file_card(card)
        .unwrap()
        .final_chunk
        .unwrap();
    let mailbox = crate::postoffice::opaque::RelayMailbox::new(
        &a.identity,
        &bp.public(),
        &relay_proof.public(),
    )
    .unwrap();
    let outer_chunks = a
        .log
        .lock()
        .unwrap()
        .events(&file)
        .unwrap()
        .into_iter()
        .map(|event| mailbox.wrap(&event).unwrap().id)
        .collect::<Vec<_>>();
    assert!(!outer_chunks.is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            a.flush_file_delivery(card).await;
            let mut channel = a
                .privacy_dial(
                    (relay_ip, relay_proof.tcp_port).into(),
                    &relay_proof.public(),
                )
                .await
                .unwrap();
            let mut complete = true;
            for outer in &outer_chunks {
                if !super::session::request_durable_have(&mut channel, mailbox.id(), *outer)
                    .await
                    .unwrap_or(false)
                {
                    complete = false;
                    break;
                }
            }
            if complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("installed relay never confirmed custody of all encrypted chunks");
    assert!(
        !a.outgoing_file_progress(card).unwrap().delivered,
        "relay custody must not claim recipient completion"
    );
    assert!(!b.file_ready_to_save(file).unwrap());
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
    drop(a);

    // The relay is restarted while it alone holds the bytes, then the recipient
    // pulls and verifies them with the sender absent.
    let restart = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            relay_host,
            restart_command,
        ])
        .status()
        .expect("ssh to installed relay failed");
    assert!(restart.success(), "installed relay restart failed");
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            b.drain_from_post_office().await;
            if b.read_file(file)
                .as_ref()
                .is_ok_and(|actual| actual == &bytes)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("installed relay did not redeliver complete file bytes after restart");
    assert_eq!(b.read_file(file).unwrap(), bytes);
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap(),
        "recovered chunks alone must not record a verified save"
    );
    let destination = bd.path().join("recovered.bin");
    b.save_file(file, &destination).unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(
        Sha256::digest(std::fs::read(&destination).unwrap()),
        Sha256::digest(&bytes)
    );
    assert!(
        b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap(),
        "verified save must retain exact-file completion while sender remains stopped"
    );
}

#[tokio::test]
async fn media_prune_before_final_probe_does_not_force_chunk_retransmission_or_residue() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let akeys = alice.secret_bytes();
    let aaccount = aa.secret_bytes();
    let bkeys = bob.secret_bytes();
    let baccount = bob_account.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, bob_account, &ap);
    let mut bytes = vec![6; 1024];
    bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let path = ad.path().join("preview.png");
    std::fs::write(&path, &bytes).unwrap();
    let (id, file) = a
        .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::Media)
        .await
        .unwrap();
    let manifest = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    b.log
        .lock()
        .unwrap()
        .append_durable(manifest.clone())
        .unwrap();
    b.process_file_events(manifest.conversation_id);
    // Serve actual authenticated pulls/ACKs, but do not start sender file retries
    // until the receiver's normal media-save path has durably copied/pruned.
    let source = a.clone();
    let at = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = al.accept() => { let (stream, _) = accepted.unwrap(); let source = source.clone();
                    connections.spawn(async move { if let Ok(channel) = source.privacy_accept(stream).await { source.serve_connection(channel).await; } }); }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    let acknowledged = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(acknowledged.is_ok());
    assert!(b.read_file(file).is_err());
    b.pull_pending_files().await;
    assert_eq!(b.read_media(file).unwrap(), bytes);
    let progress = b.file_progress(file).unwrap().unwrap();
    assert!(progress.total > 0 && progress.done == progress.total);
    assert!(b.file_ready_to_save(file).unwrap());
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_some()
    );
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    let final_chunk = a
        .delivery
        .lock()
        .unwrap()
        .file_card(id)
        .unwrap()
        .final_chunk
        .unwrap();
    drop(a);
    drop(b);
    let alice = DeviceIdentity::from_secret_bytes(akeys.0, akeys.1);
    let aa = Account::from_secret_bytes(aaccount);
    let bob = DeviceIdentity::from_secret_bytes(bkeys.0, bkeys.1);
    let bob_account = Account::from_secret_bytes(baccount);
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, bob_account, &ap);
    assert_eq!(b.read_media(file).unwrap(), bytes);
    assert!(
        b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    assert!(
        !b.historical_file_completion(file, EventId::new([1; 32]), &ap.public())
            .unwrap()
    );
    assert!(
        !b.historical_file_completion(file, final_chunk, &DeviceIdentity::generate().public())
            .unwrap()
    );
    let exported = b.save_file_into_dir(file, bd.path()).unwrap();
    assert_eq!(std::fs::read(exported).unwrap(), bytes);
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    a.flush_file_delivery(id).await;
    bt.abort();
    let _ = bt.await;
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_none()
    );
    assert!(
        b.log.lock().unwrap().events(&file).unwrap().is_empty(),
        "completed/pruned media must not be retransmitted and retained merely to satisfy exact final custody"
    );
    std::fs::remove_file(b.media.path(file).unwrap()).unwrap();
    assert!(!b.file_ready_to_save(file).unwrap());
}

async fn completed_file_fixture(
    ad: &Path,
    bd: &Path,
    kind: crate::file::FileKind,
) -> (
    Arc<Node>,
    Arc<Node>,
    Announce,
    Announce,
    EventId,
    ConversationId,
    EventId,
    Vec<u8>,
) {
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(ad, alice, aa, &bp);
    let (b, _) = node(bd, bob, bob_account, &ap);
    let bytes = vec![7; crate::file::CHUNK_SIZE + 11];
    let path = ad.join("completion.bin");
    std::fs::write(&path, &bytes).unwrap();
    let (id, file) = a
        .enqueue_file_to_account(&b.account_id(), &path, kind)
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
    b.process_file_events(original.conversation_id);
    let chunks: Vec<_> = a
        .log
        .lock()
        .unwrap()
        .events(&file)
        .unwrap()
        .into_iter()
        .collect();
    let final_chunk = chunks.last().unwrap().id;
    for chunk in chunks {
        b.log.lock().unwrap().append_durable(chunk).unwrap();
    }
    (a, b, ap, bp, id, file, final_chunk, bytes)
}

#[tokio::test]
async fn generic_save_completion_prunes_and_retires_without_second_transfer_but_keeps_source_fanout()
 {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (a, b, ap, _, id, file, final_chunk, bytes) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    let source_copy = ad.path().join("source-export.bin");
    a.save_file(file, &source_copy).unwrap();
    assert_eq!(std::fs::read(source_copy).unwrap(), bytes);
    assert!(!a.log.lock().unwrap().events(&file).unwrap().is_empty());
    let exported = b.save_file_into_dir(file, bd.path()).unwrap();
    assert_eq!(std::fs::read(exported).unwrap(), bytes);
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
    assert!(
        b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proof = Announce::new_with_account(
        &b.identity,
        &b.account,
        "Bob",
        bl.local_addr().unwrap().port(),
    );
    a.roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let receiver = b.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = bl.accept().await.unwrap();
        let channel = receiver.privacy_accept(stream).await.unwrap();
        receiver.serve_connection(channel).await;
    });
    a.flush_file_delivery(id).await;
    task.abort();
    let _ = task.await;
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_none()
    );
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
    assert_eq!(b.file_progress(file).unwrap().unwrap().done, 0);
    assert!(!b.file_ready_to_save(file).unwrap());
    // Local file-save proofs have no status authority: the queued card ACK is separate.
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    a.save_file(file, &ad.path().join("source-after-retirement.bin"))
        .unwrap();
    assert!(
        a.log.lock().unwrap().events(&file).unwrap().is_empty(),
        "source chunks become reclaimable once all immutable fanout retires"
    );
}

#[tokio::test]
async fn save_and_completion_metadata_filesystem_failures_keep_chunks_and_no_historical_truth() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (_, b, ap, _, _, file, final_chunk, bytes) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    let blocked = bd.path().join("blocked");
    std::fs::write(&blocked, []).unwrap();
    assert!(b.save_file(file, &blocked.join("file.bin")).is_err());
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
    let outbox = bd.path().join("events.log.delivery-outbox");
    let moved = bd.path().join("outbox-held");
    std::fs::rename(&outbox, &moved).unwrap();
    let destination = bd.path().join("saved.bin");
    b.save_file(file, &destination).unwrap();
    assert_eq!(std::fs::read(destination).unwrap(), bytes);
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
    std::fs::rename(moved, outbox).unwrap();
    b.save_file(file, &bd.path().join("saved-again.bin"))
        .unwrap();
    assert!(
        b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
}

#[tokio::test]
async fn same_ciphertext_signed_by_wrong_chunk_author_cannot_create_completion_or_prune() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (_, b, ap, _, _, file, final_chunk, bytes) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    let chunks: Vec<_> = b
        .log
        .lock()
        .unwrap()
        .events(&file)
        .unwrap()
        .into_iter()
        .collect();
    b.log.lock().unwrap().drop_conversation(&file).unwrap();
    let stranger = DeviceIdentity::generate();
    let mut previous = None;
    for chunk in chunks {
        let forged = crate::eventlog::Event::new(
            &stranger,
            file,
            chunk.seq,
            previous.into_iter().collect(),
            chunk.lamport,
            chunk.wall_clock,
            chunk.kind,
            chunk.ciphertext,
        );
        previous = Some(forged.id);
        b.log.lock().unwrap().append_durable(forged).unwrap();
    }
    // Chunk AEAD/checksum alone do not establish who originated the signed chain.
    assert_eq!(b.read_file(file).unwrap(), bytes);
    b.save_file(file, &bd.path().join("saved.bin")).unwrap();
    assert!(!b.has_verified_file_completion(file));
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    assert!(!b.log.lock().unwrap().events(&file).unwrap().is_empty());
}

fn size_mismatch_file_fixture(
    directory: &Path,
    declared_size: u64,
    kind: crate::file::FileKind,
    version_three: bool,
) -> (Arc<Node>, Announce, ConversationId, EventId) {
    use crate::eventlog::{Event, EventKind};
    let source = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&source, &Account::generate(), "Source", 9);
    let (receiver, _) = node(
        directory,
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let bytes = b"ab";
    let file = ConversationId::new([83; 32]);
    let manifest = crate::file::FileManifestV3 {
        v2: crate::file::FileManifestV2 {
            name: "size-mismatch.png".into(),
            size: declared_size,
            mime: "image/png".into(),
            checksum: crate::file::file_checksum(bytes),
            file_key: [41; 32],
            file_nonce: [42; 8],
            file_conv: file,
            chunk_size: crate::file::CHUNK_SIZE as u32,
            chunk_count: 1,
            chunk_hashes: vec![crate::file::chunk_hash(bytes)],
        },
        kind,
    };
    let encoded = if version_three {
        manifest.encode()
    } else {
        manifest.v2.encode()
    };
    let host =
        super::conversation::dm_conversation_id(&source.public(), &receiver.identity.public());
    let original = Event::new(
        &source,
        host,
        1,
        vec![],
        1,
        100,
        EventKind::FileManifest,
        crate::dm::seal(&source, &receiver.identity.public().x25519_pub, &encoded).unwrap(),
    );
    receiver
        .accept_manifest_event(
            &original,
            &proof,
            ReceivedEntry {
                event_id: original.id,
                conversation: host,
                from: source.user_id(),
                wall_clock: original.wall_clock,
                plaintext: encoded,
            },
        )
        .unwrap();
    let chunk = Event::new(
        &source,
        file,
        1,
        vec![],
        1,
        101,
        EventKind::Message,
        crate::file::seal_chunk_indexed(&manifest.v2.key(), &manifest.v2.file_nonce, 0, bytes)
            .unwrap(),
    );
    let final_chunk = chunk.id;
    receiver.log.lock().unwrap().append_durable(chunk).unwrap();
    assert_eq!(receiver.completion_candidates(file).unwrap().len(), 1);
    (receiver, proof, file, final_chunk)
}

#[test]
fn initial_file_save_rejects_signed_content_shorter_or_longer_than_declared_size() {
    for declared_size in [1, 3] {
        for version_three in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (receiver, proof, file, final_chunk) = size_mismatch_file_fixture(
                directory.path(),
                declared_size,
                crate::file::FileKind::File,
                version_three,
            );
            let destination = directory.path().join("saved.bin");
            assert!(receiver.save_file(file, &destination).is_err());
            assert!(!destination.exists());
            assert!(!directory.path().join("saved.bin.part").exists());
            assert!(receiver.read_file(file).is_err());
            assert!(!receiver.has_verified_file_completion(file));
            assert!(
                !receiver
                    .historical_file_completion(file, final_chunk, &proof.public())
                    .unwrap()
            );
            assert_eq!(receiver.log.lock().unwrap().events(&file).unwrap().len(), 1);
        }
    }
}

#[test]
fn initial_media_completion_rejects_signed_content_shorter_or_longer_than_declared_size() {
    for declared_size in [1, 3] {
        for version_three in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (receiver, proof, file, final_chunk) = size_mismatch_file_fixture(
                directory.path(),
                declared_size,
                crate::file::FileKind::Media,
                version_three,
            );
            assert!(!receiver.persist_media_if_complete(file));
            assert!(receiver.read_file(file).is_err());
            assert!(!receiver.media.contains(file));
            assert!(!receiver.has_verified_file_completion(file));
            assert!(
                !receiver
                    .historical_file_completion(file, final_chunk, &proof.public())
                    .unwrap()
            );
            assert_eq!(receiver.log.lock().unwrap().events(&file).unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn historical_completion_requires_current_exact_origin_and_owner_and_permission() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (a, b, ap, bp, _, file, final_chunk, _) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    b.configure_privacy(
        bd.path(),
        "pw",
        &bp,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    b.set_invisible(true).await.unwrap();
    b.set_allowed(&ap.account_id().unwrap(), true)
        .await
        .unwrap();
    b.save_file(file, &bd.path().join("saved.bin")).unwrap();
    assert!(
        b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    let mut wrong_x = ap.public();
    wrong_x.x25519_pub = [9; 32];
    assert!(
        !b.historical_file_completion(file, final_chunk, &wrong_x)
            .unwrap()
    );
    b.set_allowed(&ap.account_id().unwrap(), false)
        .await
        .unwrap();
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    b.set_allowed(&ap.account_id().unwrap(), true)
        .await
        .unwrap();
    let rebound_account = Account::generate();
    let rebound = Announce::new_with_account(&a.identity, &rebound_account, "Rebound Alice", 9);
    b.roster
        .lock()
        .unwrap()
        .update(&rebound, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    b.initiate_contact(&rebound_account.account_id())
        .await
        .unwrap();
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    let account = Account::generate();
    let keys = b.identity.secret_bytes();
    b.persist_account_adoption(&account.public(), || Ok(()), || Ok(b.account.public()))
        .unwrap();
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    drop(b);
    let (b, _) = node(
        bd.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        account,
        &ap,
    );
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
}

#[tokio::test]
async fn completion_alias_erasure_keeps_other_alias_and_failed_row_cleanup_never_keeps_proof() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (a, b, ap, _, id, file, final_chunk, _) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    let original = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    let alias = crate::eventlog::Event::new(
        &a.identity,
        original.conversation_id,
        original.seq + 1,
        vec![original.id],
        original.lamport + 1,
        original.wall_clock + 1,
        crate::eventlog::EventKind::FileManifest,
        original.ciphertext.clone(),
    );
    b.log.lock().unwrap().append_durable(alias.clone()).unwrap();
    b.process_file_events(alias.conversation_id);
    b.save_file(file, &bd.path().join("saved.bin")).unwrap();
    let host = super::conversation::account_conversation_id(&b.account_id(), &a.account_id());
    assert_eq!(
        b.delivery
            .lock()
            .unwrap()
            .completed_files(file, final_chunk)
            .len(),
        2
    );
    b.delete_message(host, id, false).unwrap();
    assert!(
        b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    b.received_files
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    assert!(b.delete_message(host, alias.id, false).is_err());
    assert!(
        b.received_files
            .lock()
            .unwrap()
            .entry(alias.id)
            .unwrap()
            .is_some()
    );
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
}

#[tokio::test]
async fn legacy_device_only_media_completion_qualifies_exact_origin_without_account_credit() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let ap = Announce::new(&alice, "Alice", 9);
    let bp = Announce::new(&bob, "Bob", 9);
    let (a, _) = node(ad.path(), alice, Account::generate(), &bp);
    let (b, _) = node(bd.path(), bob, Account::generate(), &ap);
    let path = ad.path().join("legacy.png");
    let bytes = vec![8; 1024];
    std::fs::write(&path, &bytes).unwrap();
    let (id, file) = a
        .enqueue_file_dm(&bp.public().user_id(), &path, crate::file::FileKind::Media)
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
    b.process_file_events(original.conversation_id);
    let chunks: Vec<_> = a
        .log
        .lock()
        .unwrap()
        .events(&file)
        .unwrap()
        .into_iter()
        .collect();
    let final_chunk = chunks.last().unwrap().id;
    for chunk in chunks {
        b.log.lock().unwrap().append_durable(chunk).unwrap();
    }
    assert!(b.persist_media_if_complete(file));
    assert_eq!(b.read_media(file).unwrap(), bytes);
    assert!(
        b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    assert!(
        !b.historical_file_completion(file, final_chunk, &DeviceIdentity::generate().public())
            .unwrap()
    );
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new(&b.identity, "Bob", bl.local_addr().unwrap().port());
    a.roster
        .lock()
        .unwrap()
        .update(&bp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let receiver = b.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = bl.accept().await.unwrap();
        let channel = receiver.privacy_accept(stream).await.unwrap();
        receiver.serve_connection(channel).await;
    });
    a.flush_file_delivery(id).await;
    task.abort();
    let _ = task.await;
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_none()
    );
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    let rebound = Announce::new_with_account(&a.identity, &Account::generate(), "Alice", 9);
    b.roster
        .lock()
        .unwrap()
        .update(&rebound, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
    let own = Announce::new_with_account(&b.identity, &b.account, "Bob", 9);
    b.configure_privacy(
        bd.path(),
        "pw",
        &own,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    b.set_invisible(true).await.unwrap();
    assert!(
        !b.historical_file_completion(file, final_chunk, &ap.public())
            .unwrap()
    );
}

#[tokio::test]
async fn channel_media_source_keeps_signed_chunks_when_local_preview_completes() {
    for sharing in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let other = DeviceIdentity::generate();
        let proof = Announce::new(&other, "Other", 9);
        let (sender, _) = node(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = sender
            .create_channel("Photos", vec![other.public()])
            .await
            .unwrap();
        if !sharing {
            sender
                .set_channel_replica_enabled(channel, false)
                .await
                .unwrap();
        }
        let bytes = [b"\x89PNG\r\n\x1a\n".as_slice(), &vec![3; 128 * 1024]].concat();
        let path = dir.path().join("source.png");
        std::fs::write(&path, &bytes).unwrap();
        let file = sender
            .send_file_channel(channel, &path, crate::file::FileKind::Media)
            .await
            .unwrap();
        // An incoming file fingerprint is a handled sync frame and invokes
        // this callback before the requester asks for its first chunk page.
        sender.process_file_events(file);
        assert_eq!(sender.read_media(file).unwrap(), bytes);
        assert_eq!(
            sender.file_progress(file).unwrap().unwrap().done,
            crate::file::chunk_count_for(bytes.len() as u64),
            "a local preview must not reclaim the source's signed channel photo before members fetch it; sharing={sharing}"
        );
    }
}

#[tokio::test]
async fn untracked_channel_media_local_export_verifies_retained_row_and_managed_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let other = DeviceIdentity::generate();
    let proof = Announce::new(&other, "Other", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let channel = a.create_channel("local", vec![]).await.unwrap();
    let bytes = vec![5; 2048];
    let path = dir.path().join("channel.png");
    std::fs::write(&path, &bytes).unwrap();
    let file = a
        .send_file_channel(channel, &path, crate::file::FileKind::Media)
        .await
        .unwrap();
    assert!(a.persist_media_if_complete(file));
    assert!(a.log.lock().unwrap().events(&file).unwrap().is_empty());
    let progress = a.file_progress(file).unwrap().unwrap();
    assert!(progress.total > 0 && progress.done == progress.total);
    assert!(a.file_ready_to_save(file).unwrap());
    assert!(!a.has_verified_file_completion(file));
    let exported = a.save_file_into_dir(file, dir.path()).unwrap();
    assert_eq!(std::fs::read(exported).unwrap(), bytes);
    let cached = a.media.path(file).unwrap();
    std::fs::write(&cached, b"wrong bytes").unwrap();
    assert!(a.file_ready_to_save(file).unwrap()); // A present eligible copy is attempted, then verified by save.
    assert!(
        a.save_file(file, &dir.path().join("bad-export.png"))
            .is_err()
    );
    std::fs::remove_file(cached).unwrap();
    assert!(!a.file_ready_to_save(file).unwrap());
}

#[tokio::test]
async fn durable_enqueue_is_awaiting_without_transport_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let proof = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let secret = alice.secret_bytes();
    let acct = aa.secret_bytes();
    let target = bob_account.account_id();
    let (a, _) = node(dir.path(), alice, aa, &proof);
    let id = a
        .enqueue_to_account(&target, b"durable offline", None)
        .await
        .unwrap();
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.account_history(&target, 10).unwrap()[0].id, id);
    drop(a);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(acct),
        &proof,
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.account_history(&target, 10).unwrap().len(), 1);
}

#[tokio::test]
async fn actual_durable_receive_sends_control_receipt_without_chat_callback() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let target = bob_account.account_id();
    let own = aa.account_id();
    let sender_secret = alice.secret_bytes();
    let sender_account = aa.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, mut arx) = node(ad.path(), alice, aa, &bp);
    let (b, mut brx) = node(bd.path(), bob, bob_account, &ap);
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
    assert_eq!(b.account_history(&own, 10).unwrap()[0].text, b"confirmed");
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
    assert!(!a.log.lock().unwrap().events(&control).unwrap().is_empty());
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
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
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
            .unwrap()
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
    let bob_account = Account::generate();
    let target = bob_account.account_id();
    let proof = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    a.sentlog
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    let id = a
        .enqueue_to_account(&target, b"accepted once", None)
        .await
        .unwrap();
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.account_history(&target, 10).unwrap().len(), 1);
    assert_eq!(a.account_history(&target, 10).unwrap()[0].id, id);
    assert_eq!(
        a.account_history(&target, 10).unwrap()[0].text,
        b"accepted once"
    );
    let sessions = std::fs::read(dir.path().join("ratchet.sessions")).unwrap();
    let conv = super::conversation::dm_conversation_id(&a.identity.public(), &bob.public());
    let versions = a.log.lock().unwrap().version_vector(&conv).unwrap();
    assert!(
        a.append_event(conv, crate::eventlog::EventKind::React, vec![2])
            .is_err()
    );
    assert_eq!(
        a.log.lock().unwrap().version_vector(&conv).unwrap(),
        versions
    );
    assert!(
        a.enqueue_to_account(&target, b"must wait", None)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("ratchet.sessions")).unwrap(),
        sessions
    );
    let worker = tokio::spawn(a.clone().run_delivery_loop());
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert!(!a.delivery.lock().unwrap().pending_transactions().is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    a.sentlog
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !a.delivery.lock().unwrap().pending_transactions().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    let _ = worker.await;
    assert_eq!(a.account_history(&target, 10).unwrap().len(), 1);
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
    assert_eq!(a.delete_account_message(&target, id).unwrap(), 1);
    assert_eq!(a.delivery_status(id), None);
    assert!(a.account_history(&target, 10).unwrap().is_empty());
}

#[tokio::test]
async fn oversized_enqueue_rejects_before_durable_ratchet_or_history() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let proof = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let sessions = std::fs::read(dir.path().join("ratchet.sessions")).unwrap();
    assert!(
        a.enqueue_to_account(&bob_account.account_id(), &vec![7; 256 * 1024], None)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("ratchet.sessions")).unwrap(),
        sessions
    );
    assert!(
        a.account_history(&bob_account.account_id(), 10)
            .unwrap()
            .is_empty()
    );
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
}

#[tokio::test]
async fn failed_account_save_preserves_queue_and_successful_adoption_suspends_old_node() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let secret = alice.secret_bytes();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let proof = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(dir.path(), alice, aa, &proof);
    let id = a
        .enqueue_to_account(&bob_account.account_id(), b"keep on failed adoption", None)
        .await
        .unwrap();
    let new_account = Account::generate();
    let blocked_keystore = dir.path().join("blocked.keystore");
    std::fs::create_dir(&blocked_keystore).unwrap();
    assert!(
        a.persist_account_adoption(
            &new_account.public(),
            || {
                crate::identity::account_keystore::save(&blocked_keystore, "pw", &new_account)
                    .map_err(|e| NodeError::Channel(e.to_string()))
            },
            || Ok(a.account.public())
        )
        .is_err()
    );
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(a.delivery.lock().unwrap().retry_messages(10).len(), 1);
    a.persist_account_adoption(&new_account.public(), || Ok(()), || Ok(a.account.public()))
        .unwrap();
    assert!(
        a.enqueue_to_account(&bob_account.account_id(), b"old runtime must stop", None)
            .await
            .is_err()
    );
    drop(a);
    let (adopted, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        new_account,
        &proof,
    );
    assert_eq!(adopted.delivery_status(id), None);
    assert!(
        adopted
            .delivery
            .lock()
            .unwrap()
            .retry_messages(10)
            .is_empty()
    );
}

#[tokio::test]
async fn adoption_marker_before_account_save_reopens_old_queue_and_unmarked_nonempty_rebind_rejects()
 {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let secret = alice.secret_bytes();
    let aa = Account::generate();
    let own_secret = aa.secret_bytes();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let proof = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(dir.path(), alice, aa, &proof);
    let id = a
        .enqueue_to_account(&bob_account.account_id(), b"pre-save crash", None)
        .await
        .unwrap();
    let new_account = Account::generate();
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .bind_profile(
                &dir.path().join("events.log"),
                "pw",
                &a.identity.public(),
                &new_account.account_id()
            )
            .is_err()
    );
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
    assert_eq!(
        a.account_history(&bob_account.account_id(), 8).unwrap()[0].id,
        id
    );
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .bind_profile(
                &dir.path().join("events.log"),
                "pw",
                &a.identity.public(),
                &new_account.account_id()
            )
            .is_err()
    );
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
    assert!(
        reopened
            .bind_profile(
                &dir.path().join("a.log"),
                "pw",
                &b.public(),
                &"a".repeat(32)
            )
            .is_err()
    );
    assert!(
        reopened
            .bind_profile(
                &dir.path().join("a.log"),
                "pw",
                &a.public(),
                &"a".repeat(32)
            )
            .is_ok()
    );
}

#[tokio::test]
async fn receiver_write_failure_keeps_plaintext_intent_until_restart_and_no_early_receipt() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let secret = bob.secret_bytes();
    let account = bob_account.secret_bytes();
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
    let id = a
        .enqueue_to_account(&b.account_id(), b"recover recipient", None)
        .await
        .unwrap();
    let event = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.received
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    b.emit_new_messages(event.conversation_id);
    assert!(rx.try_recv().is_err());
    assert!(b.delivery.lock().unwrap().retry_receipts(10).is_empty());
    assert_eq!(b.delivery.lock().unwrap().pending_transactions().len(), 1);
    b.received
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    drop(b);
    let (b, mut rx) = node(
        bd.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account),
        &ap,
    );
    assert_eq!(
        b.account_history(&a.account_id(), 10).unwrap()[0].text,
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
    let bob_account = Account::generate();
    let secret = alice.secret_bytes();
    let account = aa.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let cp =
        Announce::new_with_account(&other, &aa, "Other Alice", cl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    a.roster
        .lock()
        .unwrap()
        .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let (b, mut brx) = node(bd.path(), bob, bob_account, &ap);
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
    let bob_account = Account::generate();
    let bob_secret = bob.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, mut arx) = node(ad.path(), alice, aa, &bp);
    let (b, mut brx) = node(bd.path(), bob, bob_account, &ap);
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
async fn queued_private_file_work_stops_on_permission_revoke_and_exact_account_rebind() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let keys = bob.secret_bytes();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _, mut files) = node_with_files(bd.path(), bob, bob_account, &ap);
    for (node, dir, own) in [(&a, ad.path(), &ap), (&b, bd.path(), &bp)] {
        node.configure_privacy(
            dir,
            "pw",
            own,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.set_invisible(true).await.unwrap();
    }
    b.initiate_contact_locally(&a.account_id()).await.unwrap();
    let path = ad.path().join("private.txt");
    std::fs::write(&path, b"private immutable file").unwrap();
    let (id, file) = a
        .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    a.set_allowed(&b.account_id(), false).await.unwrap();
    a.flush_delivery(id).await;
    a.flush_file_delivery(id).await;
    assert!(files.try_recv().is_err());
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    a.initiate_contact_locally(&b.account_id()).await.unwrap();
    let new_account = Account::generate();
    let new_proof = Announce::new_with_account(
        &DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        &new_account,
        "Rebound Bob",
        bp.tcp_port,
    );
    a.roster
        .lock()
        .unwrap()
        .update(&new_proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    a.initiate_contact_locally(&new_account.account_id())
        .await
        .unwrap();
    a.flush_delivery(id).await;
    a.flush_file_delivery(id).await;
    assert!(files.try_recv().is_err());
    assert!(b.log.lock().unwrap().events(&file).unwrap().is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert!(
        a.delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .is_some()
    );
    bt.abort();
    let _ = bt.await;
}

#[tokio::test]
async fn private_receipts_remain_projectable_after_clear_restart_and_account_adoption() {
    let mut case = super::delivery_runtime::TestTiming::new("receipt-case-initial");
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let bob_secret = bob.secret_bytes();
    let bob_account_secret = bob_account.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
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
    wait_for_durable_delivery(&a, first).await;
    case.phase("receipt-case-after-clear-restart");
    assert_eq!(a.delivery_status(first), Some(DeliveryStatus::Delivered));
    b.clear_account_conversation(&a.account_id()).unwrap();
    bt.abort();
    let _ = bt.await;
    drop(b);
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob = DeviceIdentity::from_secret_bytes(bob_secret.0, bob_secret.1);
    let bob_account = Account::from_secret_bytes(bob_account_secret);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (b, mut rx) = node(bd.path(), bob, bob_account, &ap);
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
    wait_for_durable_delivery(&a, second).await;
    case.phase("receipt-case-after-account-adoption");
    assert_eq!(a.delivery_status(second), Some(DeliveryStatus::Delivered));
    let stale_bp = bp.clone();
    let adopted = Account::generate();
    b.persist_account_adoption(
        &adopted.public(),
        || {
            crate::identity::account_keystore::save(
                &bd.path().join("account.keystore"),
                "pw",
                &adopted,
            )
            .map_err(|e| NodeError::Channel(e.to_string()))
        },
        || {
            crate::identity::account_keystore::load(&bd.path().join("account.keystore"), "pw")
                .map(|account| account.public())
                .map_err(|e| NodeError::Channel(e.to_string()))
        },
    )
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
    a.roster
        .lock()
        .unwrap()
        .update(&stale_bp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    // An old authenticated connection can publish the pre-adoption account
    // between discovery of the new proof and this explicit contact. Simulate
    // that interleaving above, then repeat the new signed announcement until
    // the grant observes it. The receipt assertion below remains the subject
    // of this test; stale roster entries must not make setup timing-dependent.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let outcome =
                a.roster
                    .lock()
                    .unwrap()
                    .update(&bp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
            assert!(outcome.accepted());
            match a.initiate_contact_locally(&b.account_id()).await {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => panic!("adopted account grant failed: {error}"),
            }
        }
    })
    .await
    .expect("fresh adopted account proof was never observed");
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
    wait_for_durable_delivery(&a, third).await;
    assert_eq!(a.delivery_status(third), Some(DeliveryStatus::Delivered));
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    case.finish();
}

#[tokio::test]
async fn relay_file_worker_preserves_progress_across_slow_mailbox_exchanges() {
    retained_relay_file_worker(true, 1).await;
}

#[tokio::test]
async fn relay_custody_releases_slots_for_more_than_eight_pending_files() {
    retained_relay_file_worker(false, 10).await;
}

async fn retained_relay_file_worker(slow: bool, count: u8) {
    use crate::eventlog::sync::SyncStore;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
    // A stale direct endpoint rejects Noise immediately on every OS. A closed
    // port can consume the full connect budget on Windows, making this batch
    // measure SYN/refusal timing instead of retained relay scheduling.
    let rejecting_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rejecting_listener_port = rejecting_listener.local_addr().unwrap().port();
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", rejecting_listener_port);
    let rejecting = tokio::spawn(async move {
        while let Ok((stream, _)) = rejecting_listener.accept().await {
            drop(stream);
        }
    });
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _, offers) = node_with_files(bd.path(), bob, bob_account, &ap);
    let mut recipients = vec![(b, offers, bp)];
    // Slot fairness needs ten destinations, independently of shared-mailbox
    // history size. Keep the slow single-file case on its original mailbox.
    for index in 1..count {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let proof =
            Announce::new_with_account(&identity, &account, "Recipient", rejecting_listener_port);
        let (recipient, _, offers) = node_with_files(
            &bd.path().join(format!("recipient-{index}")),
            identity,
            account,
            &ap,
        );
        a.roster
            .lock()
            .unwrap()
            .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
        recipients.push((recipient, offers, proof));
    }
    a.replace_verified_dm_pull_peers(
        recipients
            .iter()
            .map(|(_, _, proof)| proof.user_id.clone())
            .collect(),
    );
    for (recipient, _, _) in &recipients {
        recipient.replace_verified_dm_pull_peers(vec![ap.user_id.clone()]);
        recipient.set_attachment_global_auto(false).unwrap();
        recipient.set_bridge_discovery_enabled(true);
    }
    a.set_bridge_discovery_enabled(true);
    let relay_identity = DeviceIdentity::generate();
    let keys = relay_identity.secret_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proof = Announce::new_post_office(
        &relay_identity,
        "Relay",
        listener.local_addr().unwrap().port(),
    );
    // Observe qualified final-chunk custody while retaining the real relay
    // admission, durable storage, and reconciliation implementation.
    struct ObservedRelay {
        inner: crate::postoffice::PostOffice,
        final_chunks: Arc<Mutex<std::collections::HashMap<EventId, ConversationId>>>,
        confirmed_files: Arc<Mutex<std::collections::HashSet<ConversationId>>>,
        confirmations: Arc<AtomicUsize>,
    }
    impl SyncStore for ObservedRelay {
        fn durable_have(
            &self,
            conversation: &ConversationId,
            id: &EventId,
        ) -> Result<bool, LogError> {
            let held = self.inner.durable_have(conversation, id)?;
            if held && let Some(file) = self.final_chunks.lock().unwrap().get(id) {
                self.confirmed_files.lock().unwrap().insert(*file);
                self.confirmations.fetch_add(1, Ordering::SeqCst);
            }
            Ok(held)
        }
        fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
            self.inner.event_ids(conversation)
        }
        fn events_excluding(
            &self,
            conversation: &ConversationId,
            have: &std::collections::HashSet<EventId>,
        ) -> Result<Vec<crate::eventlog::Event>, LogError> {
            self.inner.events_excluding(conversation, have)
        }
        fn ingest(
            &mut self,
            event: crate::eventlog::Event,
        ) -> Result<crate::eventlog::AppendOutcome, crate::eventlog::LogError> {
            self.inner.ingest(event)
        }
    }
    let final_chunks = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let confirmed_files = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let confirmations = Arc::new(AtomicUsize::new(0));
    let mut office =
        crate::postoffice::PostOffice::open(&pd.path().join("relay"), "pw", relay_identity)
            .unwrap();
    office.require_opaque_envelopes();
    let relay = Arc::new(Mutex::new(ObservedRelay {
        inner: office,
        final_chunks: final_chunks.clone(),
        confirmed_files: confirmed_files.clone(),
        confirmations: confirmations.clone(),
    }));
    let delayed = Arc::new(AtomicBool::new(false));
    let exchanges = Arc::new(AtomicUsize::new(0));
    let sender_identity = a.identity.public();
    let server = tokio::spawn({
        let relay = relay.clone();
        let delayed = delayed.clone();
        let exchanges = exchanges.clone();
        async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
                let relay = relay.clone();
                let delayed = delayed.clone();
                let exchanges = exchanges.clone();
                let sender_identity = sender_identity.clone();
                connections.spawn(async move {
                    let Ok(mut channel) = super::transport::secure_accept(stream, &identity).await
                    else {
                        return;
                    };
                    let mut delayed_exchanges = 0;
                    while let Ok(bytes) = channel.recv().await {
                        if delayed.load(Ordering::SeqCst)
                            && channel.peer_identity() == &sender_identity
                            && super::session::requests_response(&bytes)
                            && delayed_exchanges < 5
                        {
                            delayed_exchanges += 1;
                            exchanges.fetch_add(1, Ordering::SeqCst);
                            // Five exchanges on EACH new sender connection inject 750ms,
                            // exceeding the unchanged 400ms whole-attempt budget.
                            // A reconnect resets this count, so retries cannot consume
                            // a global delay allowance and then pass without the delay.
                            // Do not also delay recipient drains or every later chunk
                            // round: those add load unrelated to retained progress.
                            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        }
                        match super::session::serve_wire_bytes(&mut channel, &relay, &bytes).await {
                            Ok(_) => {}
                            Err(error) => {
                                eprintln!("delayed relay error: {error}");
                                break;
                            }
                        }
                    }
                });
                while connections.try_join_next().is_some() {}
            }
        }
    });
    for peer in std::iter::once(&a).chain(recipients.iter().map(|(peer, _, _)| peer)) {
        peer.roster.lock().unwrap().update(
            &proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &peer.user_id(),
        );
    }
    let mut files = Vec::new();
    for index in 0..count {
        let (b, offers, bp) = &mut recipients[usize::from(index)];
        let source = ad.path().join(format!("relay-{index}.bin"));
        // Fair scheduling needs more than eight destinations, not large files.
        // The separate slow case keeps its two-chunk transfer coverage.
        let size = if slow {
            crate::file::CHUNK_SIZE + 31
        } else {
            31
        };
        let bytes = vec![index + 63; size];
        std::fs::write(&source, &bytes).unwrap();
        let (card, file) = a
            .enqueue_file_to_account(&b.account_id(), &source, crate::file::FileKind::File)
            .await
            .unwrap();
        let offer = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                a.flush_delivery(card).await;
                b.drain_from_post_office().await;
                if let Ok(offer) = offers.try_recv() {
                    break offer;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(offer.file_conv, file);
        b.decide_attachment(file, true).unwrap();
        b.drain_from_post_office().await;
        a.drain_from_post_office().await;
        let final_chunk = {
            let id = a
                .delivery
                .lock()
                .unwrap()
                .file_card(card)
                .unwrap()
                .final_chunk
                .unwrap();
            a.log.lock().unwrap().get(&id).unwrap().unwrap().clone()
        };
        for mailbox in [
            crate::postoffice::opaque::RelayMailbox::new(
                &a.identity,
                &bp.public(),
                &proof.public(),
            )
            .unwrap(),
            crate::postoffice::opaque::RelayMailbox::federated(&a.identity, &bp.public()).unwrap(),
        ] {
            final_chunks
                .lock()
                .unwrap()
                .insert(mailbox.wrap(&final_chunk).unwrap().id, file);
        }
        files.push((card, file, bytes, b.clone()));
    }
    // All offers/consent were reconciled above. Observe only file-worker
    // connections here, without unrelated proactive history-recovery pulls.
    a.replace_verified_dm_pull_peers(Vec::new());
    delayed.store(slow, Ordering::SeqCst);
    let worker = tokio::spawn(a.clone().run_delivery_loop());
    let mut recipient_drains = 0;
    let custody_started = std::time::Instant::now();
    let received = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        // First require every sender slot to reach qualified relay custody while
        // recipients stay offline. Recipient polling must not release sender
        // slots or hide a relay-custody scheduling failure.
        while !files
            .iter()
            .all(|(_, file, _, _)| confirmed_files.lock().unwrap().contains(file))
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        eprintln!(
            "retained relay qualified custody for {count} files after {:?}",
            custody_started.elapsed()
        );
        assert_eq!(
            recipient_drains, 0,
            "custody must not need recipient polling"
        );
        assert!(
            files.iter().all(|(_, file, _, b)| {
                !b.file_ready_to_save(*file).unwrap()
                    && b.log.lock().unwrap().events(file).unwrap().is_empty()
            }),
            "recipient must remain without chunks until all relay custody is confirmed"
        );
        loop {
            recipient_drains += 1;
            for (recipient, _, _) in &recipients {
                recipient.drain_from_post_office().await;
            }
            if files
                .iter()
                .all(|(_, file, _, recipient)| recipient.file_ready_to_save(*file).unwrap())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    let cooldown = if slow && received.is_ok() {
        let before = confirmations.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        Some((before > 0, before, confirmations.load(Ordering::SeqCst)))
    } else {
        None
    };
    worker.abort();
    server.abort();
    rejecting.abort();
    let _ = worker.await;
    let _ = server.await;
    let _ = rejecting.await;
    assert!(
        received.is_ok(),
        "retained relay worker stranded bytes: custody={}/{} ready={}/{} recipient-drains={} peak-slots={}",
        confirmed_files.lock().unwrap().len(),
        files.len(),
        files
            .iter()
            .filter(|(_, file, _, recipient)| recipient.file_ready_to_save(*file).unwrap())
            .count(),
        files.len(),
        recipient_drains,
        a.file_transfer_peak.load(Ordering::SeqCst)
    );
    if let Some((quiet, before, after)) = cooldown {
        assert!(quiet, "relay must confirm exact final-chunk custody");
        assert_eq!(
            after, before,
            "held relay custody must cool down across normal worker ticks"
        );
    }
    if slow {
        assert!(exchanges.load(Ordering::SeqCst) >= 5);
    }
    assert!(a.file_transfer_peak.load(Ordering::SeqCst) <= 8);
    for (card, file, bytes, b) in files {
        assert_eq!(b.read_file(file).unwrap(), bytes);
        assert!(
            !a.outgoing_file_progress(card).unwrap().delivered,
            "relay custody is not recipient completion"
        );
    }
}

#[tokio::test]
async fn retained_relay_file_exchange_rechecks_signed_consent_and_relay_role() {
    for revoke_consent in [true, false] {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let pd = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
        let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 1);
        let (a, _) = node(ad.path(), alice, aa, &bp);
        let (b, _, mut offers) = node_with_files(bd.path(), bob, bob_account, &ap);
        b.set_attachment_global_auto(false).unwrap();
        b.replace_verified_dm_pull_peers(vec![ap.user_id.clone()]);
        let identity = DeviceIdentity::generate();
        let keys = identity.secret_bytes();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proof =
            Announce::new_post_office(&identity, "Relay", listener.local_addr().unwrap().port());
        let relay = Arc::new(Mutex::new(
            crate::postoffice::PostOffice::open(&pd.path().join("relay"), "pw", identity).unwrap(),
        ));
        let server = tokio::spawn(super::postbox::run_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            listener,
            relay.clone(),
        ));
        for node in [&a, &b] {
            node.roster.lock().unwrap().update(
                &proof,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &node.user_id(),
            );
        }
        let source = ad.path().join("revocable.bin");
        std::fs::write(&source, [91; 64]).unwrap();
        let (card, file) = a
            .enqueue_file_to_account(&b.account_id(), &source, crate::file::FileKind::File)
            .await
            .unwrap();
        a.flush_delivery(card).await;
        b.drain_from_post_office().await;
        assert_eq!(offers.try_recv().unwrap().file_conv, file);
        b.decide_attachment(file, true).unwrap();
        for control in b.file_consent_events(&ap.public()).unwrap() {
            a.accept_file_consent_event(&control, &bp.public()).unwrap();
        }
        let card = a.delivery.lock().unwrap().file_card(card).unwrap().clone();
        let destination = card.destinations[0].clone();
        let mut transfers = std::collections::HashMap::new();
        // Failed direct dial, authenticated relay dial, then a complete mailbox
        // fingerprint exchange. The next retained step must recheck authority.
        for _ in 0..3 {
            assert!(!a.retry_file_step(&card, &destination, &mut transfers).await);
        }
        assert_eq!(transfers.len(), 1);
        let final_chunk = a
            .log
            .lock()
            .unwrap()
            .get(&card.final_chunk.unwrap())
            .unwrap()
            .unwrap()
            .clone();
        let outer = crate::postoffice::opaque::RelayMailbox::new(
            &a.identity,
            &bp.public(),
            &proof.public(),
        )
        .unwrap()
        .wrap(&final_chunk)
        .unwrap()
        .id;
        assert!(!relay.lock().unwrap().has(&outer));
        if revoke_consent {
            b.decide_attachment(file, false).unwrap();
            for control in b.file_consent_events(&ap.public()).unwrap() {
                a.accept_file_consent_event(&control, &bp.public()).unwrap();
            }
        } else {
            let ordinary = Announce::new(
                &DeviceIdentity::from_secret_bytes(keys.0, keys.1),
                "No longer a relay",
                proof.tcp_port,
            );
            a.roster.lock().unwrap().update(
                &ordinary,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &a.user_id(),
            );
        }
        assert!(!a.retry_file_step(&card, &destination, &mut transfers).await);
        assert!(
            transfers.is_empty(),
            "invalid retained relay socket must be discarded"
        );
        assert!(
            !relay.lock().unwrap().has(&outer),
            "authority ended before any chunk upload"
        );
        assert!(!a.outgoing_file_progress(card.id).unwrap().delivered);
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn outgoing_request_retries_are_durable_and_conflicts_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let peer =
        Announce::new_with_account(&DeviceIdentity::generate(), &Account::generate(), "peer", 9);
    let request = OutgoingMessageRequest {
        request_id: [42; 32],
        destination: MessageDestination::Account(peer.account_id().unwrap()),
        text: b"alert".to_vec(),
        reply_to: None,
        expires_at_ms: None,
    };
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(identity.secret_bytes().0, identity.secret_bytes().1),
        Account::from_secret_bytes(account.secret_bytes()),
        &peer,
    );
    let first = sender
        .enqueue_message_once_if(&request, |accept| accept())
        .await
        .unwrap();
    let retry = sender
        .enqueue_message_once_if(&request, |accept| accept())
        .await
        .unwrap();
    assert_eq!(first.message_id, retry.message_id);
    assert!(!first.duplicate);
    assert!(retry.duplicate);
    let mut conflict = request.clone();
    conflict.text = b"different".to_vec();
    assert!(matches!(
        sender
            .enqueue_message_once_if(&conflict, |accept| accept())
            .await,
        Err(NodeError::RequestConflict)
    ));
    drop(sender);
    let (sender, _) = node(dir.path(), identity, account, &peer);
    assert_eq!(
        sender
            .message_request_status(&request.request_id)
            .unwrap()
            .unwrap()
            .message_id,
        first.message_id
    );
    assert!(
        sender
            .enqueue_message_once_if(&request, |accept| accept())
            .await
            .unwrap()
            .duplicate
    );
}

#[tokio::test]
async fn outgoing_channel_request_is_guarded_and_shared_with_ordinary_sends() {
    let dir = tempfile::tempdir().unwrap();
    let peer =
        Announce::new_with_account(&DeviceIdentity::generate(), &Account::generate(), "peer", 9);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &peer,
    );
    let channel = sender.create_channel("alerts", vec![]).await.unwrap();
    let request = OutgoingMessageRequest {
        request_id: [43; 32],
        destination: MessageDestination::Channel(channel),
        text: b"alert".to_vec(),
        reply_to: None,
        expires_at_ms: None,
    };
    let before = sender
        .channels
        .lock()
        .unwrap()
        .state(&channel)
        .unwrap()
        .export_my_senders();
    assert!(
        sender
            .enqueue_message_once_if(&request, |_| Err(NodeError::Authorization(
                "revoked".into()
            )))
            .await
            .is_err()
    );
    assert_eq!(
        before,
        sender
            .channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .export_my_senders()
    );
    let first = sender
        .enqueue_message_once_if(&request, |accept| accept())
        .await
        .unwrap();
    sender
        .send_channel_message(channel, b"manual")
        .await
        .unwrap();
    assert_eq!(
        sender
            .enqueue_message_once_if(&request, |accept| accept())
            .await
            .unwrap()
            .message_id,
        first.message_id
    );
    assert_eq!(
        sender
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::eventlog::EventKind::Message)
            .count(),
        2
    );
}

#[tokio::test]
async fn outgoing_request_concurrent_retry_and_revocation_do_not_advance_ratchets() {
    let dir = tempfile::tempdir().unwrap();
    let peer =
        Announce::new_with_account(&DeviceIdentity::generate(), &Account::generate(), "peer", 9);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &peer,
    );
    let request = OutgoingMessageRequest {
        request_id: [44; 32],
        destination: MessageDestination::Account(peer.account_id().unwrap()),
        text: b"one alert".to_vec(),
        reply_to: None,
        expires_at_ms: None,
    };
    let (one, two) = tokio::join!(
        sender.enqueue_message_once_if(&request, |accept| accept()),
        sender.enqueue_message_once_if(&request, |accept| accept())
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(one.message_id, two.message_id);
    assert_ne!(one.duplicate, two.duplicate);
    let conversations = sender.log.lock().unwrap().conversations().unwrap();
    let before = conversations
        .iter()
        .map(|conv| sender.log.lock().unwrap().events(conv).unwrap().len())
        .sum::<usize>();
    let mut denied = request.clone();
    denied.request_id = [45; 32];
    let mut checks = 0;
    let result = sender
        .enqueue_message_once_if(&denied, |accept| {
            checks += 1;
            if checks >= 4 {
                return Err(NodeError::Authorization("revoked".into()));
            }
            accept()
        })
        .await;
    assert!(matches!(result, Err(NodeError::Authorization(_))));
    assert!(
        sender
            .message_request_status(&denied.request_id)
            .unwrap()
            .is_none()
    );
    let conversations = sender.log.lock().unwrap().conversations().unwrap();
    let after = conversations
        .iter()
        .map(|conv| sender.log.lock().unwrap().events(conv).unwrap().len())
        .sum::<usize>();
    assert_eq!(before, after);
}

#[tokio::test]
async fn outgoing_channel_request_recovers_after_durable_acceptance_and_sidecar_failure() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let peer =
        Announce::new_with_account(&DeviceIdentity::generate(), &Account::generate(), "peer", 9);
    let (sender, _) = node(dir.path(), identity, account, &peer);
    let channel = sender.create_channel("alerts", vec![]).await.unwrap();
    sender
        .sentlog
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    let request = OutgoingMessageRequest {
        request_id: [46; 32],
        destination: MessageDestination::Channel(channel),
        text: b"accepted before crash".to_vec(),
        reply_to: None,
        expires_at_ms: None,
    };
    let first = sender
        .enqueue_message_once_if(&request, |accept| accept())
        .await
        .unwrap();
    assert!(
        !sender
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    assert_eq!(
        sender
            .message_request_status(&request.request_id)
            .unwrap()
            .unwrap()
            .message_id,
        first.message_id
    );
    drop(sender);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &peer,
    );
    assert!(
        sender
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    assert_eq!(
        sender
            .enqueue_message_once_if(&request, |accept| accept())
            .await
            .unwrap()
            .message_id,
        first.message_id
    );
    sender
        .send_channel_message(channel, b"after restart")
        .await
        .unwrap();
    let log = sender.log.lock().unwrap();
    let messages: Vec<_> = log
        .events(&channel)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == crate::eventlog::EventKind::Message)
        .collect();
    assert_eq!(messages.len(), 2);
    assert_ne!(messages[0].ciphertext, messages[1].ciphertext);
}

#[tokio::test]
async fn outgoing_reply_lookup_is_bounded_to_recent_borrowed_records() {
    let dir = tempfile::tempdir().unwrap();
    let peer =
        Announce::new_with_account(&DeviceIdentity::generate(), &Account::generate(), "peer", 9);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &peer,
    );
    let target = peer.account_id().unwrap();
    let conversation = super::conversation::account_conversation_id(&sender.account_id(), &target);
    let mut oldest = EventId::new([0; 32]);
    let mut newest = oldest;
    for index in 0u32..501 {
        let mut id = [0; 32];
        id[..4].copy_from_slice(&index.to_be_bytes());
        if index == 0 {
            oldest = EventId::new(id);
        }
        newest = EventId::new(id);
        let envelope = super::dm_envelope::DmEnvelope::new(
            sender.account_id(),
            target.clone(),
            id,
            MessageBody::new(b"history".to_vec(), None).encode(),
        )
        .encode();
        sender
            .sentlog
            .lock()
            .unwrap()
            .record(conversation, u64::from(index) + 1, 100, &envelope)
            .unwrap();
    }
    let request = OutgoingMessageRequest {
        request_id: [55; 32],
        destination: MessageDestination::Account(target),
        text: b"reply".to_vec(),
        reply_to: Some(oldest),
        expires_at_ms: None,
    };
    assert!(
        matches!(sender.enqueue_message_once_if(&request, |accept| accept()).await, Err(NodeError::InvalidInput(message)) if message.contains("500 records"))
    );
    let recent = OutgoingMessageRequest {
        reply_to: Some(newest),
        ..request
    };
    assert!(
        sender
            .enqueue_message_once_if(&recent, |accept| accept())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn received_manifest_stays_saveable_after_reseed_and_restart() {
    use crate::eventlog::{Event, EventKind};
    for compact in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let account = Account::generate();
        let ap = Announce::new_with_account(&alice, &account, "Alice", 9);
        let bob = DeviceIdentity::generate();
        let secrets = bob.secret_bytes();
        let bob_account = Account::generate();
        let account_secret = bob_account.secret_bytes();
        let (b, _, mut callbacks) = node_with_files(dir.path(), bob, bob_account, &ap);
        let payload = b"received attachment bytes";
        let key = crate::file::FileKey::generate();
        let nonce = crate::file::generate_file_nonce();
        let chunks = vec![crate::file::seal_chunk_indexed(&key, &nonce, 0, payload).unwrap()];
        let v2 = crate::file::FileManifestV2 {
            name: String::new(),
            size: payload.len() as u64,
            mime: "video/quicktime".into(),
            checksum: crate::file::file_checksum(payload),
            file_key: *key.as_bytes(),
            file_nonce: nonce,
            file_conv: ConversationId::new([71; 32]),
            chunk_size: crate::file::CHUNK_SIZE as u32,
            chunk_count: 1,
            chunk_hashes: vec![crate::file::chunk_hash(payload)],
        };
        let manifest = crate::file::FileManifestV3 {
            v2,
            kind: crate::file::FileKind::File,
        };
        let mut manifest = manifest;
        manifest.v2.name = "IMG_9349.MOV".into();
        let file = manifest.v2.file_conv;
        let wire = if compact {
            manifest.v2.chunk_hashes.clear();
            crate::file::FileManifestV4 { v3: manifest }.encode()
        } else {
            manifest.encode()
        };
        let conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
        let event = Event::new(
            &alice,
            conv,
            1,
            vec![],
            1,
            100,
            EventKind::FileManifest,
            crate::dm::seal(&alice, &b.identity.public().x25519_pub, &wire).unwrap(),
        );
        b.log.lock().unwrap().append_durable(event).unwrap();
        b.process_file_events(conv);
        assert_eq!(callbacks.try_recv().unwrap().file_conv, file);
        for (index, ciphertext) in chunks.into_iter().enumerate() {
            let chunk = Event::new(
                &alice,
                file,
                index as u64 + 1,
                vec![],
                index as u64 + 1,
                100,
                EventKind::Message,
                ciphertext,
            );
            b.log.lock().unwrap().append_durable(chunk).unwrap();
        }
        assert_eq!(b.read_file(file).unwrap(), payload);
        b.reseed_live_file_book(&b.delivery.lock().unwrap())
            .unwrap();
        assert_eq!(b.read_file(file).unwrap(), payload);
        drop(b);
        let (b, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            Account::from_secret_bytes(account_secret),
            &ap,
        );
        assert_eq!(b.read_file(file).unwrap(), payload);
        b.save_file(file, &dir.path().join("saved.mov")).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("saved.mov")).unwrap(),
            payload
        );
    }
}

#[test]
fn indexed_file_runtime_warm_open_does_not_hydrate_retained_manifest_history() {
    use crate::eventlog::{Event, EventKind};
    for count in [8u8, 80] {
        let directory = tempfile::tempdir().unwrap();
        let sender = DeviceIdentity::generate();
        let sender_account = Account::generate();
        let announce = Announce::new_with_account(&sender, &sender_account, "Sender", 9);
        let identity = DeviceIdentity::generate();
        let secret = identity.secret_bytes();
        let account = Account::generate();
        let account_secret = account.secret_bytes();
        let (receiver, _, _files) = node_with_files(directory.path(), identity, account, &announce);
        let conversation =
            super::conversation::dm_conversation_id(&sender.public(), &receiver.identity.public());
        let mut parent = None;
        for n in 1..=count {
            let manifest = crate::file::FileManifest {
                name: format!("retained-{n}.txt"),
                size: 0,
                mime: "text/plain".into(),
                checksum: crate::file::file_checksum(&[]),
                file_key: [6; 32],
                file_conv: ConversationId::new([n; 32]),
                chunk_count: 1,
            };
            let event = Event::new(
                &sender,
                conversation,
                u64::from(n),
                parent.into_iter().collect(),
                u64::from(n),
                100,
                EventKind::FileManifest,
                crate::dm::seal(
                    &sender,
                    &receiver.identity.public().x25519_pub,
                    &manifest.encode(),
                )
                .unwrap(),
            );
            parent = Some(event.id);
            receiver.log.lock().unwrap().append_durable(event).unwrap();
        }
        receiver.process_file_events(conversation);
        let host = super::conversation::account_conversation_id(
            &receiver.account_id(),
            &sender_account.account_id(),
        );
        assert_eq!(
            receiver
                .received_files
                .lock()
                .unwrap()
                .entries_ref(&host)
                .len(),
            usize::from(count)
        );
        drop(receiver);
        let (reopened, _, _) = node_with_files(
            directory.path(),
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            Account::from_secret_bytes(account_secret),
            &announce,
        );
        assert_eq!(
            reopened.received_files.lock().unwrap().body_reads(),
            0,
            "warm startup must use file eligibility and pending-work metadata for {count} retained cards"
        );
        let before = reopened
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        reopened.process_file_events(conversation);
        assert_eq!(
            reopened
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "unchanged file receive turns must not load {count} processed signed manifests"
        );
    }
}

#[test]
fn expired_received_attachment_loses_readiness_after_cleanup() {
    use crate::eventlog::{Event, EventKind};
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let account = Account::generate();
    let ap = Announce::new_with_account(&alice, &account, "Alice", 9);
    let (b, _, mut callbacks) = node_with_files(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &ap,
    );
    let payload = b"attachment with five-minute deadline";
    let key = crate::file::FileKey::generate();
    let file = ConversationId::new([72; 32]);
    let manifest = crate::file::FileManifest {
        name: "IMG_9349.MOV".into(),
        size: payload.len() as u64,
        mime: "video/quicktime".into(),
        checksum: crate::file::file_checksum(payload),
        file_key: *key.as_bytes(),
        file_conv: file,
        chunk_count: 1,
    };
    let sent_at = super::node::now_millis();
    let wire = crate::expiry::wrap(&manifest.encode(), Some(sent_at + 300_000));
    let conv = super::conversation::dm_conversation_id(&alice.public(), &b.identity.public());
    let event = Event::new(
        &alice,
        conv,
        1,
        vec![],
        1,
        sent_at,
        EventKind::FileManifest,
        crate::dm::seal(&alice, &b.identity.public().x25519_pub, &wire).unwrap(),
    );
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.process_file_events(conv);
    assert_eq!(callbacks.try_recv().unwrap().file_conv, file);
    let chunk = Event::new(
        &alice,
        file,
        1,
        vec![],
        1,
        sent_at,
        EventKind::Message,
        crate::file::seal_chunk(&key, payload).unwrap(),
    );
    b.log.lock().unwrap().append_durable(chunk).unwrap();
    assert!(b.file_ready_to_save(file).unwrap());
    assert_eq!(b.read_file(file).unwrap(), payload);
    b.expiry_first_seen.lock().unwrap().insert(
        event.id,
        std::time::Instant::now() - std::time::Duration::from_secs(301),
    );
    assert!(!b.file_ready_to_save(file).unwrap());
    let output = dir.path().join("expired.mov");
    assert!(
        matches!(b.save_file(file, &output), Err(NodeError::File(reason)) if reason == "file expired")
    );
    assert_eq!(b.prune_expired_received().unwrap(), 1);
    assert!(!b.file_ready_to_save(file).unwrap());
    assert!(
        b.account_history(&account.account_id(), 10)
            .unwrap()
            .is_empty()
    );
    // Cleanup deliberately erases the plaintext manifest and its deadline. A stale UI
    // must refresh readiness rather than offering Save based on the previous result.
    assert!(
        matches!(b.save_file(file, &output), Err(NodeError::File(reason)) if reason == "unknown file")
    );
    assert!(!output.exists());
    assert!(!dir.path().join("expired.mov.part").exists());
}

#[tokio::test]
async fn delayed_routed_dm_retries_reach_recipient_and_confirm_without_manual_flush() {
    use std::time::Duration;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut received) = node(bd.path(), bob, bob_account, &ap);
    let id = a
        .enqueue_to_account(&b.account_id(), b"delayed routed message", None)
        .await
        .unwrap();
    let mut workers = tokio::task::JoinSet::new();
    workers.spawn(a.clone().run_delivery_loop());
    workers.spawn(b.clone().run_delivery_loop());
    // Delay both directions: the recipient's authenticated receipt must also
    // survive a fresh routed handshake, without a manual flush or pull bypass.
    for (receiver, listener) in [(a.clone(), al), (b.clone(), bl)] {
        workers.spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let receiver = receiver.clone();
                connections.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(450)).await;
                    if let Ok(channel) = receiver.privacy_accept(stream).await {
                        receiver.serve_connection(channel).await;
                    }
                });
            }
        });
    }
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        let message = received.recv().await.unwrap();
        assert_eq!(message.text, b"delayed routed message");
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    workers.abort_all();
    while workers.join_next().await.is_some() {}
    for node in [&a, &b] {
        node.runtime_work.close();
        tokio::time::timeout(Duration::from_secs(1), node.runtime_work.drain())
            .await
            .expect("cancelled controls must release retirement admission");
    }
    assert!(
        result.is_ok(),
        "a reachable high-latency recipient must receive the queued message and return its authenticated receipt"
    );
}

#[tokio::test]
async fn busy_delivery_slots_preserve_the_next_queued_destination_turn() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let account = Account::generate();
    let bob = DeviceIdentity::generate();
    let remote = Account::generate();
    let listeners = [
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
    ];
    let proof = Announce::new_with_account(
        &bob,
        &remote,
        "Bob",
        listeners[0].local_addr().unwrap().port(),
    );
    let (sender, _) = node(dir.path(), alice, account, &proof);
    for listener in &listeners[1..] {
        let sibling = DeviceIdentity::generate();
        let proof = Announce::new_with_account(
            &sibling,
            &sender.account,
            "Sibling",
            listener.local_addr().unwrap().port(),
        );
        sender.roster.lock().unwrap().update(
            &proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &sender.user_id(),
        );
    }
    let id = sender
        .enqueue_to_account(&remote.account_id(), b"keep every destination's turn", None)
        .await
        .unwrap();
    let mut destinations: Vec<_> = sender
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations
        .iter()
        .map(|d| d.event.id)
        .collect();
    destinations.sort();
    assert_eq!(destinations.len(), 3);
    let worker = tokio::spawn(sender.clone().run_delivery_loop());
    // Two identity-pinned dials remain pending while the third destination waits.
    // Hold the accepted streams without completing their Noise handshakes.
    let mut streams = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..2 {
            let (stream, _) = tokio::select! {
                result = listeners[0].accept() => result.unwrap(),
                result = listeners[1].accept() => result.unwrap(),
                result = listeners[2].accept() => result.unwrap(),
            };
            streams.push(stream);
        }
        // Force four additional snapshot passes before either dial times out.
        for _ in 0..4 {
            let (observed, snapshot) = tokio::sync::oneshot::channel();
            *sender.delivery_snapshot_hook.lock().unwrap() = Some(Box::new(move || {
                let _ = observed.send(());
            }));
            sender.delivery_notify.notify_one();
            snapshot.await.unwrap();
        }
    })
    .await
    .unwrap();
    worker.abort();
    let _ = worker.await;
    sender.runtime_work.close();
    tokio::time::timeout(Duration::from_secs(1), sender.runtime_work.drain())
        .await
        .unwrap();
    let next = sender
        .delivery
        .lock()
        .unwrap()
        .next_destination(None)
        .unwrap()
        .1
        .event_id;
    assert_eq!(
        next, destinations[2],
        "a full lane must not consume an unattempted destination's turn"
    );
}

#[tokio::test]
async fn relocated_recipient_cancels_stalled_old_route_before_exchange_deadline() {
    use std::time::Duration;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp =
        Announce::new_with_account(&bob, &bob_account, "Bob", old.local_addr().unwrap().port());
    let relocated =
        Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut incoming) = node(bd.path(), bob, bob_account, &ap);
    let id = a
        .enqueue_to_account(&b.account_id(), b"handoff queued", None)
        .await
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(a.clone().run_accept_loop(al));
    tasks.spawn(b.clone().run_accept_loop(bl));
    let (stalled, _) = tokio::time::timeout(Duration::from_secs(2), old.accept())
        .await
        .unwrap()
        .unwrap();
    a.roster
        .lock()
        .unwrap()
        .update(&relocated, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        assert_eq!(incoming.recv().await.unwrap().text, b"handoff queued");
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    drop(stalled);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    assert!(
        result.is_ok(),
        "fresh recipient route must preempt a stalled old exchange without reducing the routed exchange budget"
    );
}

#[tokio::test]
async fn approving_an_attachment_wakes_recovery_immediately() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(source.path(), alice, aa, &bp);
    let (b, _) = node(target.path(), bob, bob_account, &ap);
    let path = source.path().join("approved.bin");
    std::fs::write(&path, b"consented bytes").unwrap();
    let (id, file) = a
        .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let event = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    b.process_file_events(event.conversation_id);
    // Clear any initial metadata wake; the decision must cause its own wake.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(10),
        b.file_pull_notify.notified(),
    )
    .await;
    b.decide_attachment(file, true).unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            b.file_pull_notify.notified()
        )
        .await
        .is_ok(),
        "local acceptance must wake recovery without waiting for the timer"
    );
}

#[tokio::test]
async fn authenticated_silent_preferred_relay_does_not_block_upload_or_drain_fallback() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let relay_dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(source.path(), alice, aa, &bp);
    let (b, mut inbox) = node(target.path(), bob, bob_account, &ap);
    b.replace_verified_dm_pull_peers(vec![ap.user_id.clone()]);
    let silent = DeviceIdentity::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_proof =
        Announce::new_post_office(&silent, "Silent", listener.local_addr().unwrap().port());
    let silent_task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                    let keys = silent.secret_bytes();
                    connections.spawn(async move {
                        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
                        if let Ok(_channel) = super::transport::secure_accept(stream, &identity).await {
                            std::future::pending::<()>().await;
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let live = DeviceIdentity::generate();
    let keys = live.secret_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let live_proof =
        Announce::new_post_office(&live, "Fallback", listener.local_addr().unwrap().port());
    let relay = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(&relay_dir.path().join("relay"), "pw", live).unwrap(),
    ));
    let live_task = tokio::spawn(super::postbox::run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        listener,
        relay,
    ));
    for participant in [&a, &b] {
        for proof in [&silent_proof, &live_proof] {
            participant.roster.lock().unwrap().update(
                proof,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &participant.user_id(),
            );
        }
        participant
            .set_bridge_hub_id(Some(&silent_proof.user_id))
            .unwrap();
    }
    let id = a
        .enqueue_to_account(
            &b.account_id(),
            b"fallback survives a silent authenticated relay",
            None,
        )
        .await
        .unwrap();
    let destination = a.delivery.lock().unwrap().message(id).unwrap().destinations[0].clone();
    let uploaded = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        a.replicate_delivery_destination(&destination),
    )
    .await;
    assert!(
        matches!(uploaded, Ok(true)),
        "a silent preferred relay must yield to healthy custody"
    );
    let drained = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        b.drain_from_post_office(),
    )
    .await;
    silent_task.abort();
    live_task.abort();
    let _ = silent_task.await;
    let _ = live_task.await;
    assert!(
        drained.is_ok(),
        "one stalled relay must not pin all recipient drains"
    );
    let received = tokio::time::timeout(std::time::Duration::from_secs(1), inbox.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        received.text,
        b"fallback survives a silent authenticated relay"
    );
    assert_eq!(
        a.delivery_status(id),
        Some(DeliveryStatus::Awaiting),
        "relay custody is not recipient confirmation"
    );
}

#[tokio::test]
async fn healthy_relay_receive_cadence_is_independent_of_unrelated_silent_relay() {
    relay_receive_cadence_fixture(false, RelayReceiveReset::None).await;
}

#[tokio::test]
async fn healthy_relay_receive_cadence_preserves_slow_alternative_during_roster_churn() {
    relay_receive_cadence_fixture(true, RelayReceiveReset::None).await;
}

#[tokio::test]
async fn receive_worker_network_reset_cancels_active_exchange_and_recovers() {
    relay_receive_cadence_fixture(false, RelayReceiveReset::Network).await;
}

#[tokio::test]
async fn receive_worker_permission_change_cancels_active_exchange_and_recovers() {
    relay_receive_cadence_fixture(false, RelayReceiveReset::Permission).await;
}

#[tokio::test]
async fn receive_worker_master_change_cancels_active_exchange_and_recovers() {
    relay_receive_cadence_fixture(false, RelayReceiveReset::Master).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RelayReceiveReset {
    None,
    Network,
    Permission,
    Master,
}

async fn relay_receive_cadence_fixture(progressing_alternative: bool, reset: RelayReceiveReset) {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let relay_dir = tempfile::tempdir().unwrap();
    let alternative_dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _) = node(source.path(), alice, aa, &bp);
    let (b, mut inbox) = node(target.path(), bob, bob_account, &ap);
    b.replace_verified_dm_pull_peers(vec![ap.user_id.clone()]);
    let silent = DeviceIdentity::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_proof =
        Announce::new_post_office(&silent, "Silent", listener.local_addr().unwrap().port());
    let keys = silent.secret_bytes();
    let alternate_store = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(
            &alternative_dir.path().join("relay"),
            "pw",
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        )
        .unwrap(),
    ));
    let slow_recipient = b.identity.public();
    let (authenticated, mut silent_ready) = mpsc::unbounded_channel();
    let silent_task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let keys = silent.secret_bytes();
                    let authenticated = authenticated.clone();
                    let alternate_store = alternate_store.clone();
                    let slow_recipient = slow_recipient.clone();
                    connections.spawn(async move {
                        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
                        let channel = super::transport::secure_accept(stream, &identity).await.unwrap();
                        if channel.peer_identity() == &slow_recipient {
                            let _ = authenticated.send(());
                            if progressing_alternative {
                                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                            } else {
                                std::future::pending::<()>().await;
                            }
                        }
                        super::postbox::serve_relay_connection(channel, alternate_store).await;
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    // Rank synthetic churn offices ahead of the healthy office when its moved
    // endpoint is cold and the policy reset creates a fresh schedule. Held
    // listeners make their unresponsiveness deterministic instead of relying on
    // platform-specific refusal of ports 9/10.
    let mut identities = std::array::from_fn::<_, 3, _>(|_| DeviceIdentity::generate());
    identities.sort_by_key(DeviceIdentity::user_id);
    let [first_churn, second_churn, live] = identities;
    let mut churn_offices = Vec::new();
    let mut churn_listeners = Vec::new();
    if progressing_alternative {
        for identity in [first_churn, second_churn] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            churn_offices.push(Announce::new_post_office(
                &identity,
                "Synthetic unrelated office",
                listener.local_addr().unwrap().port(),
            ));
            churn_listeners.push(listener);
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let live_proof =
        Announce::new_post_office(&live, "Healthy", listener.local_addr().unwrap().port());
    let keys = live.secret_bytes();
    let relay = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(&relay_dir.path().join("relay"), "pw", live).unwrap(),
    ));
    let replacement_store = relay.clone();
    let (completed, mut rounds) = mpsc::unbounded_channel();
    let recipient = b.identity.public();
    let live_task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let relay = relay.clone();
                    let completed = completed.clone();
                    let recipient = recipient.clone();
                    connections.spawn(async move {
                        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
                        let mut channel = super::transport::secure_accept(stream, &identity).await.unwrap();
                        while let Ok(super::session::Served::Handled(_)) = super::session::serve_one(&mut channel, &relay).await {
                            if channel.peer_identity() == &recipient {
                                let _ = completed.send(());
                            }
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    for participant in [&a, &b] {
        for proof in [&silent_proof, &live_proof] {
            participant.roster.lock().unwrap().update(
                proof,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &participant.user_id(),
            );
        }
    }
    b.set_bridge_hub_id(Some(&silent_proof.user_id)).unwrap();
    let worker = tokio::spawn(b.clone().run_post_office_drains());
    tokio::time::timeout(std::time::Duration::from_secs(2), silent_ready.recv())
        .await
        .unwrap()
        .unwrap();
    let slow_started = tokio::time::Instant::now();
    tokio::time::timeout(std::time::Duration::from_secs(2), rounds.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    if reset != RelayReceiveReset::None {
        if reset == RelayReceiveReset::Network {
            // Only the network event can cancel this still-active socket;
            // deliberately avoid policy notification and generation changes.
            b.post_office_receive_enabled
                .store(false, std::sync::atomic::Ordering::Release);
            b.sessions.clear();
        }
        let capacity = || {
            (0..super::session_pool::MAX_DIRECTION_CONNECTIONS)
                .map(|_| b.sessions.try_outbound())
                .collect::<Option<Vec<_>>>()
        };
        assert!(
            capacity().is_none(),
            "an authenticated silent exchange must still own TCP capacity before reset"
        );
        match reset {
            RelayReceiveReset::Network => b
                .network_changes
                .send_modify(|revision| *revision = revision.wrapping_add(1)),
            RelayReceiveReset::Permission => b.set_post_office_permissions(true, false),
            RelayReceiveReset::Master => b.set_post_office_enabled(false),
            RelayReceiveReset::None => unreachable!(),
        }
        let released = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(permits) = capacity() {
                    break permits;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("receive reset must promptly drop the active exchange and all its permits");
        drop(released);
        while rounds.try_recv().is_ok() {}
        match reset {
            RelayReceiveReset::Network => {
                b.post_office_receive_enabled
                    .store(true, std::sync::atomic::Ordering::Release);
                b.network_changes
                    .send_modify(|revision| *revision = revision.wrapping_add(1));
            }
            RelayReceiveReset::Permission => b.set_post_office_permissions(true, true),
            RelayReceiveReset::Master => b.set_post_office_enabled(true),
            RelayReceiveReset::None => unreachable!(),
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), rounds.recv())
            .await
            .expect("receive reset must resume healthy office pulls")
            .unwrap();
    }
    if progressing_alternative {
        // Neither a timestamp heartbeat nor an unrelated new relay may restart
        // the valid four-second exchange already running on the alternative.
        b.roster
            .lock()
            .unwrap()
            .update(&live_proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
        let unrelated = &churn_offices[0];
        b.roster
            .lock()
            .unwrap()
            .update(unrelated, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    }
    let mut delayed_churn = if progressing_alternative {
        let recipient = b.clone();
        let unrelated = churn_offices[1].clone();
        Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            recipient.roster.lock().unwrap().update(
                &unrelated,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &recipient.user_id(),
            );
        }))
    } else {
        None
    };
    // The healthy mailbox was empty when the receive worker last pulled it.
    // Publish only now, while the unrelated authenticated relay stays silent.
    let id = a
        .enqueue_to_account(&b.account_id(), b"arrived after the empty pull", None)
        .await
        .unwrap();
    let destination = a.delivery.lock().unwrap().message(id).unwrap().destinations[0].clone();
    let mut upload = a
        .privacy_sync_dial(
            std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), live_proof.tcp_port),
            &live_proof.public(),
        )
        .await
        .unwrap();
    let uploaded = a
        .opaque_relay_round(
            &mut upload,
            &b.identity.public(),
            Some(destination.event.conversation_id),
        )
        .await
        .unwrap();
    assert!(
        uploaded
            .iter()
            .any(|(inner, _)| *inner == destination.event.id),
        "the synthetic DM must actually reach relay custody before measuring receive latency"
    );
    drop(upload);
    let slow_id = if progressing_alternative {
        let id = a
            .enqueue_to_account(&b.account_id(), b"held only by the slow alternative", None)
            .await
            .unwrap();
        let destination = a.delivery.lock().unwrap().message(id).unwrap().destinations[0].clone();
        let mut channel = a
            .privacy_sync_dial(
                std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), silent_proof.tcp_port),
                &silent_proof.public(),
            )
            .await
            .unwrap();
        let uploaded = a
            .opaque_relay_round(
                &mut channel,
                &b.identity.public(),
                Some(destination.event.conversation_id),
            )
            .await
            .unwrap();
        assert!(
            uploaded
                .iter()
                .any(|(inner, _)| *inner == destination.event.id)
        );
        Some(id)
    } else {
        None
    };
    let received = tokio::time::timeout(std::time::Duration::from_secs(4), inbox.recv()).await;
    let slow_received = if slow_id.is_some() {
        Some(
            tokio::time::timeout_at(
                slow_started + std::time::Duration::from_secs(5),
                inbox.recv(),
            )
            .await,
        )
    } else {
        None
    };

    let replacement_task = if progressing_alternative {
        // The cadence/churn phase is complete. Join its producer before retiring
        // only its synthetic offices so it cannot reinsert one during the
        // independent endpoint/policy-reset phase. A fresh schedule has no learned
        // primary for the moved endpoint; unrelated blackholes must not turn this
        // assertion into an OS-specific connection-failure timing test.
        if let Some(task) = delayed_churn.take() {
            task.await.unwrap();
        }
        {
            let mut roster = b.roster.lock().unwrap();
            for office in &churn_offices {
                roster.forget(&office.public());
            }
        }
        drop(churn_listeners);
        // Move this same authenticated office to a new endpoint while receive
        // permission is suspended, then prove the owned worker resumes there.
        b.set_post_office_permissions(true, false);
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let replacement = Announce::new_post_office(
            &identity,
            "Moved healthy office",
            listener.local_addr().unwrap().port(),
        );
        let task = tokio::spawn(super::postbox::run_relay_accept_loop(
            identity,
            listener,
            replacement_store,
        ));
        for participant in [&a, &b] {
            participant.roster.lock().unwrap().update(
                &replacement,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &participant.user_id(),
            );
        }
        let id = a
            .enqueue_to_account(&b.account_id(), b"after endpoint and policy change", None)
            .await
            .unwrap();
        let destination = a.delivery.lock().unwrap().message(id).unwrap().destinations[0].clone();
        let mut channel = a
            .privacy_sync_dial(
                std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), replacement.tcp_port),
                &replacement.public(),
            )
            .await
            .unwrap();
        let uploaded = a
            .opaque_relay_round(
                &mut channel,
                &b.identity.public(),
                Some(destination.event.conversation_id),
            )
            .await
            .unwrap();
        assert!(
            uploaded
                .iter()
                .any(|(inner, _)| *inner == destination.event.id)
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(4), inbox.recv())
                .await
                .is_err(),
            "suspended receive permission must prevent polling the newly populated office"
        );
        b.set_post_office_permissions(true, true);
        let received = tokio::time::timeout(std::time::Duration::from_secs(4), inbox.recv())
            .await
            .expect("resuming receive permission must use the office's new endpoint")
            .unwrap();
        assert_eq!(received.message_id, Some(id));
        assert_eq!(received.text, b"after endpoint and policy change");
        Some(task)
    } else {
        None
    };
    worker.abort();
    if let Some(task) = delayed_churn {
        task.abort();
        let _ = task.await;
    }
    silent_task.abort();
    live_task.abort();
    let _ = worker.await;
    let _ = silent_task.await;
    let _ = live_task.await;
    if let Some(task) = replacement_task {
        task.abort();
        let _ = task.await;
    }
    assert!(
        received.is_ok(),
        "a healthy empty pull must be repeated on its own three-second cadence while another relay stalls"
    );
    let received = received.unwrap().unwrap();
    assert_eq!(received.message_id, Some(id));
    assert_eq!(received.text, b"arrived after the empty pull");
    if let Some(id) = slow_id {
        let received = slow_received.unwrap().expect("a valid slow alternative must finish despite healthy polls and unrelated roster churn").unwrap();
        assert_eq!(received.message_id, Some(id));
        assert_eq!(received.text, b"held only by the slow alternative");
        assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    }
    let capacity: Vec<_> = (0..super::session_pool::MAX_DIRECTION_CONNECTIONS)
        .map(|_| {
            b.sessions
                .try_outbound()
                .expect("retiring the receive worker releases every active socket")
        })
        .collect();
    drop(capacity);
    assert_eq!(
        a.delivery_status(id),
        Some(DeliveryStatus::Awaiting),
        "upload custody alone cannot confirm recipient delivery"
    );
}

#[tokio::test]
async fn failed_media_persistence_remains_pending_until_storage_recovers() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let (_, b, _, _, _, file, _, bytes) =
        completed_file_fixture(source.path(), target.path(), crate::file::FileKind::Media).await;
    b.decide_attachment(file, true).unwrap();
    let destination = b.media.path_for(file, "completion.bin");
    std::fs::create_dir(&destination).unwrap();
    b.pull_pending_files().await;
    assert!(
        b.files.lock().unwrap().pending_for_test(file),
        "completed chunks do not erase a failed durable media write from recovery"
    );
    assert!(!b.media.contains(file));
    std::fs::remove_dir(&destination).unwrap();
    b.pull_pending_files().await;
    assert!(b.media.contains(file));
    assert!(!b.files.lock().unwrap().pending_for_test(file));
    assert_eq!(b.read_media(file).unwrap(), bytes);
}

#[tokio::test]
async fn failed_media_completion_metadata_keeps_durable_bytes_pending() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let (_, b, _, _, _, file, _, bytes) =
        completed_file_fixture(source.path(), target.path(), crate::file::FileKind::Media).await;
    b.decide_attachment(file, true).unwrap();
    let outbox = target.path().join("events.log.delivery-outbox");
    let held = target.path().join("held-outbox");
    std::fs::rename(&outbox, &held).unwrap();
    b.pull_pending_files().await;
    assert!(b.media.contains(file));
    assert!(!b.has_verified_file_completion(file));
    assert!(
        b.files.lock().unwrap().pending_for_test(file),
        "bytes alone cannot retire pending durable completion metadata"
    );
    std::fs::rename(&held, &outbox).unwrap();
    b.pull_pending_files().await;
    assert!(b.has_verified_file_completion(file));
    assert!(!b.files.lock().unwrap().pending_for_test(file));
    assert_eq!(b.read_media(file).unwrap(), bytes);
}

#[tokio::test(start_paused = true)]
async fn adaptive_exchange_uses_latency_learned_after_the_attempt_started() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let remote = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&remote, &Account::generate(), "slow peer", 47479);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let address = sender
        .roster
        .lock()
        .unwrap()
        .get(&remote.user_id())
        .unwrap()
        .addr;
    let started = tokio::time::Instant::now();
    let result = sender
        .adaptive_control_timeout(&remote.public(), async {
            tokio::time::sleep(Duration::from_secs(8)).await;
            sender.sessions.authentication_succeeded(
                &remote.public(),
                address,
                Duration::from_secs(8),
            );
            tokio::time::sleep(Duration::from_secs(12)).await;
            true
        })
        .await;
    assert_eq!(
        result,
        Some(true),
        "useful slow progress must survive the former fifteen-second deadline"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(20));
}

#[tokio::test(start_paused = true)]
async fn adaptive_exchange_times_out_without_a_sample_and_learns_the_next_control_budget() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let remote = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&remote, &Account::generate(), "stalled peer", 47479);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let address = sender
        .roster
        .lock()
        .unwrap()
        .get(&remote.user_id())
        .unwrap()
        .addr;
    let started = tokio::time::Instant::now();
    let result = sender
        .adaptive_control_timeout(&remote.public(), std::future::pending::<()>())
        .await;
    assert_eq!(result, None);
    assert_eq!(started.elapsed(), Duration::from_secs(15));
    assert_eq!(
        sender.sessions.control_timeout(&remote.public(), address),
        Duration::from_secs(30)
    );
}

#[tokio::test(start_paused = true)]
async fn adaptive_exchange_has_one_non_sliding_hard_ceiling() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let remote = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&remote, &Account::generate(), "slow peer", 47479);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let address = sender
        .roster
        .lock()
        .unwrap()
        .get(&remote.user_id())
        .unwrap()
        .addr;
    sender
        .sessions
        .authentication_succeeded(&remote.public(), address, Duration::from_secs(8));
    let started = tokio::time::Instant::now();
    let result = sender
        .adaptive_control_timeout(&remote.public(), async {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                sender.sessions.control_succeeded(
                    &remote.public(),
                    address,
                    Duration::from_secs(20),
                );
            }
        })
        .await;
    assert_eq!(result, None::<()>);
    assert_eq!(started.elapsed(), Duration::from_secs(60));
}

#[tokio::test]
async fn automatic_delivery_uses_slow_reachable_route_behind_stalled_former_lan() {
    delayed_route_delivery(
        std::time::Duration::from_millis(3200),
        std::time::Duration::ZERO,
    )
    .await;
}

#[tokio::test]
async fn automatic_delivery_with_one_second_rtt_does_not_take_minutes() {
    delayed_route_delivery(
        std::time::Duration::ZERO,
        std::time::Duration::from_millis(500),
    )
    .await;
}

#[tokio::test]
async fn adaptive_delivery_at_two_second_rtt_preserves_progress_and_collects_the_exact_receipt() {
    delayed_route_delivery_with_budget(
        std::time::Duration::ZERO,
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(45),
        true,
    )
    .await;
}

async fn delayed_route_delivery(
    initial_delay: std::time::Duration,
    frame_delay: std::time::Duration,
) {
    delayed_route_delivery_with_budget(
        initial_delay,
        frame_delay,
        std::time::Duration::from_secs(25),
        false,
    )
    .await;
}

async fn delayed_route_delivery_with_budget(
    initial_delay: std::time::Duration,
    frame_delay: std::time::Duration,
    total_budget: std::time::Duration,
    recipient_inbound_only: bool,
) {
    use std::time::{Duration, Instant};
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old = TcpListener::bind("[::1]:0").await.unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "Bob",
        proxy.local_addr().unwrap().port(),
    );
    let old_bp =
        Announce::new_with_account(&bob, &bob_account, "Bob", old.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut incoming) = node(bd.path(), bob, bob_account, &ap);
    a.configure_privacy(
        ad.path(),
        "pw",
        &ap,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    a.roster
        .lock()
        .unwrap()
        .update(&old_bp, old.local_addr().unwrap().ip(), &a.user_id());
    a.initiate_contact_locally(&b.account_id()).await.unwrap();
    let backend = bl.local_addr().unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(a.clone().run_accept_loop(al));
    if recipient_inbound_only {
        let recipient = b.clone();
        tasks.spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    result = bl.accept() => {
                        let (stream, _) = result.unwrap();
                        let recipient = recipient.clone();
                        connections.spawn(async move {
                            if let Ok(channel) = recipient.privacy_accept(stream).await {
                                recipient.serve_connection(channel).await;
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
    } else {
        tasks.spawn(b.clone().run_accept_loop(bl));
    }
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accepted_by_proxy = accepted.clone();
    tasks.spawn(async move {
        let mut streams = Vec::new();
        loop {
            let (stream, _) = old.accept().await.unwrap();
            streams.push(stream); // Simulate a proxy accepting the departed LAN route.
        }
    });
    tasks.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result = proxy.accept() => {
                    let (client, _) = result.unwrap();
                    accepted_by_proxy.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    connections.spawn(async move {
                        // Routed authentication can legitimately exceed three seconds.
                        tokio::time::sleep(initial_delay).await;
                        let server = tokio::net::TcpStream::connect(backend).await.unwrap();
                        let (client_read, client_write) = client.into_split();
                        let (server_read, server_write) = server.into_split();
                        let forward = |mut input: tokio::net::tcp::OwnedReadHalf, mut output: tokio::net::tcp::OwnedWriteHalf| async move {
                            use tokio::io::{AsyncReadExt, AsyncWriteExt};
                            let mut buffer = vec![0u8; 65536];
                            loop {
                                let count = input.read(&mut buffer).await?;
                                if count == 0 { return output.shutdown().await; }
                                tokio::time::sleep(frame_delay).await;
                                output.write_all(&buffer[..count]).await?;
                            }
                        };
                        let _ = tokio::try_join!(forward(client_read, server_write), forward(server_read, client_write));
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let start = Instant::now();
    let id = tokio::time::timeout(
        Duration::from_millis(500),
        a.enqueue_to_account(&b.account_id(), b"hotspot handoff", None),
    )
    .await
    .expect("local enqueue remains independent of network latency")
    .unwrap();
    let result = tokio::time::timeout(total_budget, async {
        let message = incoming.recv().await.unwrap();
        assert_eq!(message.text, b"hotspot handoff");
        assert_eq!(
            message.message_id,
            Some(id),
            "live DM identity must match the logical history identity"
        );
        assert_eq!(
            message.from_account.as_deref(),
            Some(a.account_id().as_str())
        );
        let received = start.elapsed();
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (received, start.elapsed())
    })
    .await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let (received, confirmed) = result.expect("automatic delivery and authenticated receipt must progress over the slow valid route without waiting for the stale route");
    if recipient_inbound_only {
        assert!(
            confirmed > Duration::from_secs(15),
            "fixture must exercise an exchange beyond the former fixed deadline"
        );
        assert!(
            accepted.load(std::sync::atomic::Ordering::SeqCst) <= 2,
            "slow but progressing exchanges must not restart their authenticated connections"
        );
    }
    eprintln!("handoff received={received:?} confirmed={confirmed:?}");
}

#[tokio::test]
async fn automatic_send_collects_receipt_when_recipient_can_only_serve_inbound_connections() {
    use std::time::Duration;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut incoming) = node(bd.path(), bob, bob_account, &ap);
    let recipient = b.account_id();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(a.clone().run_accept_loop(al));
    tasks.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result = bl.accept() => {
                    let (stream, _) = result.unwrap();
                    let node = b.clone();
                    connections.spawn(async move {
                        if let Ok(channel) = node.privacy_accept(stream).await {
                            node.serve_connection(channel).await;
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let id = a
        .enqueue_to_account(&recipient, b"receipt without reverse dial", None)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        assert_eq!(
            incoming.recv().await.unwrap().text,
            b"receipt without reverse dial"
        );
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    assert!(
        result.is_ok(),
        "an accepted outgoing message must collect its authenticated receipt over the reachable path without requiring a reverse dial or unrelated background history sweep"
    );
}

#[tokio::test]
async fn delivery_observation_tracks_exact_worker_retry_and_cancellation() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_account = Account::generate();
    let proof = Announce::new_with_account(
        &DeviceIdentity::generate(),
        &peer_account,
        "Recipient",
        listener.local_addr().unwrap().port(),
    );
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let account = peer_account.account_id();
    let id = sender
        .enqueue_to_account(&account, b"observe scheduling", None)
        .await
        .unwrap();
    let worker = tokio::spawn(sender.clone().run_delivery_loop());
    let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let snapshot = sender.account_delivery_observations(&account, &[id]);
    assert_eq!(
        snapshot[0].2.unwrap().kind,
        DeliveryObservationKind::Attempting
    );
    assert!(
        sender
            .account_delivery_observations(&sender.account_id(), &[id])
            .is_empty()
    );
    assert!(
        sender
            .account_delivery_observations(&"11".repeat(16), &[id])
            .is_empty()
    );
    assert!(
        sender
            .account_delivery_observations(&account, &[EventId::new([7; 32])])
            .is_empty()
    );
    drop(stream);
    drop(listener);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if sender.account_delivery_observations(&account, &[id])[0]
                .2
                .is_some_and(|hint| hint.kind == DeliveryObservationKind::RetryScheduled)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(sender.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(
        sender.account_delivery_observations(&account, &[id])[0]
            .2
            .unwrap()
            .reason,
        None,
        "a refused connection must not be relabeled as a timeout"
    );
    worker.abort();
    let _ = worker.await;
    assert!(
        sender.account_delivery_observations(&account, &[id])[0]
            .2
            .is_none()
    );
}

#[tokio::test]
async fn delivery_observation_confirmation_and_generation_override_late_completion() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let proof = Announce::new_with_account(&DeviceIdentity::generate(), &account, "Recipient", 9);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let id = sender
        .enqueue_to_account(&account.account_id(), b"confirmed", None)
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .clone();
    let token = sender.begin_delivery_observation(id, &destination).unwrap();
    sender.privacy.generation.fetch_add(1, Ordering::SeqCst);
    sender.finish_delivery_observation(token);
    assert!(
        sender.account_delivery_observations(&account.account_id(), &[id])[0]
            .2
            .is_none()
    );
    let token = sender.begin_delivery_observation(id, &destination).unwrap();
    // Receipt signature/binding validation has its own production protocol tests.
    // This reproduces its durable commit before a worker's late completion.
    sender
        .delivery
        .lock()
        .unwrap()
        .mark_delivered_for(id, destination.event.id)
        .unwrap();
    sender.finish_delivery_observation(token);
    let snapshot = sender.account_delivery_observations(&account.account_id(), &[id]);
    assert_eq!(snapshot[0].1, DeliveryStatus::Delivered);
    assert!(snapshot[0].2.is_none());
    assert_eq!(
        sender.account_delivery_statuses(&account.account_id(), &[id]),
        vec![(id, DeliveryStatus::Delivered)]
    );
}

#[tokio::test]
async fn delivery_observation_projection_excludes_incoming_siblings_erased_and_retired_work() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (sender, _) = node(ad.path(), alice, aa, &bp);
    let (recipient, _) = node(bd.path(), bob, bob_account, &ap);
    let sibling = Announce::new_with_account(
        &DeviceIdentity::generate(),
        &sender.account,
        "Own sibling",
        9,
    );
    sender.roster.lock().unwrap().update(
        &sibling,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &sender.user_id(),
    );
    let account = recipient.account_id();
    let id = sender
        .enqueue_to_account(&account, b"outgoing", None)
        .await
        .unwrap();
    let destinations = sender
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations
        .clone();
    let external = destinations
        .iter()
        .find(|d| d.account.as_deref() == Some(account.as_str()))
        .unwrap();
    let own = destinations
        .iter()
        .find(|d| d.account.as_deref() == Some(sender.account_id().as_str()))
        .unwrap();
    let token = sender.begin_delivery_observation(id, external).unwrap();
    assert!(sender.begin_delivery_observation(id, own).is_none());
    assert_eq!(
        sender.account_delivery_observations(&account, &[id])[0]
            .2
            .unwrap()
            .kind,
        DeliveryObservationKind::Attempting
    );
    assert_eq!(
        sender
            .account_delivery_observations(&account, &vec![id; 257])
            .len(),
        256
    );
    let incoming = recipient
        .enqueue_to_account(&sender.account_id(), b"incoming", None)
        .await
        .unwrap();
    let event = recipient
        .delivery
        .lock()
        .unwrap()
        .message(incoming)
        .unwrap()
        .destinations[0]
        .event
        .clone();
    sender
        .log
        .lock()
        .unwrap()
        .append_durable(event.clone())
        .unwrap();
    sender.emit_new_messages(event.conversation_id);
    assert!(
        sender
            .account_delivery_observations(&account, &[incoming])
            .is_empty()
    );
    sender.delete_account_message(&account, id).unwrap();
    sender.finish_delivery_observation(token);
    assert!(
        sender
            .account_delivery_observations(&account, &[id])
            .is_empty()
    );
    let expired = sender
        .enqueue_to_account(&account, b"expire", None)
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .message(expired)
        .unwrap()
        .destinations
        .iter()
        .find(|d| d.account.as_deref() == Some(account.as_str()))
        .unwrap()
        .clone();
    let token = sender
        .begin_delivery_observation(expired, &destination)
        .unwrap();
    sender.prune_older_than(u64::MAX).unwrap();
    sender.finish_delivery_observation(token);
    assert!(
        sender
            .account_delivery_observations(&account, &[expired])
            .is_empty()
    );
    let retained = sender
        .enqueue_to_account(&account, b"retire", None)
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .message(retained)
        .unwrap()
        .destinations
        .iter()
        .find(|d| d.account.as_deref() == Some(account.as_str()))
        .unwrap()
        .clone();
    sender
        .begin_delivery_observation(retained, &destination)
        .unwrap();
    sender.runtime_work.close();
    assert!(
        sender.account_delivery_observations(&account, &[retained])[0]
            .2
            .is_none()
    );
    assert!(
        sender
            .begin_delivery_observation(retained, &destination)
            .is_none()
    );
}

#[tokio::test]
async fn delivery_observation_authenticated_receipt_removes_pending_hint() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (sender, _) = node(ad.path(), alice, aa, &bp);
    let (recipient, _) = node(bd.path(), bob, bob_account, &ap);
    let account = recipient.account_id();
    let id = sender
        .enqueue_to_account(&account, b"receipt overrides scheduling", None)
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .clone();
    let token = sender.begin_delivery_observation(id, &destination).unwrap();
    recipient
        .log
        .lock()
        .unwrap()
        .append_durable(destination.event.clone())
        .unwrap();
    recipient.emit_new_messages(destination.event.conversation_id);
    let receipt = recipient.delivery.lock().unwrap().retry_receipts(1)[0]
        .destination
        .event
        .clone();
    sender.process_delivery_control(&receipt, &bp, &mut sender.delivery.lock().unwrap());
    sender.finish_delivery_observation(token);
    let snapshot = sender.account_delivery_observations(&account, &[id]);
    assert_eq!(snapshot[0].1, DeliveryStatus::Delivered);
    assert!(snapshot[0].2.is_none());
}

#[tokio::test]
async fn delivery_observation_cancelled_before_first_poll_releases_exact_hint() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let proof = Announce::new_with_account(&DeviceIdentity::generate(), &account, "Recipient", 9);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let id = sender
        .enqueue_to_account(&account.account_id(), b"queued worker", None)
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .clone();
    let attempt = super::delivery_runtime::ObservationAttempt {
        node: sender.clone(),
        token: sender.begin_delivery_observation(id, &destination),
    };
    assert!(
        sender.account_delivery_observations(&account.account_id(), &[id])[0]
            .2
            .is_some()
    );
    let worker = tokio::spawn(sender.runtime_work.admit().unwrap().track(async move {
        let mut attempt = attempt;
        std::future::pending::<()>().await;
        let _ = attempt.token.take();
    }));
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert!(
        sender.account_delivery_observations(&account.account_id(), &[id])[0]
            .2
            .is_none()
    );
}

#[tokio::test]
async fn delivery_observation_worker_reports_local_route_policy_and_clears_stale_reasons() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let remote = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_with_account(&remote, &account, "Recipient", 9);
    let (sender, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let id = sender
        .enqueue_to_account(&account.account_id(), b"recover locally", None)
        .await
        .unwrap();
    sender.roster.lock().unwrap().forget(&remote.public());
    for (enabled, send, expected) in [
        (true, true, DeliveryRecoveryReason::NoKnownRoute),
        (false, true, DeliveryRecoveryReason::PostOfficeDisabled),
        (true, false, DeliveryRecoveryReason::PostOfficeDisabled),
    ] {
        sender.set_post_office_enabled(enabled);
        sender.set_post_office_permissions(send, true);
        let worker = tokio::spawn(sender.clone().run_delivery_loop());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if sender.account_delivery_observations(&account.account_id(), &[id])[0]
                    .2
                    .is_some_and(|o| o.reason == Some(expected))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("worker must publish its locally evidenced recovery reason");
        // A direction-policy generation change must invalidate even a retained reason.
        sender.set_post_office_permissions(false, false);
        assert!(
            sender.account_delivery_observations(&account.account_id(), &[id])[0]
                .2
                .is_none()
        );
        worker.abort();
        let _ = worker.await;
        sender.set_post_office_permissions(true, true);
        assert!(
            sender.account_delivery_observations(&account.account_id(), &[id])[0]
                .2
                .is_none()
        );
    }
    assert_eq!(sender.delivery_status(id), Some(DeliveryStatus::Awaiting));
}

#[tokio::test]
async fn delivery_observation_worker_reports_only_an_observed_exchange_timeout() {
    use std::time::Duration;
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let alice_account = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_announce = Announce::new_with_account(&alice, &alice_account, "Alice", 9);
    let bob_announce = Announce::new_with_account(
        &bob,
        &bob_account,
        "Bob",
        listener.local_addr().unwrap().port(),
    );
    let (sender, _) = node(source.path(), alice, alice_account, &bob_announce);
    let (receiver, _) = node(target.path(), bob, bob_account, &alice_announce);
    sender.set_post_office_enabled(false);
    let id = sender
        .enqueue_to_account(
            &receiver.account_id(),
            b"wait for authenticated exchange",
            None,
        )
        .await
        .unwrap();
    let (authenticated, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = receiver.privacy_accept(stream).await.unwrap();
        // Wait for the sender's request so its authenticated exchange is active
        // before advancing the clock. Keep the socket open without replying.
        let _request = channel.recv().await.unwrap();
        authenticated.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let worker = tokio::spawn(sender.clone().run_delivery_loop());
    tokio::time::timeout(Duration::from_secs(3), ready)
        .await
        .unwrap()
        .unwrap();
    let snapshot = sender.account_delivery_observations(&bob_announce.account_id().unwrap(), &[id]);
    assert_eq!(
        snapshot[0].2.unwrap().reason,
        None,
        "disabled relaying is not blamed when a direct route exists"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(70)).await;
    // The loop can still be awaiting its spawn_blocking store snapshot. A fixed
    // number of yields does not give that thread a bounded chance to complete.
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if sender.account_delivery_observations(&bob_announce.account_id().unwrap(), &[id])[0]
                .2
                .is_some_and(|o| o.reason == Some(DeliveryRecoveryReason::AttemptTimedOut))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("worker must publish the observed exchange timeout");
    assert_eq!(
        sender.account_delivery_observations(&bob_announce.account_id().unwrap(), &[id])[0]
            .2
            .unwrap()
            .reason,
        Some(DeliveryRecoveryReason::AttemptTimedOut)
    );
    assert_eq!(sender.delivery_status(id), Some(DeliveryStatus::Awaiting));
    worker.abort();
    server.abort();
    let _ = worker.await;
    let _ = server.await;
}

async fn external_recipient_delivery_after_sibling_archive(cached_prior_binding: bool) {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let cd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let sibling = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let account = Account::generate();
    let shared_secret = account.secret_bytes();
    let bob_account = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let recipient_address = bl.local_addr().unwrap();
    let cl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &account, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", recipient_address.port());
    let cp = Announce::new_with_account(
        &sibling,
        &account,
        "Alice sibling",
        cl.local_addr().unwrap().port(),
    );
    let prior_account = Account::generate();
    let prior_sender =
        Announce::new_with_account(&alice, &prior_account, "Before adoption", ap.tcp_port);
    let (a, _) = node(ad.path(), alice, account, &bp);
    a.roster
        .lock()
        .unwrap()
        .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let (b, mut recipient_messages) = node(
        bd.path(),
        bob,
        bob_account,
        if cached_prior_binding {
            &prior_sender
        } else {
            &ap
        },
    );
    let (c, mut sibling_messages) = node(
        cd.path(),
        sibling,
        Account::from_secret_bytes(shared_secret),
        &ap,
    );
    if cached_prior_binding {
        b.roster
            .lock()
            .unwrap()
            .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    }
    for (node, directory, own) in [
        (&a, ad.path(), &ap),
        (&b, bd.path(), &bp),
        (&c, cd.path(), &cp),
    ] {
        node.configure_privacy(
            directory,
            "pw",
            own,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    if cached_prior_binding {
        b.cached_peer_snapshot_async().await.unwrap();
        // Discovery now carries a valid certificate from the kept account, but
        // this recipient has previously pinned the same sender device elsewhere.
        b.roster
            .lock()
            .unwrap()
            .update(&ap, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    }
    drop(bl); // The recipient is genuinely unreachable while the sibling mirrors.
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let ct = tokio::spawn(c.clone().run_accept_loop(cl));
    let id = a
        .enqueue_to_account(&b.account_id(), b"external recipient must receive", None)
        .await
        .unwrap();
    let sibling_copy =
        tokio::time::timeout(std::time::Duration::from_secs(10), sibling_messages.recv())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        sibling_copy.text.as_slice(),
        b"external recipient must receive"
    );
    assert!(recipient_messages.try_recv().is_err());
    assert_ne!(
        a.delivery_status(id),
        Some(DeliveryStatus::Delivered),
        "a sibling's archive is never recipient delivery"
    );
    let bl = TcpListener::bind(recipient_address).await.unwrap();
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    if cached_prior_binding {
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                recipient_messages.recv()
            )
            .await
            .is_err(),
            "a changed account certificate must not silently replace a pinned device binding"
        );
        assert_ne!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
        // Explicitly accepting the kept account authorizes the new device
        // membership. Preserve the original queued message and logical ID.
        let change = b
            .account_binding_changes(&a.account_id())
            .into_iter()
            .find(|change| change.device_id == a.user_id())
            .expect("recipient can review the sender's verified account change");
        b.accept_account_binding_change_guarded(&change, |commit| commit())
            .await
            .unwrap();
        a.flush_delivery(id).await;
    }
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        recipient_messages.recv(),
    )
    .await
    .expect("the external recipient must receive independently of sibling mirroring")
    .unwrap();
    assert_eq!(
        delivered.text.as_slice(),
        b"external recipient must receive"
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("only the recipient's authenticated receipt confirms delivery");
    for task in [at, bt, ct] {
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn own_sibling_archive_cannot_confirm_or_replace_external_recipient_delivery() {
    external_recipient_delivery_after_sibling_archive(false).await;
}

#[tokio::test]
async fn explicit_account_acceptance_restores_adopted_sender_delivery_without_false_sibling_receipt()
 {
    external_recipient_delivery_after_sibling_archive(true).await;
}

#[tokio::test]
async fn tracked_completion_and_active_fanout_skip_untracked_history_validation() {
    use std::sync::atomic::Ordering;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (sender, receiver, _, _, _, file, _, _) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    let sender_checks = sender.untracked_chunk_validations.load(Ordering::Relaxed);
    let chunks = sender.log.lock().unwrap().events(&file).unwrap().len();
    let sender_result =
        sender.save_file_progress(file, &ad.path().join("active-fanout.bin"), |progress| {
            if progress.done == progress.total {
                sender.log.lock().unwrap().fail_reads(true);
            }
        });
    assert_eq!(
        sender.untracked_chunk_validations.load(Ordering::Relaxed),
        sender_checks,
        "active fanout must not scan untracked chunk ancestry"
    );
    sender_result.expect("active fanout retains chunks without an untracked/share-policy lookup");
    sender.log.lock().unwrap().fail_reads(false);
    assert_eq!(
        sender.log.lock().unwrap().events(&file).unwrap().len(),
        chunks
    );

    let receiver_checks = receiver.untracked_chunk_validations.load(Ordering::Relaxed);
    receiver
        .save_file_progress(file, &bd.path().join("tracked.bin"), |progress| {
            if progress.done == progress.total {
                receiver.log.lock().unwrap().fail_reads(true);
            }
        })
        .expect("a tracked completion needs no unrelated manifest read after verifying chunks");
    assert_eq!(
        receiver.untracked_chunk_validations.load(Ordering::Relaxed),
        receiver_checks,
        "a valid tracked completion must not scan untracked chunk ancestry"
    );
}

#[tokio::test]
async fn owned_read_failure_delivery_snapshot_keeps_accepted_work_without_sending() {
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_with_account(
        &remote,
        &account,
        "remote",
        listener.local_addr().unwrap().port(),
    );
    let (sender, _) = node(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let id = sender
        .enqueue_to_account(&account.account_id(), b"accepted durable work", None)
        .await
        .unwrap();
    let destination = sender
        .delivery
        .lock()
        .unwrap()
        .next_destination(None)
        .unwrap()
        .1
        .event_id;
    let (entered, observed) = tokio::sync::oneshot::channel();
    let weak = Arc::downgrade(&sender);
    *sender.delivery_snapshot_hook.lock().unwrap() = Some(Box::new(move || {
        weak.upgrade().unwrap().log.lock().unwrap().fail_reads(true);
        let _ = entered.send(());
    }));
    let worker = tokio::spawn(sender.clone().run_delivery_loop());
    tokio::time::timeout(Duration::from_secs(2), observed)
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), listener.accept())
            .await
            .is_err(),
        "failed snapshot must not dial the destination"
    );
    assert_eq!(sender.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert_eq!(
        sender
            .delivery
            .lock()
            .unwrap()
            .next_destination(None)
            .unwrap()
            .1
            .event_id,
        destination
    );
    worker.abort();
    let _ = worker.await;
    sender.runtime_work.close();
    tokio::time::timeout(Duration::from_secs(1), sender.runtime_work.drain())
        .await
        .unwrap();
}

async fn owned_scope_sender_fixture(
    dir: &Path,
    pending: bool,
) -> (Arc<Node>, EventId, ConversationId, EventId) {
    let remote = Account::generate();
    let first = DeviceIdentity::generate();
    let second = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&first, &remote, "first", 9);
    let (sender, _) = node(dir, DeviceIdentity::generate(), Account::generate(), &proof);
    sender.roster.lock().unwrap().update(
        &Announce::new_with_account(&second, &remote, "second", 10),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &sender.user_id(),
    );
    let own = sender.signed_announce("sender", 11);
    sender
        .configure_privacy(
            dir,
            "pw",
            &own,
            Arc::new(crate::discovery::visibility::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    if pending {
        sender
            .received_files
            .lock()
            .unwrap()
            .fail_appends_for_test(true)
            .unwrap();
    }
    let path = dir.join("scope.txt");
    std::fs::write(&path, b"immutable scoped file").unwrap();
    let (id, file) = sender
        .enqueue_file_to_account(&remote.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let target = {
        let store = sender.delivery.lock().unwrap();
        let message = if pending {
            match store.pending_transactions()[0].body() {
                super::delivery_store::DeliveryTransaction::OutgoingManifest {
                    message, ..
                } => message,
                _ => panic!("expected outgoing file transaction"),
            }
        } else {
            store.message(id).unwrap()
        };
        assert_eq!(message.destinations.len(), 2);
        message
            .destinations
            .iter()
            .find(|destination| destination.event.id != id)
            .unwrap()
            .event
            .id
    };
    (sender, id, file, target)
}

#[tokio::test]
async fn owned_read_failure_late_scope_reseed_preserves_all_live_projections() {
    let directory = tempfile::tempdir().unwrap();
    let (sender, target, file, _) = owned_scope_sender_fixture(directory.path(), false).await;
    let sentinel = ConversationId::new([91; 32]);
    let manifest = crate::file::FileManifest {
        name: "sentinel".into(),
        size: 0,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(&[]),
        file_key: [7; 32],
        file_conv: sentinel,
        chunk_count: 1,
    };
    let original = crate::eventlog::Event::new(
        &sender.identity,
        ConversationId::new([93; 32]),
        1,
        vec![],
        1,
        0,
        crate::eventlog::EventKind::FileManifest,
        vec![91],
    );
    let row = super::received_log::ReceivedEntry {
        event_id: original.id,
        conversation: original.conversation_id,
        from: sender.user_id(),
        wall_clock: original.wall_clock,
        plaintext: manifest.encode(),
    };
    sender
        .received_files
        .lock()
        .unwrap()
        .record_file_with_reception(&row, Some(&original), None)
        .unwrap();
    let snapshot_book = || {
        let book = sender.files.lock().unwrap();
        let mut files = book.file_convs();
        files.sort_by_key(|file| *file.as_bytes());
        files
            .into_iter()
            .map(|file| {
                let mut ids = book.manifest_events(file);
                ids.sort();
                (
                    file,
                    book.manifest(file).unwrap(),
                    ids.iter()
                        .map(|id| (*id, book.is_emitted(id)))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let book_before = snapshot_book();
    let pending_before = sender.files.lock().unwrap().pending_for_test(sentinel);
    let scopes_before = sender
        .manifest_scopes(file)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    sender.log.lock().unwrap().fail_read_for(Some(target));
    assert!(matches!(
        sender.reseed_live_file_book(&sender.delivery.lock().unwrap()),
        Err(NodeError::Log(LogError::Io(_)))
    ));
    assert!(
        snapshot_book() == book_before,
        "late scope failure must not replace the live FileBook"
    );
    assert!(
        sender.files.lock().unwrap().pending_for_test(sentinel) == pending_before,
        "late scope failure must not trim pending files"
    );
    assert!(
        sender
            .manifest_scopes(file)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            == scopes_before,
        "late scope failure must not replace privacy scopes"
    );
    sender.log.lock().unwrap().fail_read_for(None);
    sender
        .reseed_live_file_book(&sender.delivery.lock().unwrap())
        .unwrap();
    assert!(
        sender
            .files
            .lock()
            .unwrap()
            .manifest(file)
            .unwrap()
            .is_some()
    );
    assert!(
        sender
            .files
            .lock()
            .unwrap()
            .manifest(sentinel)
            .unwrap()
            .is_none()
    );
    assert!(!sender.files.lock().unwrap().pending_for_test(sentinel));
}

#[tokio::test]
async fn owned_read_failure_manifest_recovery_uses_complete_wal_before_retirement() {
    use super::delivery_store::{DeliveryStore, DeliveryTransaction};
    let directory = tempfile::tempdir().unwrap();
    let (sender, id, file, target) = owned_scope_sender_fixture(directory.path(), true).await;
    let mut transaction: DeliveryTransaction = bincode::deserialize(
        &bincode::serialize(&sender.delivery.lock().unwrap().pending_transactions()[0]).unwrap(),
    )
    .unwrap();
    sender
        .received_files
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    sender.log.lock().unwrap().fail_read_for(Some(target));
    assert!(matches!(
        sender.log.lock().unwrap().get(&target),
        Err(LogError::Io(_))
    ));
    sender.recover_delivery(&mut sender.delivery.lock().unwrap()).expect("owned WAL events must complete scope preparation without a backing read after retirement");
    assert!(
        sender
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    assert!(
        sender
            .manifest_scopes(file)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .iter()
            .any(|scope| scope.event == target),
        "the additional destination scope must not be stranded after journal retirement"
    );
    assert_eq!(sender.delivery_status(id), Some(DeliveryStatus::Awaiting));
    sender.log.lock().unwrap().fail_read_for(None);
    let DeliveryTransaction::OutgoingManifest { message, .. } = &mut transaction else {
        panic!("expected outgoing file");
    };
    message
        .destinations
        .retain(|destination| destination.event.id != target);
    let mut broken = DeliveryStore::open(&directory.path().join("broken-journal"), "pw").unwrap();
    broken
        .bind_profile(
            &directory.path().join("events.log"),
            "pw",
            &sender.identity.public(),
            &sender.account_id(),
        )
        .unwrap();
    assert!(
        matches!(broken.begin(transaction), Err(LogError::CorruptFile(_))),
        "missing local WAL material is rejected by the existing delivery owner as local corruption"
    );
    assert!(broken.pending_transactions().is_empty());
}

#[tokio::test]
async fn owned_read_failure_relay_consent_aborts_before_successful_empty_export() {
    use super::opaque_relay::OpaqueMailboxProgress;
    use super::session::{Served, SessionError};
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (_, receiver, source, _, _, _, _, _) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    let relay = DeviceIdentity::generate();
    let relay_public = relay.public();
    let mailbox = crate::postoffice::opaque::RelayMailbox::new(
        &receiver.identity,
        &source.public(),
        &relay_public,
    )
    .unwrap();
    let relay_store = Arc::new(Mutex::new(crate::eventlog::EventLog::default()));
    let remote_store = relay_store.clone();
    let (client_io, server_io) = tokio::io::duplex(128 * 1024);
    let server = tokio::spawn(async move {
        let mut channel = crate::transport::SecureChannel::accept(server_io, &relay)
            .await
            .unwrap();
        while matches!(
            super::session::serve_one(&mut channel, &remote_store).await,
            Ok(Served::Handled(_))
        ) {}
    });
    let mut channel = crate::transport::SecureChannel::connect(
        client_io,
        &receiver.identity,
        Some(&relay_public),
    )
    .await
    .unwrap();
    let mut progress = OpaqueMailboxProgress::new(&relay_public, &source.public(), None, mailbox);
    receiver.log.lock().unwrap().fail_reads(true);
    assert!(matches!(
        receiver.file_consent_events(&source.public()),
        Err(LogError::Io(_))
    ));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        receiver.advance_opaque_mailbox(&mut channel, &mut progress),
    )
    .await;
    server.abort();
    let _ = server.await;
    assert!(
        matches!(result, Ok(Err(SessionError::Store(LogError::Io(_))))),
        "a failed consent read must reach the relay supervisor, not report a successful empty export"
    );
    assert!(progress.uploaded().is_empty());
    assert!(relay_store.lock().unwrap().conversations().is_empty());
}
