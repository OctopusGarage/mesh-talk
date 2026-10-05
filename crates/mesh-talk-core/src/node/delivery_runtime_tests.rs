use super::*;
use crate::discovery::{roster::Roster, Announce};
use crate::eventlog::{ConversationId, EventId};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, sync::mpsc};

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
            if result.is_ok() {
                if let Some(entered) = entered.take() {
                    let _ = entered.send(());
                }
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
    let log_empty = node.log.lock().unwrap().conversations().is_empty();
    assert!(matches!(result, Err(NodeError::Authorization(_))));
    assert!(node
        .delivery
        .lock()
        .unwrap()
        .pending_transactions()
        .is_empty());
    assert!(node
        .delivery
        .lock()
        .unwrap()
        .next_destination(None)
        .is_none());
    assert!(node
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_none());
    assert!(node.account_history(&remote.account_id(), 10).is_empty());
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
    eprintln!("denied grant: memory_unchanged={unchanged}, disk_unchanged={disk_unchanged}, staged_callbacks={staged}, log_empty={log_empty}");
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
    assert!(log_empty && reopened.log.lock().unwrap().conversations().is_empty());
    assert!(reopened.files.lock().unwrap().file_convs().is_empty());
    assert!(reopened
        .delivery
        .lock()
        .unwrap()
        .pending_transactions()
        .is_empty());
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
async fn host_enqueue_authorizes_before_privacy_and_inside_final_wal_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let bp = Announce::new_with_account(&bob, &ba, "Bob", 1);
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
        .enqueue_to_account_if(&ba.account_id(), b"before permission", None, |_| {
            Err(NodeError::Authorization("host replaced".into()))
        })
        .await;
    assert!(matches!(denied, Err(NodeError::Authorization(_))));
    assert!(a.privacy_snapshot().allowed_accounts.is_empty());
    let before_sessions = std::fs::read(dir.path().join("ratchet.sessions")).unwrap();
    let mut checks = 0;
    let denied = a
        .enqueue_to_account_if(&ba.account_id(), b"deny WAL", None, |accept| {
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
    assert!(a.log.lock().unwrap().conversations().is_empty());
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
            &ba.account_id(),
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
    assert!(a.account_history(&ba.account_id(), 10).is_empty());
    assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
    // Chunk staging was real, but no local file card/WAL transaction was accepted.
    assert!(!a.log.lock().unwrap().conversations().is_empty());
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
    let ba = Account::generate();
    let secret = bob.secret_bytes();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 1);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", 1);
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
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
    b.persist_account_adoption(&adopted.account_id(), || Ok(()))
        .unwrap();
    drop(b);
    let bob = DeviceIdentity::from_secret_bytes(secret.0, secret.1);
    let new_proof = Announce::new_with_account(&bob, &adopted, "Rekeyed Bob", 1);
    let (b, _) = node(bd.path(), bob, adopted, &ap);
    a.roster
        .lock()
        .unwrap()
        .update(&new_proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    assert!(super::delivery_receipt::open_receipt(
        &a.identity,
        &a.account_id(),
        &new_proof,
        &receipt
    )
    .is_none());
    assert!(a.log.lock().unwrap().has(&receipt.id));
    assert!(b.log.lock().unwrap().has(&receipt.id));
    assert_eq!(
        a.sync_store(&new_proof.public())
            .lock()
            .unwrap()
            .event_ids(&control),
        vec![receipt.id]
    );
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
        .durable_have(&control, &receipt.id));
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
    assert!(!b
        .sync_store(&ap.public())
        .lock()
        .unwrap()
        .durable_have(&control, &receipt.id));
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
    let ba = Account::generate();
    let other = DeviceIdentity::generate();
    let ca = Account::from_secret_bytes(aa.secret_bytes());
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let cp = Announce::new_with_account(&other, &ca, "Own copy", cl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    a.roster
        .lock()
        .unwrap()
        .update(&cp, IpAddr::V4(Ipv4Addr::LOCALHOST), &a.user_id());
    let (b, _) = node(bd.path(), bob, ba, &ap);
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
        while c.file_progress(file).is_none() || c.read_file(file).is_err() {
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
    assert!(c.historical_file_completion(file, final_chunk, &ap.public()));
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
    let history = a.account_history(&b.account_id(), 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert_eq!(b.account_history(&a.account_id(), 10)[0].id, actual);
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
    assert_eq!(a.account_history(&b.account_id(), 10)[0].id, id);
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_none());
}

#[tokio::test]
async fn file_enqueue_progress_never_waits_for_stalled_contact_tcp() {
    for account_addressed in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let ba = Account::generate();
        let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bp = Announce::new_with_account(&bob, &ba, "Bob", stalled.local_addr().unwrap().port());
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
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            if account_addressed {
                a.enqueue_file_to_account_progress(
                    &ba.account_id(),
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
async fn private_file_restart_progresses_when_protocol_stages_fit_but_total_exceeds_operation_budget(
) {
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
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> bool {
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
        false
    }
    fn admission_denied(&self) -> bool {
        self.inner.admission_denied()
    }
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        if *conversation == self.file || *conversation == self.dm {
            self.inner.event_ids(conversation)
        } else {
            Vec::new()
        }
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &std::collections::HashSet<EventId>,
    ) -> Vec<crate::eventlog::Event> {
        if *conversation == self.file || *conversation == self.dm {
            self.inner.events_excluding(conversation, have)
        } else {
            Vec::new()
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
async fn file_terminal_negative_custody_retries_on_cadence_without_empty_cache_self_wake() {
    use crate::eventlog::sync::SyncStore;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, ba, &ap);
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
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        sync_delivery_test_stage(&a, &b, &bl, dm).await;
        sync_delivery_test_stage(&a, &b, &bl, file).await;
    })
    .await
    .unwrap();
    let ids = a.sync_store(&bp.public()).lock().unwrap().event_ids(&file);
    assert_eq!(
        b.sync_store(&ap.public()).lock().unwrap().event_ids(&file),
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
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_some());
    assert_eq!(
        a.sync_store(&bp.public()).lock().unwrap().event_ids(&file),
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
        let ba = Account::generate();
        let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
        let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
        let (a, _) = node(ad.path(), alice, aa, &bp);
        let (b, _) = node(bd.path(), bob, ba, &ap);
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
        assert!(b.log.lock().unwrap().events(&file).is_empty());
        assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
        workers.shutdown().await;
    }
}

async fn private_file_restart(delay: FilePeerDelay) {
    private_file_restart_batch(delay, 1).await;
}

async fn private_file_restart_batch(delay: FilePeerDelay, count: usize) {
    let mut case = super::delivery_runtime::TestTiming::new("file-restart-case-setup");
    use crate::eventlog::sync::SyncStore;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let secret = alice.secret_bytes();
    let account_secret = aa.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, ba, &ap);
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
    a.set_allowed(&b.account_id(), true).await.unwrap();
    b.set_allowed(&a.account_id(), true).await.unwrap();
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
        .iter()
        .map(|e| e.id)
        .collect::<Vec<_>>();
    let mut originals = vec![(id, file, ids.clone())];
    // Reproduce missing volatile scope: DM card/control remain authorized, while
    // chunk disclosure is blocked until durable scope import on reopen.
    for (_, file, _) in &originals {
        a.privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .file_scopes
            .remove(file);
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
                .iter()
                .map(|e| e.id)
                .collect();
            a.privacy
                .state
                .write()
                .unwrap()
                .as_mut()
                .unwrap()
                .file_scopes
                .remove(&file);
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
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
    drop(al);
    drop(bl);
    assert!(confirmed.is_ok());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    for (id, file, _) in &originals {
        assert_eq!(a.delivery_status(*id), Some(DeliveryStatus::Delivered));
        assert!(b.read_file(*file).is_err(), "every card precedes download");
    }
    assert!(
        !a.privacy
            .state
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .file_scopes
            .contains_key(&file),
        "card ACK is installed before the missing chunk scope is restored"
    );
    assert!(
        b.read_file(file).is_err(),
        "card delivery is independent of complete download"
    );
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_some());
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
        assert_eq!(a.account_history(&b.account_id(), 10)[0].id, id);
    } else {
        assert!(a
            .account_history(&b.account_id(), 100)
            .iter()
            .any(|entry| entry.id == id));
    }
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Delivered));
    assert_eq!(
        a.log
            .lock()
            .unwrap()
            .events(&file)
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        a.sync_store(&bp.public()).lock().unwrap().event_ids(&file),
        ids,
        "restart restores the original authorized chunk projection"
    );
    for (_, file, ids) in &originals {
        assert_eq!(
            a.sync_store(&bp.public()).lock().unwrap().event_ids(file),
            *ids
        );
    }
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
            while !originals
                .iter()
                .any(|(_, file, _)| b.file_progress(*file).is_some_and(|p| p.done == p.total))
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok()
    } else {
        true
    };
    let mut slowest_poll = std::time::Duration::ZERO;
    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(if count == 1 { 5 } else { 60 }),
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
                        b.file_progress(*file).is_some_and(|p| p.done == p.total)
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
    workers.shutdown().await;
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
        eprintln!("file-backlog observer slowest-poll-ms={} completed={}/{} accepted={} max-file-channels={}",
            slowest_poll.as_millis(), originals.iter().filter(|(_, file, _)| b.file_progress(*file).is_some_and(|p| p.done == p.total)).count(),
            count, observation.next_connection.load(std::sync::atomic::Ordering::SeqCst), observation.max_file_channels.load(std::sync::atomic::Ordering::SeqCst));
        let final_chunk = *ids.last().unwrap();
        let exact_bytes = b
            .read_file(file)
            .as_ref()
            .is_ok_and(|actual| actual == &bytes);
        let counts = b.file_progress(file).map(|p| (p.done, p.total));
        let held = b
            .sync_store(&ap.public())
            .lock()
            .unwrap()
            .durable_have(&file, &final_chunk);
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
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>()
            == ids;
        eprintln!("file-restart failure chunks={counts:?} exact-bytes={exact_bytes} final-authorized-have={held} pending-work={pending} immutable-ids={immutable}");
    }
    assert!(
        completed.is_ok(),
        "restart must restore immutable chunk work and private scope"
    );
    assert_eq!(b.read_file(file).unwrap(), bytes);
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
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            *ids
        );
        assert!(b
            .sync_store(&ap.public())
            .lock()
            .unwrap()
            .durable_have(file, ids.last().unwrap()));
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
        assert_eq!(a.account_history(&b.account_id(), 10)[0].id, id);
    } else {
        assert!(a
            .account_history(&b.account_id(), 100)
            .iter()
            .any(|entry| entry.id == id));
    }
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_none());
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
                            .is_some_and(|conv| node.file_progress(conv).is_some())
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
                        tokio::time::sleep(std::time::Duration::from_millis(
                            if delay == FilePeerDelay::Exchange {
                                250
                            } else {
                                150
                            },
                        ))
                        .await;
                    } else if super::session::is_round_request(&bytes, file)
                        && node
                            .file_progress(file)
                            .is_some_and(|progress| progress.done == 1)
                        && !observation
                            .interrupted
                            .swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        observation
                            .interrupted_connection
                            .store(connection, std::sync::atomic::Ordering::SeqCst);
                        // One original chunk is already admitted. Force this
                        // round past 400ms once; retries must abandon its socket.
                        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
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
                                && node.file_progress(file).is_some_and(|p| p.done == p.total)
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
    let ba = Account::generate();
    let secret = bob.secret_bytes();
    let mut account_secret = ba.secret_bytes();
    let (b, _) = node(dir.path(), bob, ba, &ap);
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
    assert_eq!(b.account_history(&aa.account_id(), 10).len(), 1);
    assert_eq!(
        b.delete_account_message(&aa.account_id(), event.id)
            .unwrap(),
        1
    );
    if adopt {
        let account = Account::generate();
        b.persist_account_adoption(&account.account_id(), || Ok(()))
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
        b.account_history(&aa.account_id(), 10).is_empty(),
        "static manifest must stay locally erased"
    );
    assert!(b.delivery.lock().unwrap().retry_receipts(1).is_empty());
    assert!(b
        .files
        .lock()
        .unwrap()
        .manifest(&manifest.file_conv)
        .is_none());
}

#[tokio::test]
async fn automatic_file_card_receipt_and_chunk_resume_without_manual_flush() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let aa = Account::generate();
    let ba = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, ba, &ap);
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
        let sidecar = dir.path().join("sidecar");
        *a.received_files.lock().unwrap() =
            ReceivedLog::open(&sidecar.join("files"), "pw").unwrap();
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
        let history = a.dm_history(&bob.public(), 10);
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
        assert_eq!(a.dm_history(&bob.public(), 10)[0].id, id);
        assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
        assert!(a.delivery.lock().unwrap().pending_transactions().is_empty());
        assert!(a.files.lock().unwrap().manifest(&file).is_some());
    }
}

#[tokio::test]
async fn oversized_text_is_rejected_before_acceptance_and_ratchet_install() {
    let dir = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(dir.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
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
    assert!(a.log.lock().unwrap().conversations().is_empty());
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
    let ba = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, mut rx) = node(bd.path(), bob, ba, &ap);
    let id = a
        .enqueue_to_account(&b.account_id(), b"live recovery", None)
        .await
        .unwrap();
    let event = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
        .event
        .clone();
    b.log.lock().unwrap().append_durable(event.clone()).unwrap();
    let sidecar = bd.path().join("sidecar");
    *b.received.lock().unwrap() = ReceivedLog::open(&sidecar.join("received"), "pw").unwrap();
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
        let ba = Account::generate();
        let secret = bob.secret_bytes();
        let account_secret = ba.secret_bytes();
        let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
        let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
        let (a, _) = node(ad.path(), alice, aa, &bp);
        let (mut b, _, mut files) = node_with_files(bd.path(), bob, ba, &ap);
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
        let sidecar = bd.path().join("sidecar");
        *b.received_files.lock().unwrap() =
            ReceivedLog::open(&sidecar.join("files"), "pw").unwrap();
        b.received_files
            .lock()
            .unwrap()
            .fail_appends_for_test(true)
            .unwrap();
        b.process_file_events(event.conversation_id);
        assert!(files.try_recv().is_err());
        assert!(b.delivery.lock().unwrap().retry_receipts(1).is_empty());
        assert!(b.files.lock().unwrap().manifest(&file).is_none());
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
        assert_eq!(b.account_history(&a.account_id(), 10).len(), 1);
        assert!(b.files.lock().unwrap().manifest(&file).is_some());
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
    assert_eq!(b.account_history(&ap.account_id().unwrap(), 10).len(), 1);
    manifest.file_key = [7; 32];
    let collision = seal(&manifest, conv, 3, vec![valid.id]);
    b.log.lock().unwrap().append_durable(collision).unwrap();
    b.process_file_events(conv);
    assert_eq!(
        b.account_history(&ap.account_id().unwrap(), 10).len(),
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
                .manifest(&manifest.file_conv)
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
                b.reseed_live_file_book(&b.delivery.lock().unwrap());
                (b, None)
            };
            let book = b.files.lock().unwrap();
            let expected = matches!(shape, "valid-account-host" | "conflicting-alias");
            assert_eq!(
                book.manifest(&manifest.file_conv).is_some(),
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
                    book.manifest(&manifest.file_conv).unwrap(),
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
    let mut row = a.received_files.lock().unwrap().entry(id).unwrap().clone();
    row.wall_clock += 1;
    let mut rows = ReceivedLog::open(&dir.path().join("corrupted-files.log"), "pw").unwrap();
    rows.record_durable(&row).unwrap();
    *a.received_files.lock().unwrap() = rows;
    a.configure_privacy(
        dir.path(),
        "pw",
        &a.signed_announce("Source", 9),
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    assert!(a.files.lock().unwrap().manifest(&file).is_none());
    assert!(!a
        .privacy
        .state
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .file_scopes
        .contains_key(&file));
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
    let host =
        super::conversation::account_conversation_id(&b.account_id(), &ap.account_id().unwrap());
    let row = b.received_files.lock().unwrap().entries(&host)[0].clone();
    let sidecar = dir.path().join("sidecar");
    let mut files = ReceivedLog::open(&sidecar.join("files"), "pw").unwrap();
    files.record_durable(&row).unwrap();
    *b.received_files.lock().unwrap() = files;
    std::fs::create_dir(sidecar.join("files.compact-tmp")).unwrap();
    assert!(b
        .delete_account_message(&ap.account_id().unwrap(), event.id)
        .is_err());
    assert!(b.account_history(&ap.account_id().unwrap(), 10).is_empty());
    assert!(
        b.files
            .lock()
            .unwrap()
            .manifest(&manifest.file_conv)
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
    b.log.lock().unwrap().append_durable(event).unwrap();
    b.received_files
        .lock()
        .unwrap()
        .record_durable(&ReceivedEntry {
            event_id: id,
            conversation: host,
            from: alice.user_id(),
            wall_clock: 1,
            plaintext: manifest.encode(),
        })
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
    b.reseed_live_file_book(&store);
    *b.delivery.lock().unwrap() = store;
    assert!(b
        .delete_account_message(&ap.account_id().unwrap(), id)
        .is_err());
    assert_eq!(b.account_history(&ap.account_id().unwrap(), 10)[0].id, id);
    assert!(b
        .files
        .lock()
        .unwrap()
        .manifest(&manifest.file_conv)
        .is_some());
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
    assert_eq!(b.account_history(&aa.account_id(), 10).len(), 1);
    assert_eq!(b.account_history(&ca.account_id(), 10).len(), 1);
    b.delete_account_message(&aa.account_id(), originals[0].id)
        .unwrap();
    {
        let state = b.privacy.state.read().unwrap();
        let scopes = &state.as_ref().unwrap().file_scopes[&manifest.file_conv];
        assert!(
            scopes.iter().all(|scope| scope.event != originals[0].id),
            "erased alias scope must be removed"
        );
        assert!(scopes.iter().any(|scope| scope.event == originals[1].id));
    }
    assert!(b
        .files
        .lock()
        .unwrap()
        .manifest(&manifest.file_conv)
        .is_some());
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
    assert!(b.account_history(&aa.account_id(), 10).is_empty());
    assert_eq!(
        b.account_history(&ca.account_id(), 10)[0].id,
        originals[1].id
    );
    assert!(b
        .files
        .lock()
        .unwrap()
        .manifest(&manifest.file_conv)
        .is_some());
    assert_eq!(b.prune_older_than(101).unwrap(), 1);
    assert!(b
        .files
        .lock()
        .unwrap()
        .manifest(&manifest.file_conv)
        .is_none());
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
    let history = a.dm_history(&bob.public(), 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].file.as_ref().unwrap().file_conv, file_conv);
    assert_eq!(
        a.delivery_status(history[0].id),
        Some(DeliveryStatus::Awaiting)
    );
    assert!(a.sentlog.lock().unwrap().conversations().is_empty());
}

#[test]
fn oversized_manifest_is_rejected_before_staging_ciphertext_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let bp = Announce::new_with_account(&bob, &Account::generate(), "Bob", 9);
    let (a, _) = node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &bp,
    );
    let path = dir.path().join("oversized.txt");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(4 * 1024 * 1024 * 1024)
        .unwrap();
    let before = a.log.lock().unwrap().conversations();
    let result = a.stage_file(&path, crate::file::FileKind::File, |_| {
        panic!("transport-impossible manifests must fail before the first chunk");
    });
    assert!(
        matches!(result, Err(NodeError::InvalidInput(ref error)) if error.contains("transport frame"))
    );
    assert_eq!(a.log.lock().unwrap().conversations(), before);
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
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &identity.public().user_id(),
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (file, files) = mpsc::unbounded_channel();
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
        files,
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
async fn file_post_office_final_custody_does_not_retire_target_resume_after_early_card_ack() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp_port = bl.local_addr().unwrap().port();
    drop(bl);
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bp_port);
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, ba, &ap);
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
    assert!(relay.lock().unwrap().has(&final_chunk));
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
    assert_eq!(b.read_file(file).unwrap(), bytes);
}

#[tokio::test]
async fn media_prune_before_final_probe_does_not_force_chunk_retransmission_or_residue() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let akeys = alice.secret_bytes();
    let aaccount = aa.secret_bytes();
    let bkeys = bob.secret_bytes();
    let baccount = ba.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, ba, &ap);
    let bytes = vec![6; 1024];
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
    assert_eq!(b.file_progress(file).unwrap().done, 0);
    assert!(b.file_ready_to_save(file));
    assert!(b.log.lock().unwrap().events(&file).is_empty());
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_some());
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
    let ba = Account::from_secret_bytes(baccount);
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _) = node(bd.path(), bob, ba, &ap);
    assert_eq!(b.read_media(file).unwrap(), bytes);
    assert!(b.historical_file_completion(file, final_chunk, &ap.public()));
    assert!(!b.historical_file_completion(file, EventId::new([1; 32]), &ap.public()));
    assert!(!b.historical_file_completion(file, final_chunk, &DeviceIdentity::generate().public()));
    let exported = b.save_file_into_dir(file, bd.path()).unwrap();
    assert_eq!(std::fs::read(exported).unwrap(), bytes);
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    a.flush_file_delivery(id).await;
    bt.abort();
    let _ = bt.await;
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_none());
    assert!(b.log.lock().unwrap().events(&file).is_empty(), "completed/pruned media must not be retransmitted and retained merely to satisfy exact final custody");
    std::fs::remove_file(b.media.path(file).unwrap()).unwrap();
    assert!(!b.file_ready_to_save(file));
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
    let ba = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", 9);
    let (a, _) = node(ad, alice, aa, &bp);
    let (b, _) = node(bd, bob, ba, &ap);
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
        .into_iter()
        .cloned()
        .collect();
    let final_chunk = chunks.last().unwrap().id;
    for chunk in chunks {
        b.log.lock().unwrap().append_durable(chunk).unwrap();
    }
    (a, b, ap, bp, id, file, final_chunk, bytes)
}

#[tokio::test]
async fn generic_save_completion_prunes_and_retires_without_second_transfer_but_keeps_source_fanout(
) {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let (a, b, ap, _, id, file, final_chunk, bytes) =
        completed_file_fixture(ad.path(), bd.path(), crate::file::FileKind::File).await;
    let source_copy = ad.path().join("source-export.bin");
    a.save_file(file, &source_copy).unwrap();
    assert_eq!(std::fs::read(source_copy).unwrap(), bytes);
    assert!(!a.log.lock().unwrap().events(&file).is_empty());
    let exported = b.save_file_into_dir(file, bd.path()).unwrap();
    assert_eq!(std::fs::read(exported).unwrap(), bytes);
    assert!(b.log.lock().unwrap().events(&file).is_empty());
    assert!(b.historical_file_completion(file, final_chunk, &ap.public()));
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
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_none());
    assert!(b.log.lock().unwrap().events(&file).is_empty());
    assert_eq!(b.file_progress(file).unwrap().done, 0);
    assert!(!b.file_ready_to_save(file));
    // Local file-save proofs have no status authority: the queued card ACK is separate.
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    a.save_file(file, &ad.path().join("source-after-retirement.bin"))
        .unwrap();
    assert!(
        a.log.lock().unwrap().events(&file).is_empty(),
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
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
    assert_eq!(b.read_file(file).unwrap(), bytes);
    let outbox = bd.path().join("events.log.delivery-outbox");
    let moved = bd.path().join("outbox-held");
    std::fs::rename(&outbox, &moved).unwrap();
    let destination = bd.path().join("saved.bin");
    b.save_file(file, &destination).unwrap();
    assert_eq!(std::fs::read(destination).unwrap(), bytes);
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
    assert_eq!(b.read_file(file).unwrap(), bytes);
    std::fs::rename(moved, outbox).unwrap();
    b.save_file(file, &bd.path().join("saved-again.bin"))
        .unwrap();
    assert!(b.historical_file_completion(file, final_chunk, &ap.public()));
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
        .into_iter()
        .cloned()
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
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
    assert!(!b.log.lock().unwrap().events(&file).is_empty());
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
    assert_eq!(receiver.completion_candidates(file).len(), 1);
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
            assert!(!receiver.historical_file_completion(file, final_chunk, &proof.public()));
            assert_eq!(receiver.log.lock().unwrap().events(&file).len(), 1);
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
            assert!(!receiver.historical_file_completion(file, final_chunk, &proof.public()));
            assert_eq!(receiver.log.lock().unwrap().events(&file).len(), 1);
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
    assert!(b.historical_file_completion(file, final_chunk, &ap.public()));
    let mut wrong_x = ap.public();
    wrong_x.x25519_pub = [9; 32];
    assert!(!b.historical_file_completion(file, final_chunk, &wrong_x));
    b.set_allowed(&ap.account_id().unwrap(), false)
        .await
        .unwrap();
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
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
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
    let account = Account::generate();
    let owner = account.account_id();
    let keys = b.identity.secret_bytes();
    b.persist_account_adoption(&owner, || Ok(())).unwrap();
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
    drop(b);
    let (b, _) = node(
        bd.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        account,
        &ap,
    );
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
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
    assert!(b.historical_file_completion(file, final_chunk, &ap.public()));
    let row = b
        .received_files
        .lock()
        .unwrap()
        .entry(alias.id)
        .unwrap()
        .clone();
    let sidecar = bd.path().join("alternate-sidecar");
    let mut files = ReceivedLog::open(&sidecar.join("files"), "pw").unwrap();
    files.record_durable(&row).unwrap();
    *b.received_files.lock().unwrap() = files;
    std::fs::create_dir(sidecar.join("files.compact-tmp")).unwrap();
    assert!(b.delete_message(host, alias.id, false).is_err());
    assert!(b.received_files.lock().unwrap().entry(alias.id).is_some());
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
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
        .into_iter()
        .cloned()
        .collect();
    let final_chunk = chunks.last().unwrap().id;
    for chunk in chunks {
        b.log.lock().unwrap().append_durable(chunk).unwrap();
    }
    assert!(b.persist_media_if_complete(file));
    assert_eq!(b.read_media(file).unwrap(), bytes);
    assert!(b.historical_file_completion(file, final_chunk, &ap.public()));
    assert!(!b.historical_file_completion(file, final_chunk, &DeviceIdentity::generate().public()));
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
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_none());
    assert!(b.log.lock().unwrap().events(&file).is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    let rebound = Announce::new_with_account(&a.identity, &Account::generate(), "Alice", 9);
    b.roster
        .lock()
        .unwrap()
        .update(&rebound, IpAddr::V4(Ipv4Addr::LOCALHOST), &b.user_id());
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
    let own = Announce::new_with_account(&b.identity, &b.account, "Bob", 9);
    b.configure_privacy(
        bd.path(),
        "pw",
        &own,
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    b.set_invisible(true).await.unwrap();
    assert!(!b.historical_file_completion(file, final_chunk, &ap.public()));
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
    assert!(a.log.lock().unwrap().events(&file).is_empty());
    assert_eq!(a.file_progress(file).unwrap().done, 0);
    assert!(a.file_ready_to_save(file));
    assert!(!a.has_verified_file_completion(file));
    let exported = a.save_file_into_dir(file, dir.path()).unwrap();
    assert_eq!(std::fs::read(exported).unwrap(), bytes);
    let cached = a.media.path(file).unwrap();
    std::fs::write(&cached, b"wrong bytes").unwrap();
    assert!(a.file_ready_to_save(file)); // A present eligible copy is attempted, then verified by save.
    assert!(a
        .save_file(file, &dir.path().join("bad-export.png"))
        .is_err());
    std::fs::remove_file(cached).unwrap();
    assert!(!a.file_ready_to_save(file));
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
    *a.sentlog.lock().unwrap() =
        super::sentlog::SentLog::open(&sidecar_dir.join("sent"), "pw").unwrap();
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
    *b.received.lock().unwrap() = ReceivedLog::open(&sidecar.join("received"), "pw").unwrap();
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
async fn queued_private_file_work_stops_on_permission_revoke_and_exact_account_rebind() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let ba = Account::generate();
    let keys = bob.secret_bytes();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _) = node(ad.path(), alice, aa, &bp);
    let (b, _, mut files) = node_with_files(bd.path(), bob, ba, &ap);
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
    assert!(b.log.lock().unwrap().events(&file).is_empty());
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
    assert!(b.log.lock().unwrap().events(&file).is_empty());
    assert_eq!(a.delivery_status(id), Some(DeliveryStatus::Awaiting));
    assert!(a
        .delivery
        .lock()
        .unwrap()
        .next_file_destination(None)
        .is_some());
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
    wait_for_durable_delivery(&a, first).await;
    case.phase("receipt-case-after-clear-restart");
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
    wait_for_durable_delivery(&a, second).await;
    case.phase("receipt-case-after-account-adoption");
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
    wait_for_durable_delivery(&a, third).await;
    assert_eq!(a.delivery_status(third), Some(DeliveryStatus::Delivered));
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    case.finish();
}
