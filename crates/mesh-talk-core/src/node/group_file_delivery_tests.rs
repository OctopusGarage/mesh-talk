//! Ordinary group files cross opaque federation without a direct client route.
use super::{Node, ReceiveDecision, ReceivedFile, test_support::node_with_files};
use crate::{
    discovery::Announce,
    eventlog::{ConversationId, EventId},
    identity::{account::Account, device::DeviceIdentity},
    postoffice::{PostOffice, federation::FederationDirectory},
};
use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};

struct GroupRelayFixture {
    directory: tempfile::TempDir,
    a: Arc<Node>,
    b: Arc<Node>,
    offers: mpsc::UnboundedReceiver<ReceivedFile>,
    ap: Announce,
    bp: Announce,
    akeys: ([u8; 32], [u8; 32]),
    bkeys: ([u8; 32], [u8; 32]),
    aaccount: [u8; 32],
    baccount: [u8; 32],
    group: ConversationId,
    p: DeviceIdentity,
    q: DeviceIdentity,
    pproof: Announce,
    qproof: Announce,
    qaddr: SocketAddr,
    ps: Arc<Mutex<PostOffice>>,
    qs: Arc<Mutex<PostOffice>>,
    federation: FederationDirectory,
    tasks: Vec<JoinHandle<()>>,
}

fn duplicate(identity: &DeviceIdentity) -> DeviceIdentity {
    let (ed, x) = identity.secret_bytes();
    DeviceIdentity::from_secret_bytes(ed, x)
}

// These fixtures intentionally run no participant worker, so mailbox and raw
// signed-event intake must explicitly resume the same bounded channel/file work.
async fn settle_group_files(node: &Node, channel: ConversationId) {
    node.await_channel_metadata(channel).await.unwrap();
    for _ in 0..64 {
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts()
            .0;
        node.process_channel_file_reception(channel, "live")
            .unwrap();
        let after = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts()
            .0;
        assert!(
            after - before <= 64,
            "one file turn must retain its body budget"
        );
        if node.pending_channel_file().is_none() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("group file fixture must complete its bounded continuation");
}

impl GroupRelayFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        for name in ["a", "b", "relays"] {
            std::fs::create_dir(directory.path().join(name)).unwrap();
        }
        // Neither client can complete a direct Noise handshake. Immediate EOF
        // also avoids platform-dependent connect waits on a closed TCP port.
        let rejecting = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = rejecting.local_addr().unwrap().port();
        let rejecting_task = tokio::spawn(async move {
            while let Ok((stream, _)) = rejecting.accept().await {
                drop(stream);
            }
        });
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let akeys = alice.secret_bytes();
        let bkeys = bob.secret_bytes();
        let aaccount = aa.secret_bytes();
        let baccount = bob_account.secret_bytes();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", port);
        let bp = Announce::new_with_account(&bob, &bob_account, "Bob", port);
        let (a, _, _) = node_with_files(&directory.path().join("a"), alice, aa, &bp);
        let (b, _, offers) = node_with_files(&directory.path().join("b"), bob, bob_account, &ap);
        // Seed only genuine signed group setup controls. The offer and every
        // file byte below must travel through actual encrypted TCP mailboxes.
        let group = a
            .create_channel("Remote files", vec![bp.public()])
            .await
            .unwrap();
        let setup = a
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>();
        for event in setup {
            b.log.lock().unwrap().append_durable(event).unwrap();
        }
        b.await_channel_metadata(group).await.unwrap();
        a.set_bridge_discovery_enabled(true);
        b.set_bridge_discovery_enabled(true);
        // Group authority must be sufficient; no private-DM permission is added.
        a.replace_verified_dm_pull_peers(vec![]);
        b.replace_verified_dm_pull_peers(vec![]);
        let p = DeviceIdentity::generate();
        let q = DeviceIdentity::generate();
        let pl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ql = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let paddr = pl.local_addr().unwrap();
        let qaddr = ql.local_addr().unwrap();
        let pproof = Announce::new_post_office(&p, "P", paddr.port());
        let qproof = Announce::new_post_office(&q, "Q", qaddr.port());
        let ps = Arc::new(Mutex::new(
            PostOffice::open(&directory.path().join("relays/p"), "pw", duplicate(&p)).unwrap(),
        ));
        let qs = Arc::new(Mutex::new(
            PostOffice::open(&directory.path().join("relays/q"), "pw", duplicate(&q)).unwrap(),
        ));
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
        a.roster
            .lock()
            .unwrap()
            .update(&pproof, paddr.ip(), &a.user_id());
        b.roster
            .lock()
            .unwrap()
            .update(&qproof, qaddr.ip(), &b.user_id());
        Self {
            directory,
            a,
            b,
            offers,
            ap,
            bp,
            akeys,
            bkeys,
            aaccount,
            baccount,
            group,
            p,
            q,
            pproof,
            qproof,
            qaddr,
            ps,
            qs,
            federation: FederationDirectory::new(),
            tasks: vec![rejecting_task, pt, qt],
        }
    }

    async fn federate(&self) {
        self.federation
            .sync_with_relay(self.qaddr, &self.p, &self.q.public(), &self.ps)
            .await
            .unwrap();
    }

    async fn offer(&mut self) -> (EventId, ConversationId, Vec<u8>) {
        self.offer_expiring(None).await
    }

    async fn offer_expiring(
        &mut self,
        expires_at_ms: Option<u64>,
    ) -> (EventId, ConversationId, Vec<u8>) {
        let (manifest, file, payload) = self.enqueue_offer_expiring(expires_at_ms).await;
        self.federate().await;
        self.b.drain_from_post_office().await;
        settle_group_files(&self.b, self.group).await;
        let offer = self.offers.try_recv().expect("an authorized group offer must arrive through federation without granting private-DM access");
        assert_eq!(offer.event_id, manifest);
        assert_eq!(offer.file_conv, file);
        assert!(offer.is_channel);
        assert_eq!(self.b.file_progress(file).unwrap().unwrap().done, 0);
        (manifest, file, payload)
    }

    async fn enqueue_offer_expiring(
        &self,
        expires_at_ms: Option<u64>,
    ) -> (EventId, ConversationId, Vec<u8>) {
        let payload = vec![0x9d; crate::file::CHUNK_SIZE * 2 + 73];
        let source = self.directory.path().join("ordinary-document.bin");
        std::fs::write(&source, &payload).unwrap();
        let (manifest, file) = self
            .a
            .enqueue_file_channel_progress_expiring_if(
                self.group,
                &source,
                crate::file::FileKind::File,
                |_| {},
                expires_at_ms,
                |commit| commit(),
            )
            .await
            .unwrap();
        // The manual attachment worker also needs the just-accepted signed
        // manifest's current authority before selecting recipient custody.
        self.a.await_channel_metadata(self.group).await.unwrap();
        self.a.flush_channel_file(self.group, file).await;
        (manifest, file, payload)
    }

    async fn return_controls(&self) {
        self.b.drain_from_post_office().await;
        settle_group_files(&self.b, self.group).await;
        self.federate().await;
        self.a.drain_from_post_office().await;
        settle_group_files(&self.a, self.group).await;
    }

    async fn transfer_tick(&self) {
        // This is the existing production attachment worker seam. It must
        // retry relay-backed outgoing group work even with no incoming files.
        self.a.await_channel_metadata(self.group).await.unwrap();
        self.a.pull_pending_files().await;
        self.federate().await;
        self.b.drain_from_post_office().await;
        settle_group_files(&self.b, self.group).await;
    }

    fn restart_clients(&mut self) {
        // Both nodes are quiescent and run no background tasks. Release each
        // old writer before reopening its encrypted logs, rather than having
        // two Node instances own the same storage even briefly.
        let old_a = std::mem::replace(&mut self.a, self.b.clone());
        drop(old_a);
        let (a, _, _) = node_with_files(
            &self.directory.path().join("a"),
            DeviceIdentity::from_secret_bytes(self.akeys.0, self.akeys.1),
            Account::from_secret_bytes(self.aaccount),
            &self.bp,
        );
        self.a = a.clone();
        let old_b = std::mem::replace(&mut self.b, a.clone());
        drop(old_b);
        let (b, _, offers) = node_with_files(
            &self.directory.path().join("b"),
            DeviceIdentity::from_secret_bytes(self.bkeys.0, self.bkeys.1),
            Account::from_secret_bytes(self.baccount),
            &self.ap,
        );
        for node in [&a, &b] {
            node.set_bridge_discovery_enabled(true);
            node.replace_verified_dm_pull_peers(vec![]);
        }
        a.roster
            .lock()
            .unwrap()
            .update(&self.pproof, self.qaddr.ip(), &a.user_id());
        b.roster
            .lock()
            .unwrap()
            .update(&self.qproof, self.qaddr.ip(), &b.user_id());
        self.a = a;
        self.b = b;
        self.offers = offers;
    }
}

impl Drop for GroupRelayFixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[tokio::test]
async fn ordinary_group_file_crosses_federation_after_consent_and_client_restart() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut f = GroupRelayFixture::new().await;
        let (manifest, file, payload) = f.offer().await;
        f.return_controls().await;
        f.transfer_tick().await;
        assert_eq!(
            f.b.file_progress(file).unwrap().unwrap().done,
            0,
            "ordinary file bytes require explicit recipient consent"
        );
        f.b.decide_attachment(file, true).unwrap();
        f.return_controls().await;
        f.restart_clients();
        assert_eq!(
            f.b.attachment_consent_status(file).unwrap(),
            Some(ReceiveDecision::Accepted)
        );
        for _ in 0..8 {
            f.return_controls().await;
            f.transfer_tick().await;
            if f.b.file_ready_to_save(file).unwrap() {
                break;
            }
        }
        assert_eq!(
            f.b.read_file(file).expect(
                "consented ordinary group bytes must arrive without any direct client route"
            ),
            payload
        );
        assert_eq!(
            f.a.delivery_status(manifest),
            None,
            "group file transfer must not create private-DM delivery status"
        );
        let saved = f.directory.path().join("saved-document.bin");
        f.b.save_file(file, &saved).unwrap();
        assert_eq!(std::fs::read(saved).unwrap(), payload);
        f.return_controls().await;
        let control =
            f.a.file_consent
                .lock()
                .unwrap()
                .received_control(manifest)
                .unwrap()
                .expect("verified group completion must return through federation");
        assert!(
            super::file_consent::Decision::open(&control)
                .unwrap()
                .completion
                .is_some(),
            "relay custody alone cannot replace verified recipient completion"
        );
        assert!(!f.ps.lock().unwrap().has(&control.id));
        assert!(!f.qs.lock().unwrap().has(&control.id));
    })
    .await
    .expect("group file federation must make bounded progress");
}

#[tokio::test]
async fn declined_group_file_and_removed_recipient_do_not_receive_relay_chunks() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut f = GroupRelayFixture::new().await;
        let (_, file, _) = f.offer().await;
        f.b.decide_attachment(file, true).unwrap();
        f.return_controls().await;
        f.b.decide_attachment(file, false).unwrap();
        f.return_controls().await;
        f.transfer_tick().await;
        assert_eq!(
            f.b.file_progress(file).unwrap().unwrap().done,
            0,
            "a newer decline must revoke an earlier signed approval before upload"
        );
        f.b.decide_attachment(file, true).unwrap();
        f.return_controls().await;
        f.a.remove_channel_member(f.group, &f.b.user_id())
            .await
            .unwrap();
        f.transfer_tick().await;
        assert_eq!(
            f.b.file_progress(file).unwrap().unwrap().done,
            0,
            "an approval cannot outlive current membership in its exact group"
        );
        assert!(f.b.read_file(file).is_err());
    })
    .await
    .expect("revoked group transfers must remain bounded");
}

#[tokio::test]
async fn expired_group_file_cannot_export_chunks_after_prior_recipient_approval() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut f = GroupRelayFixture::new().await;
        let deadline = super::node::now_millis() + 5_000;
        let (manifest, file, _) = f.offer_expiring(Some(deadline)).await;
        f.b.decide_attachment(file, true).unwrap();
        f.return_controls().await;
        let approved =
            f.a.file_consent
                .lock()
                .unwrap()
                .received_control(manifest)
                .unwrap()
                .expect("the sender must first durably receive the exact approval");
        assert_eq!(
            super::file_consent::Decision::open(&approved)
                .unwrap()
                .decision,
            ReceiveDecision::Accepted
        );
        let last_chunk =
            f.a.log
                .lock()
                .unwrap()
                .events(&file)
                .unwrap()
                .into_iter()
                .last()
                .unwrap()
                .clone();
        let outer = crate::postoffice::opaque::RelayMailbox::federated(
            &f.a.identity,
            &f.b.identity.public(),
        )
        .unwrap()
        .wrap(&last_chunk)
        .unwrap()
        .id;
        assert!(!f.ps.lock().unwrap().has(&outer));
        assert!(!f.qs.lock().unwrap().has(&outer));
        // Expiry uses the persisted wall-clock deadline rather than Tokio's
        // monotonic timer, so wait for the genuine outgoing policy to expire.
        let remaining = deadline.saturating_sub(super::node::now_millis());
        tokio::time::sleep(Duration::from_millis(remaining + 20)).await;
        f.transfer_tick().await;
        assert!(
            !f.ps.lock().unwrap().has(&outer) && !f.qs.lock().unwrap().has(&outer),
            "an old accepted control must not export expired ciphertext to either relay"
        );
        assert!(f.b.log.lock().unwrap().events(&file).unwrap().is_empty());
        assert!(f.b.read_file(file).is_err());
    })
    .await
    .expect("expired group work must terminate within its bounded retry turn");
}

#[tokio::test]
async fn malformed_and_other_group_consent_cannot_authorize_group_file_chunks() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut f = GroupRelayFixture::new().await;
        let (manifest, file, _) = f.offer().await;
        let outsider = DeviceIdentity::generate();
        let outsider_account = Account::generate();
        let proof = Announce::new_with_account(
            &outsider,
            &outsider_account,
            "Member of another group",
            f.ap.tcp_port,
        );
        f.a.roster
            .lock()
            .unwrap()
            .update(&proof, f.qaddr.ip(), &f.a.user_id());
        f.a.create_channel("Unrelated group", vec![proof.public()])
            .await
            .unwrap();
        assert!(f.a.group_member(&proof.public()).unwrap());
        let decision = super::file_consent::Decision {
            file,
            manifest,
            sender: f.a.identity.public(),
            recipient: proof.public(),
            sender_account: Some(f.a.account_id()),
            recipient_account: outsider_account.account_id(),
            decision: ReceiveDecision::Accepted,
            completion: None,
        };
        let unrelated = decision.event(&outsider, 1).unwrap();
        assert!(
            f.a.accept_file_consent_event(&unrelated, &proof.public())
                .unwrap()
        );
        assert!(
            f.a.file_consent
                .lock()
                .unwrap()
                .received_control(manifest)
                .unwrap()
                .is_none()
        );
        let mut correct = decision;
        correct.recipient = f.b.identity.public();
        correct.recipient_account = f.b.account_id();
        let mut malformed = correct.event(&f.b.identity, 1).unwrap();
        *malformed.ciphertext.last_mut().unwrap() ^= 1;
        assert!(
            f.a.accept_file_consent_event(&malformed, &f.b.identity.public())
                .unwrap()
        );
        let mut wrong_file = correct;
        wrong_file.file = ConversationId::new([0x52; 32]);
        let wrong_scope = wrong_file.event(&f.b.identity, 2).unwrap();
        assert!(
            f.a.accept_file_consent_event(&wrong_scope, &f.b.identity.public())
                .unwrap()
        );
        assert!(
            f.a.file_consent
                .lock()
                .unwrap()
                .received_control(manifest)
                .unwrap()
                .is_none()
        );
        f.transfer_tick().await;
        assert_eq!(f.b.file_progress(file).unwrap().unwrap().done, 0);
        assert!(f.b.read_file(file).is_err());
    })
    .await
    .expect("invalid group controls must not hold the retry worker");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authorized_group_file_card_surfaces_without_observing_the_senders_announcement() {
    let f = tokio::time::timeout(Duration::from_secs(30), GroupRelayFixture::new())
        .await
        .expect("group relay setup must finish before the history-only reception window");
    tokio::time::timeout(Duration::from_secs(10), async {
        let source = f.directory.path().join("historical-document.bin");
        std::fs::write(&source, b"group history can outlive the original sender").unwrap();
        let (manifest, file) =
            f.a.enqueue_file_channel_progress_expiring_if(
                f.group,
                &source,
                crate::file::FileKind::File,
                |_| {},
                None,
                |commit| commit(),
            )
            .await
            .unwrap();
        let unseen_sender_directory = f.directory.path().join("history-only-recipient");
        std::fs::create_dir(&unseen_sender_directory).unwrap();
        let unrelated = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "An unrelated discovered device",
            f.ap.tcp_port,
        );
        let (recipient, _, mut offers) = node_with_files(
            &unseen_sender_directory,
            DeviceIdentity::from_secret_bytes(f.bkeys.0, f.bkeys.1),
            Account::from_secret_bytes(f.baccount),
            &unrelated,
        );
        assert!(recipient.historical_author(&f.ap.ed25519_pub).is_none());
        // Signed group authority and sender-key distributions authorize the
        // history card independently from any current discovery announcement.
        let history =
            f.a.log
                .lock()
                .unwrap()
                .events(&f.group)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>();
        for event in history {
            recipient.log.lock().unwrap().append_durable(event).unwrap();
        }
        settle_group_files(&recipient, f.group).await;
        let offer = offers
            .try_recv()
            .expect("missing discovery proof must not suppress an authorized group file card");
        assert_eq!(offer.event_id, manifest);
        assert_eq!(offer.file_conv, file);
        assert_eq!(offer.name, "historical-document.bin");
        assert!(offer.is_channel);
        assert!(recipient.historical_author(&f.ap.ed25519_pub).is_none());
        assert!(
            recipient
                .channel_history(f.group, 10)
                .unwrap()
                .into_iter()
                .any(|row| row.id == manifest && row.file.is_some()),
            "the history projection must retain the same exact signed file identity"
        );
        assert_eq!(recipient.file_progress(file).unwrap().unwrap().done, 0);
    })
    .await
    .expect("history-only group file reception must remain bounded");
}

#[tokio::test]
async fn invisible_group_permission_cannot_republish_revoked_private_file_consent() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let f = GroupRelayFixture::new().await;
        f.b.configure_privacy(
            &f.directory.path().join("b"),
            "pw",
            &f.bp,
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        f.b.initiate_contact_locally(&f.a.account_id())
            .await
            .unwrap();
        let source = f.directory.path().join("private-document.bin");
        std::fs::write(
            &source,
            b"this offer belongs only to the private conversation",
        )
        .unwrap();
        let (manifest, file) =
            f.a.enqueue_file_to_account(&f.b.account_id(), &source, crate::file::FileKind::File)
                .await
                .unwrap();
        let event =
            f.a.log
                .lock()
                .unwrap()
                .get(&manifest)
                .unwrap()
                .unwrap()
                .clone();
        let private_scope = event.conversation_id;
        f.b.log.lock().unwrap().append_durable(event).unwrap();
        f.b.process_file_events(private_scope);
        f.b.decide_attachment(file, true).unwrap();
        assert!(
            f.b.file_consent_events(&f.ap.public())
                .unwrap()
                .iter()
                .any(|event| {
                    super::file_consent::Decision::open(event)
                        .is_some_and(|decision| decision.manifest == manifest)
                }),
            "the authorized private offer must have a publishable control before revocation"
        );
        f.b.set_invisible(true).await.unwrap();
        f.b.set_allowed(&f.a.account_id(), false).await.unwrap();
        assert!(!f.b.known_account_allowed(&f.ap.public()));
        assert!(f.b.group_member(&f.ap.public()).unwrap());
        assert!(
            f.b.file_consent_events(&f.ap.public())
                .unwrap()
                .iter()
                .all(|event| {
                    super::file_consent::Decision::open(event)
                        .is_none_or(|decision| decision.manifest != manifest)
                }),
            "sharing a group must not republish a revoked private file's decision"
        );
        assert!(
            f.b.direct_file_consent_control(&f.ap.public(), file, manifest)
                .unwrap()
                .is_none(),
            "an exact direct query must enforce the same private permission boundary"
        );
    })
    .await
    .expect("private consent revocation must not wait for a direct route");
}

#[tokio::test]
async fn offline_group_file_is_deposited_before_consent_and_received_after_sender_stops() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut f = GroupRelayFixture::new().await;
        // The recipient has not polled its mailbox, observed the card, or
        // issued a receive decision while the sender uploads encrypted custody.
        let (manifest, file, payload) = f.enqueue_offer_expiring(None).await;
        assert!(f.b.file_progress(file).unwrap().is_none());
        assert!(f.b.log.lock().unwrap().events(&file).unwrap().is_empty());
        let last_chunk = f
            .a
            .log
            .lock()
            .unwrap()
            .events(&file).unwrap()
            .into_iter()
            .last()
            .unwrap()
            .clone();
        let outer = crate::postoffice::opaque::RelayMailbox::federated(
            &f.a.identity,
            &f.b.identity.public(),
        )
        .unwrap()
        .wrap(&last_chunk)
        .unwrap()
        .id;
        for _ in 0..8 {
            f.a.pull_pending_files().await;
            if f.ps.lock().unwrap().has(&outer) {
                break;
            }
        }
        assert!(
            f.ps.lock().unwrap().has(&outer),
            "the exact encrypted final chunk must reach the sender's post office before the recipient is online or approves"
        );
        assert!(
            f.a.file_consent
                .lock()
                .unwrap()
                .received_control(manifest).unwrap()
                .is_none(),
            "offline ciphertext custody cannot depend on a recipient control"
        );
        assert!(!f.ps.lock().unwrap().has(&last_chunk.id));
        // There is no sender accept loop in this fixture. Drop its only Node
        // owner as well, so subsequent progress can only come from the relays.
        let stopped_sender = std::mem::replace(&mut f.a, f.b.clone());
        drop(stopped_sender);
        f.federate().await;
        assert!(f.qs.lock().unwrap().has(&outer));
        f.b.drain_from_post_office().await;
        settle_group_files(&f.b, f.group).await;
        let offer = f
            .offers
            .try_recv()
            .expect("the offline recipient must recover the group file card after the sender stops");
        assert_eq!(offer.event_id, manifest);
        assert_eq!(offer.file_conv, file);
        assert_eq!(
            f.b.file_progress(file).unwrap().unwrap().done,
            0,
            "encrypted post-office custody must not bypass the recipient's local receive decision"
        );
        assert!(f.b.read_file(file).is_err());
        f.b.decide_attachment(file, true).unwrap();
        for _ in 0..8 {
            f.b.drain_from_post_office().await;
            settle_group_files(&f.b, f.group).await;
            if f.b.file_ready_to_save(file).unwrap() {
                break;
            }
        }
        assert_eq!(
            f.b.read_file(file)
                .expect("a later approval must recover retained ciphertext without waking the sender"),
            payload
        );
        let saved = f.directory.path().join("received-after-sender-stopped.bin");
        f.b.save_file(file, &saved).unwrap();
        assert_eq!(std::fs::read(saved).unwrap(), payload);
    })
    .await
    .expect("offline group file custody and later receipt must make bounded progress");
}
