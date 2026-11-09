//! Exact outgoing entitlement must not rebuild the channel history per check.
use super::*;
use crate::{eventlog::Author, identity::device::DeviceIdentity};
use std::sync::Arc;

fn node(path: &std::path::Path) -> Arc<Node> {
    std::fs::create_dir_all(path).unwrap();
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (group, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        dm,
        group,
        files,
        &path.join("events.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap()
}

fn append_history(node: &Node, channel: ConversationId, count: usize) -> EventId {
    let author = Author::from_ed25519(node.identity.public().ed25519_pub);
    let mut log = node.log.lock().unwrap();
    let mut first = None;
    for _ in 0..count {
        let (parents, lamport) = log.prepare(&channel).unwrap();
        let sequence = log.next_sequence(&channel, &author).unwrap();
        let event = Event::new(
            &node.identity,
            channel,
            sequence,
            parents,
            lamport,
            1,
            EventKind::Message,
            vec![7; 8192],
        );
        first.get_or_insert(event.id);
        log.append(event).unwrap();
    }
    first.unwrap()
}

fn settle_contains(
    node: &Node,
    channel: ConversationId,
    event: EventId,
    peer: &PublicIdentity,
) -> bool {
    for _ in 0..4096 {
        match node.group_file_outgoing_contains(channel, event, peer) {
            Ok(allowed) => return allowed,
            Err(LogError::StaleCursor) => {}
            Err(error) => panic!("outgoing entitlement preparation failed: {error}"),
        }
    }
    panic!("exact outgoing entitlement did not finish bounded preparation")
}

#[tokio::test]
async fn group_file_exact_outgoing_warm_checks_do_not_hydrate_32_or_128_messages() {
    for count in [32, 128] {
        let root = tempfile::tempdir().unwrap();
        let node = node(root.path());
        let peer = DeviceIdentity::generate();
        let channel = node
            .create_channel("File custody", vec![peer.public()])
            .await
            .unwrap();
        let event = append_history(&node, channel, count);
        crate::node::test_support::settle_group_metadata(&node, channel);
        assert!(settle_contains(&node, channel, event, &peer.public()));
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        for _ in 0..3 {
            assert!(
                node.group_file_outgoing_contains(channel, event, &peer.public())
                    .unwrap()
            );
        }
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "warm exact outgoing checks must not re-read {count} Message bodies"
        );
    }
}

#[tokio::test]
async fn group_file_exact_outgoing_revalidates_unapplied_leave() {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peer = DeviceIdentity::generate();
    let channel = node
        .create_channel("File custody", vec![peer.public()])
        .await
        .unwrap();
    let event = append_history(&node, channel, 128);
    crate::node::test_support::settle_group_metadata(&node, channel);
    assert!(settle_contains(&node, channel, event, &peer.public()));
    let leave = {
        let log = node.log.lock().unwrap();
        let (parents, lamport) = log.prepare(&channel).unwrap();
        Event::new(
            &peer,
            channel,
            1,
            parents,
            lamport,
            1,
            EventKind::ChannelLeave,
            vec![],
        )
    };
    node.log.lock().unwrap().append(leave).unwrap();
    crate::node::test_support::settle_group_metadata(&node, channel);
    let oracle = node
        .sync_store(&peer.public())
        .lock()
        .unwrap()
        .authorized_event_ids(&channel)
        .unwrap()
        .contains(&event);
    assert_eq!(
        settle_contains(&node, channel, event, &peer.public()),
        oracle,
        "terminal closing ancestry keeps the existing historical entitlement"
    );
    let after_leave = append_history(&node, channel, 1);
    assert!(
        !matches!(
            node.group_file_outgoing_contains(channel, after_leave, &peer.public()),
            Ok(true)
        ),
        "a new event outside the terminal ancestry must not inherit an old positive grant"
    );
    crate::node::test_support::settle_group_metadata(&node, channel);
    assert!(!settle_contains(
        &node,
        channel,
        after_leave,
        &peer.public()
    ));
}

#[tokio::test]
async fn group_file_exact_outgoing_retains_at_most_four_prepared_recipient_views() {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peers: Vec<_> = (0..5).map(|_| DeviceIdentity::generate()).collect();
    let channel = node
        .create_channel(
            "File custody",
            peers.iter().map(DeviceIdentity::public).collect(),
        )
        .await
        .unwrap();
    let event = append_history(&node, channel, 32);
    crate::node::test_support::settle_group_metadata(&node, channel);
    for peer in &peers {
        assert!(settle_contains(&node, channel, event, &peer.public()));
        assert!(node.group_file_delivery.authorization.lock().unwrap().len() <= 4);
    }
    assert_eq!(
        node.group_file_delivery.authorization.lock().unwrap().len(),
        4
    );
    assert!(
        settle_contains(&node, channel, event, &peers[0].public()),
        "an evicted scope can prepare again without changing entitlement"
    );
    assert_eq!(
        node.group_file_delivery.authorization.lock().unwrap().len(),
        4
    );
}

#[tokio::test]
async fn group_file_five_cold_interleaved_scopes_finish_without_discarding_pending_work() {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peers: Vec<_> = (0..5).map(|_| DeviceIdentity::generate()).collect();
    let channel = node
        .create_channel(
            "Interleaved custody",
            peers.iter().map(DeviceIdentity::public).collect(),
        )
        .await
        .unwrap();
    let event = append_history(&node, channel, 128);
    crate::node::test_support::settle_group_metadata(&node, channel);
    let mut completed = [false; 5];
    for _ in 0..512 {
        for (index, peer) in peers.iter().enumerate() {
            // Finished recipients model queued jobs in their held cooldown;
            // other scopes get one bounded turn before rotation.
            if completed[index] {
                continue;
            }
            match node.group_file_outgoing_contains(channel, event, &peer.public()) {
                Ok(true) => completed[index] = true,
                Err(LogError::StaleCursor) => {}
                Ok(false) => panic!("an established recipient lost exact source entitlement"),
                Err(error) => panic!("interleaved custody preparation failed: {error}"),
            }
            assert!(node.group_file_delivery.authorization.lock().unwrap().len() <= 4);
        }
        if completed.iter().all(|done| *done) {
            break;
        }
    }
    assert!(
        completed.iter().all(|done| *done),
        "every cold scope must progress under the four-view limit: {completed:?}"
    );
}

#[tokio::test]
async fn group_file_periodic_retry_finishes_orphan_pending_views_and_becomes_idle() {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peers: Vec<_> = (0..4).map(|_| DeviceIdentity::generate()).collect();
    let channel = node
        .create_channel(
            "Orphan custody",
            peers.iter().map(DeviceIdentity::public).collect(),
        )
        .await
        .unwrap();
    let event = append_history(&node, channel, 128);
    crate::node::test_support::settle_group_metadata(&node, channel);
    for peer in &peers {
        assert!(matches!(
            node.group_file_outgoing_contains(channel, event, &peer.public()),
            Err(LogError::StaleCursor)
        ));
    }
    assert!(node.group_file_delivery.jobs.lock().unwrap().is_empty());
    assert_eq!(
        node.group_file_delivery.authorization.lock().unwrap().len(),
        4
    );
    for _ in 0..512 {
        // No manifest/job can revisit these views. The existing periodic
        // owner must discharge their admitted finite preparation itself.
        node.retry_group_file_delivery().await;
        if node
            .group_file_delivery
            .authorization
            .lock()
            .unwrap()
            .values()
            .all(|view| view.ready)
        {
            break;
        }
    }
    assert!(
        node.group_file_delivery
            .authorization
            .lock()
            .unwrap()
            .values()
            .all(|view| view.ready),
        "orphan pending preparation must not be stranded outside the job queue"
    );
    assert!(
        !node.group_file_work_pending(),
        "completed orphan preparation must permit idle cadence"
    );
}

#[tokio::test]
async fn group_file_periodic_retry_retires_invalid_orphans_before_new_scope_admission() {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peers: Vec<_> = (0..5).map(|_| DeviceIdentity::generate()).collect();
    let old = node
        .create_channel(
            "Removed custody",
            peers[..4].iter().map(DeviceIdentity::public).collect(),
        )
        .await
        .unwrap();
    let fresh = node
        .create_channel("Current custody", vec![peers[4].public()])
        .await
        .unwrap();
    let old_event = append_history(&node, old, 128);
    let fresh_event = append_history(&node, fresh, 128);
    crate::node::test_support::settle_group_metadata(&node, old);
    crate::node::test_support::settle_group_metadata(&node, fresh);
    for peer in &peers[..4] {
        assert!(matches!(
            node.group_file_outgoing_contains(old, old_event, &peer.public()),
            Err(LogError::StaleCursor)
        ));
    }
    node.log.lock().unwrap().drop_conversation(&old).unwrap();
    for _ in 0..16 {
        node.retry_group_file_delivery().await;
    }
    assert!(
        !node
            .group_file_delivery
            .authorization
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.0 == *old.as_bytes()),
        "invalid source views must be released even when their jobs never existed"
    );
    assert!(settle_contains(
        &node,
        fresh,
        fresh_event,
        &peers[4].public()
    ));
    assert!(!node.group_file_work_pending());
}

struct QueuedFixture {
    _root: tempfile::TempDir,
    node: Arc<Node>,
    channel: ConversationId,
    manifest: EventId,
    file: ConversationId,
    peers: Vec<DeviceIdentity>,
}

async fn queued_fixture() -> QueuedFixture {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peers: Vec<_> = (0..5).map(|_| DeviceIdentity::generate()).collect();
    let channel = node
        .create_channel(
            "Queued custody",
            peers.iter().map(DeviceIdentity::public).collect(),
        )
        .await
        .unwrap();
    append_history(&node, channel, 128);
    // These are legacy accepted members. Authentication supplies each original
    // device proof without changing the group's existing admission contract.
    for peer in &peers {
        let account = crate::identity::account::Account::generate();
        let proof = Announce::new_with_account(peer, &account, "Custody recipient", 47479);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
    }
    node.cached_peer_snapshot_async().await.unwrap();
    crate::node::test_support::settle_group_metadata(&node, channel);
    let source = root.path().join("queued.bin");
    std::fs::write(&source, [7; 128]).unwrap();
    let (manifest, file) = node
        .enqueue_file_channel_progress_expiring_if(
            channel,
            &source,
            crate::file::FileKind::File,
            |_| {},
            None,
            |commit| commit(),
        )
        .await
        .unwrap();
    crate::node::test_support::settle_group_metadata(&node, channel);
    let mut previous = 0;
    for _ in 0..1024 {
        match node.queue_group_file_delivery(file, manifest) {
            Ok(()) | Err(LogError::StaleCursor) => {}
            Err(error) => panic!("group queue admission failed: {error}"),
        }
        let count = node.group_file_delivery.jobs.lock().unwrap().len();
        assert!(
            count >= previous,
            "prepared recipient jobs must not disappear during refill"
        );
        previous = count;
        if count == 5 {
            break;
        }
    }
    assert_eq!(
        node.group_file_delivery.jobs.lock().unwrap().len(),
        5,
        "refill must reach the fifth recipient after the first four were queued"
    );
    QueuedFixture {
        _root: root,
        node,
        channel,
        manifest,
        file,
        peers,
    }
}

#[tokio::test]
async fn group_file_publisher_leaves_incoming_mailbox_to_dedicated_drain() {
    use super::super::opaque_relay::incremental::RECEIVE_ENTRY_CALLS;
    use crate::eventlog::sync::network::NetworkSyncStore;
    use crate::postoffice::{IndexedPostOffice, federation::IndexedRelayEndpoint};
    use crate::storage::profile::ProfileStorageLease;
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    node.refill_group_file_delivery().unwrap();
    let (key, job) = {
        let mut jobs = node.group_file_delivery.jobs.lock().unwrap();
        for job in jobs.values_mut() {
            job.retry_after =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        }
        let (key, job) = jobs.iter_mut().next().unwrap();
        job.retry_after = None;
        (*key, job.clone())
    };
    assert!(settle_job(node, &job));
    let recipient = fixture
        .peers
        .iter()
        .find(|peer| peer.public() == job.recipient)
        .unwrap();
    let proof = node.historical_author(&job.recipient.ed25519_pub).unwrap();
    let relay = DeviceIdentity::generate();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let announce = Announce::new_post_office_with_account(
        &relay,
        &crate::identity::account::Account::generate(),
        "Publish relay",
        listener.local_addr().unwrap().port(),
    );
    assert!(announce.valid_presence());
    node.roster
        .lock()
        .unwrap()
        .update(&announce, "127.0.0.1".parse().unwrap(), &node.user_id());
    let relay_root = tempfile::tempdir().unwrap();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_root.path()).unwrap(),
        &relay_root.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let relay_public = relay.public();
    let mailbox = RelayMailbox::new(&node.identity, &job.recipient, &relay_public).unwrap();
    let historical = node.log.lock().unwrap().events(&fixture.channel).unwrap();
    let mut seeded = 0;
    let mut pending_outer = None;
    for event in historical
        .iter()
        .filter(|event| event.kind == EventKind::Message)
        .take(8)
    {
        let outer = mailbox.wrap(event).unwrap();
        office.ingest(outer.clone()).unwrap();
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .remember(mailbox.id(), &[(outer.id, event.id)]);
        pending_outer.get_or_insert(outer.id);
        seeded += 1;
    }
    assert_eq!(seeded, 8);
    let (old_receipt_key, old_receipt_bytes) = node.seed_duplex_receipt_pending_for_test(
        &relay.public(),
        &job.recipient,
        fixture.file,
        pending_outer.unwrap(),
    );
    let chunks = node
        .files
        .lock()
        .unwrap()
        .retention_for(fixture.file)
        .unwrap()
        .unwrap()
        .chunks;
    let final_chunk = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .authored_header(
            &fixture.file,
            Author::from_ed25519(node.identity.public().ed25519_pub),
            u64::from(chunks),
        )
        .unwrap()
        .unwrap()
        .id;
    let final_event = node.log.lock().unwrap().get(&final_chunk).unwrap().unwrap();
    let final_outer = mailbox.wrap(&final_event).unwrap().id;
    let relay_store = Arc::new(Mutex::new(office.clone()));
    let served_store = relay_store.clone();
    let replacement_root = tempfile::tempdir().unwrap();
    let (reset_server, mut reset_requests) =
        tokio::sync::mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<()>>();
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let endpoint = IndexedRelayEndpoint::new(served_store.lock().unwrap().clone(), None, None);
                    let relay = DeviceIdentity::from_secret_bytes(relay.secret_bytes().0, relay.secret_bytes().1);
                    let announce = announce.clone();
                    connections.spawn(async move {
                        let channel = crate::transport::SecureChannel::accept_with_presence(
                            stream, &relay, Some(&announce), |_| true,
                        ).await.unwrap();
                        endpoint.serve_connection_checked(channel, None).await.unwrap();
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    result.unwrap().unwrap();
                }
                reset = reset_requests.recv() => {
                    let completed = reset.expect("reset owner closed before server retirement");
                    connections.shutdown().await;
                    let _ = completed.send(());
                }
            }
        }
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        RECEIVE_ENTRY_CALLS.with(|calls| calls.set(0));
        node.retry_group_file_delivery().await;
        let publisher_reads = RECEIVE_ENTRY_CALLS.with(std::cell::Cell::get);
        let held_before_approval = office.durable_have(&mailbox.id(), &final_outer).unwrap();
        let preserved_receipt = node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&old_receipt_key)
            .unwrap()
            == Some(old_receipt_bytes.clone());
        // Replace the real relay store under the same authenticated endpoint.
        // Closing its existing sockets ensures the next exchange reaches that
        // replacement instead of an old pooled connection's retained store.
        office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(replacement_root.path()).unwrap(),
            &replacement_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        *relay_store.lock().unwrap() = office.clone();
        let (reset_done, reset_finished) = tokio::sync::oneshot::channel();
        reset_server.send(reset_done).unwrap();
        reset_finished.await.unwrap();
        assert!(!office.durable_have(&mailbox.id(), &final_outer).unwrap());
        RECEIVE_ENTRY_CALLS.with(|calls| calls.set(0));
        for _ in 0..2 {
            node.group_file_delivery
                .jobs
                .lock()
                .unwrap()
                .get_mut(&key)
                .unwrap()
                .retry_after = None;
            node.retry_group_file_delivery().await;
            if office.durable_have(&mailbox.id(), &final_outer).unwrap() {
                break;
            }
        }
        let replacement_reads = RECEIVE_ENTRY_CALLS.with(std::cell::Cell::get);
        let restored_custody = office.durable_have(&mailbox.id(), &final_outer).unwrap();
        let preserved_after_reset = node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&old_receipt_key)
            .unwrap()
            == Some(old_receipt_bytes.clone());
        // Exercise the generic immediate group publisher through its owning
        // consumer too, with unrelated incoming work in the same peer mailbox.
        let mut generic_pending = None;
        for event in historical
            .iter()
            .filter(|event| event.kind == EventKind::Message)
            .take(8)
        {
            let outer = mailbox.wrap(event).unwrap();
            office.ingest(outer.clone()).unwrap();
            generic_pending.get_or_insert(outer.id);
        }
        let (generic_receipt_key, generic_receipt_bytes) = node
            .seed_duplex_receipt_pending_for_test(
                &relay_public,
                &job.recipient,
                fixture.channel,
                generic_pending.unwrap(),
            );
        let manifest_event = node
            .log
            .lock()
            .unwrap()
            .get(&fixture.manifest)
            .unwrap()
            .unwrap();
        let manifest_outer = mailbox.wrap(&manifest_event).unwrap().id;
        assert!(!office.durable_have(&mailbox.id(), &manifest_outer).unwrap());
        RECEIVE_ENTRY_CALLS.with(|calls| calls.set(0));
        let generic_confirmed = node
            .replicate_to_post_office_targets_confirming(
                fixture.channel,
                vec![job.recipient.clone()],
                Some(fixture.manifest),
            )
            .await
            .unwrap();
        let generic_reads = RECEIVE_ENTRY_CALLS.with(std::cell::Cell::get);
        let generic_held =
            generic_confirmed && office.durable_have(&mailbox.id(), &manifest_outer).unwrap();
        let generic_preserved = node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&generic_receipt_key)
            .unwrap()
            == Some(generic_receipt_bytes);
        let declined = Decision {
            file: fixture.file,
            manifest: fixture.manifest,
            sender: node.identity.public(),
            recipient: job.recipient.clone(),
            sender_account: Some(node.account_id()),
            recipient_account: proof.account_id().unwrap(),
            decision: ReceiveDecision::Declined,
            completion: None,
        }
        .event(recipient, 1)
        .unwrap();
        office.ingest(mailbox.wrap(&declined).unwrap()).unwrap();
        RECEIVE_ENTRY_CALLS.with(|calls| calls.set(0));
        node.drain_from_post_office().await;
        let drain_reads = RECEIVE_ENTRY_CALLS.with(std::cell::Cell::get);
        let refused = !settle_job(node, &job);
        if let Some(job) = node.group_file_delivery.jobs.lock().unwrap().get_mut(&key) {
            job.retry_after = None;
        }
        node.retry_group_file_delivery().await;
        let retired = !node
            .group_file_delivery
            .jobs
            .lock()
            .unwrap()
            .contains_key(&key);
        (
            publisher_reads,
            held_before_approval,
            drain_reads,
            refused,
            retired,
            preserved_receipt,
            replacement_reads,
            restored_custody,
            preserved_after_reset,
            generic_reads,
            generic_held,
            generic_preserved,
        )
    })
    .await;
    server.abort();
    let _ = server.await;
    let (
        publisher_reads,
        held,
        drain_reads,
        refused,
        retired,
        preserved_receipt,
        replacement_reads,
        restored_custody,
        preserved_after_reset,
        generic_reads,
        generic_held,
        generic_preserved,
    ) = result.unwrap();
    assert!(
        held,
        "publication must obtain real final-chunk custody before approval"
    );
    assert!(
        drain_reads > 0,
        "the dedicated drain must still own real incoming work"
    );
    assert!(
        refused && retired,
        "a refusal imported by the drain must revoke and retire the outgoing job"
    );
    assert_eq!(
        publisher_reads, 0,
        "outgoing group-file custody scanned {publisher_reads} unrelated incoming mailbox IDs"
    );
    assert!(
        preserved_receipt,
        "publishing must preserve the old duplex Some(file) receipt byte for byte, including pending work and all receive bindings"
    );
    assert_eq!(
        replacement_reads, 0,
        "remote store replacement must not turn publication into incoming work"
    );
    assert!(
        restored_custody,
        "the replacement relay must actually regain the final chunk; an old completion proof is insufficient"
    );
    assert!(
        preserved_after_reset,
        "remote replacement must preserve the original duplex receipt byte for byte"
    );
    assert!(
        generic_held,
        "generic publication must establish actual requested-manifest custody"
    );
    assert_eq!(
        generic_reads, 0,
        "generic immediate publication scanned {generic_reads} unrelated incoming mailbox IDs"
    );
    assert!(
        generic_preserved,
        "generic publication must preserve its old duplex Some(group) receipt byte for byte"
    );
}

#[tokio::test]
async fn group_file_queue_restarts_skip_queued_prefix_and_admit_all_five_recipients() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let file = fixture.file;
    let manifest = fixture.manifest;
    let before = node.received_files.lock().unwrap().body_reads();
    node.queue_group_file_delivery(file, manifest).unwrap();
    assert_eq!(
        node.received_files.lock().unwrap().body_reads(),
        before,
        "queued keys are revalidated by the retry owner, not rehydrated by every refill"
    );
}

fn settle_job(node: &Node, job: &GroupFileUpload) -> bool {
    for _ in 0..4096 {
        match node.group_file_custody_allowed(job) {
            Ok(allowed) => return allowed,
            Err(LogError::StaleCursor) => {}
            Err(error) => panic!("queued custody preparation failed: {error}"),
        }
    }
    panic!("queued custody did not finish finite preparation")
}

#[tokio::test]
async fn group_file_five_queued_warm_jobs_compare_versions_without_sql_changes() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let jobs: Vec<_> = node
        .group_file_delivery
        .jobs
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    // Establish all five exact authorizations. A four-view LRU alone cannot
    // retain them, so the following cycles exercise real eviction pressure.
    for job in &jobs {
        assert!(settle_job(node, job));
    }
    let before = node
        .storage
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    for _ in 0..2 {
        // Real retry selection clones jobs afresh, rather than borrowing the
        // original queue entry or depending on one call's stack-local result.
        let selected: Vec<_> = node
            .group_file_delivery
            .jobs
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for job in &selected {
            assert!(settle_job(node, job));
        }
    }
    assert_eq!(
        node.storage
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap(),
        before,
        "five unchanged queued scopes must compare qualified versions without rebuilding SQL selections"
    );
}

#[tokio::test]
async fn group_file_queued_qualification_rechecks_appends_and_signed_departure() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let jobs: Vec<_> = node
        .group_file_delivery
        .jobs
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    for job in &jobs {
        assert!(settle_job(node, job));
    }
    let job = {
        let views = node.group_file_delivery.authorization.lock().unwrap();
        jobs.iter()
            .find(|job| {
                !views.contains_key(&(
                    *fixture.channel.as_bytes(),
                    job.recipient.ed25519_pub,
                    job.recipient.x25519_pub,
                ))
            })
            .unwrap()
            .clone()
    };
    let before_stamp = node
        .sync_store(&job.recipient)
        .lock()
        .unwrap()
        .history_sync_stamp(&fixture.channel)
        .unwrap();
    assert!(before_stamp.is_some());
    append_history(node, fixture.channel, 1);
    crate::node::test_support::settle_group_metadata(node, fixture.channel);
    let after_stamp = node
        .sync_store(&job.recipient)
        .lock()
        .unwrap()
        .history_sync_stamp(&fixture.channel)
        .unwrap();
    assert_ne!(
        before_stamp, after_stamp,
        "ordinary source append changes the qualified observation"
    );
    let before = node
        .storage
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    assert!(
        settle_job(node, &job),
        "the retained manifest remains authorized after a new message"
    );
    assert!(
        node.storage
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap()
            > before,
        "an evicted scope must requalify through its exact prepared owner after a source version change"
    );
    let warm = node
        .storage
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    assert!(settle_job(node, &job));
    assert_eq!(
        node.storage
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap(),
        warm
    );
    let peer = fixture
        .peers
        .iter()
        .find(|peer| peer.public() == job.recipient)
        .unwrap();
    let leave = {
        let log = node.log.lock().unwrap();
        let (parents, lamport) = log.prepare(&fixture.channel).unwrap();
        Event::new(
            peer,
            fixture.channel,
            1,
            parents,
            lamport,
            1,
            EventKind::ChannelLeave,
            vec![],
        )
    };
    node.log.lock().unwrap().append(leave).unwrap();
    assert!(
        !matches!(node.group_file_custody_allowed(&job), Ok(true)),
        "a queued positive qualification must not conceal an unapplied signed departure"
    );
    crate::node::test_support::settle_group_metadata(node, fixture.channel);
    assert!(
        !settle_job(node, &job),
        "fresh membership still governs every use of a qualified job"
    );
}

#[tokio::test]
async fn group_file_refused_and_completed_cold_recipients_skip_outgoing_preparation() {
    for completed in [false, true] {
        let fixture = queued_fixture().await;
        let node = &fixture.node;
        let completion = if completed {
            let chunks = node
                .files
                .lock()
                .unwrap()
                .retention_for(fixture.file)
                .unwrap()
                .unwrap()
                .chunks;
            Some(
                node.log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .authored_header(
                        &fixture.file,
                        Author::from_ed25519(node.identity.public().ed25519_pub),
                        u64::from(chunks),
                    )
                    .unwrap()
                    .expect("verified source has its exact final chunk")
                    .id,
            )
        } else {
            None
        };
        for peer in &fixture.peers {
            let proof = node.historical_author(&peer.public().ed25519_pub).unwrap();
            let decision = Decision {
                file: fixture.file,
                manifest: fixture.manifest,
                sender: node.identity.public(),
                recipient: peer.public(),
                sender_account: Some(node.account_id()),
                recipient_account: proof.account_id().unwrap(),
                decision: if completed {
                    ReceiveDecision::Accepted
                } else {
                    ReceiveDecision::Declined
                },
                completion,
            };
            let event = decision.event(peer, 1).unwrap();
            assert!(
                Decision::open(&event).is_some_and(|opened| opened == decision),
                "fixture must first verify the original recipient's signed terminal control"
            );
            assert!(
                node.accept_file_consent_event(&event, &peer.public())
                    .unwrap()
            );
            let retained = node
                .file_consent
                .lock()
                .unwrap()
                .received_control_for(fixture.manifest, &peer.public())
                .unwrap()
                .expect("the authenticated exact control must be durably retained");
            assert_eq!(retained.id, event.id);
            assert!(Decision::open(&retained).is_some_and(|opened| opened == decision));
        }
        // Simulate reconstruction after process-local jobs and selections were
        // discarded. Signed terminal controls remain in their durable owner.
        crate::node::test_support::settle_group_metadata(node, fixture.channel);
        for peer in &fixture.peers {
            assert!(
                node.group_file_offer(fixture.file, fixture.manifest, &peer.public())
                    .unwrap()
                    .is_some(),
                "fixture must retain a currently authorized signed offer"
            );
        }
        let progress = node.file_progress(fixture.file).unwrap().unwrap();
        assert_eq!(progress.done, progress.total);
        node.group_file_delivery.jobs.lock().unwrap().clear();
        node.group_file_delivery
            .authorization
            .lock()
            .unwrap()
            .clear();
        let before = node
            .storage
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap();
        for _ in 0..2 {
            node.queue_group_file_delivery(fixture.file, fixture.manifest)
                .expect(
                    "definitive signed terminal decisions must bypass cold entitlement preparation",
                );
            assert!(node.group_file_delivery.jobs.lock().unwrap().is_empty());
            assert!(
                node.group_file_delivery
                    .authorization
                    .lock()
                    .unwrap()
                    .is_empty(),
                "terminal recipients must not recreate source-history selections"
            );
        }
        assert_eq!(
            node.storage
                .database()
                .read(|c| Ok(c.total_changes()))
                .unwrap(),
            before,
            "five terminal recipients must remain read-only after restart (completed={completed})"
        );
    }
}

#[tokio::test]
async fn group_file_no_relay_retries_cool_all_five_jobs_without_repeated_reads() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    for _ in 0..3 {
        node.retry_group_file_delivery().await;
    }
    assert!(
        node.group_file_delivery
            .jobs
            .lock()
            .unwrap()
            .values()
            .all(|job| job.retry_after.is_some()),
        "every recipient must get a fair attempt and a deadline even without an eligible relay"
    );
    for job in node.group_file_delivery.jobs.lock().unwrap().values_mut() {
        job.retry_after = Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
    }
    assert!(
        !node.group_file_work_pending(),
        "cooled jobs must permit idle cadence"
    );
    let before = node.received_files.lock().unwrap().body_reads();
    let sql = node
        .storage
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    node.retry_group_file_delivery().await;
    assert_eq!(
        node.received_files.lock().unwrap().body_reads(),
        before,
        "cooled retries and stable EOF must not hydrate retained offers"
    );
    assert_eq!(
        node.storage
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap(),
        sql
    );
    node.group_file_delivery
        .jobs
        .lock()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()
        .retry_after = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    assert!(
        node.group_file_work_pending(),
        "expired work must become eligible again"
    );
    node.retry_group_file_delivery().await;
    assert!(!node.group_file_work_pending());
}

#[tokio::test]
async fn group_file_stable_refill_eof_skips_unchanged_rows() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    node.refill_group_file_delivery().unwrap();
    let before = node.received_files.lock().unwrap().body_reads();
    node.refill_group_file_delivery().unwrap();
    assert_eq!(
        node.received_files.lock().unwrap().body_reads(),
        before,
        "unchanged EOF must sleep until its repair deadline or source revision changes"
    );
    node.group_file_delivery.scan.lock().unwrap().repair_after =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    node.refill_group_file_delivery().unwrap();
    assert!(
        node.received_files.lock().unwrap().body_reads() > before,
        "periodic repair revisits eligibility without FILES appends"
    );
    let before = node.received_files.lock().unwrap().body_reads();
    node.received_files
        .lock()
        .unwrap()
        .record_durable(&unrelated_file_row(217))
        .unwrap();
    node.refill_group_file_delivery().unwrap();
    assert!(
        node.received_files.lock().unwrap().body_reads() > before,
        "FILES append immediately reopens discovery"
    );
}

#[tokio::test]
async fn group_file_full_queue_retains_scan_backlog_and_independent_retry_progress() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let original_keys: Vec<_> = node
        .group_file_delivery
        .jobs
        .lock()
        .unwrap()
        .keys()
        .copied()
        .collect();
    {
        let mut jobs = node.group_file_delivery.jobs.lock().unwrap();
        let template = jobs.values().next().unwrap().clone();
        for index in 0..MAX_GROUP_FILE_JOBS - original_keys.len() {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
            jobs.insert(
                (EventId::new(bytes), template.recipient.ed25519_pub),
                template.clone(),
            );
        }
        assert_eq!(jobs.len(), MAX_GROUP_FILE_JOBS);
    }
    node.retry_group_file_delivery().await;
    assert!(
        node.group_file_delivery
            .scan
            .lock()
            .unwrap()
            .repair_after
            .is_none(),
        "a full queue cannot acknowledge the unvisited discovery page"
    );
    assert!(
        node.group_file_delivery
            .jobs
            .lock()
            .unwrap()
            .values()
            .any(|job| job.retry_after.is_some()),
        "full discovery capacity must not prevent existing work advancing"
    );
    node.group_file_delivery
        .jobs
        .lock()
        .unwrap()
        .retain(|key, _| original_keys.contains(key));
    node.refill_group_file_delivery().unwrap();
    assert!(
        node.group_file_delivery
            .scan
            .lock()
            .unwrap()
            .repair_after
            .is_some(),
        "discovery resumes after capacity returns"
    );
}

fn unrelated_file_row(index: u64) -> super::super::received_log::ReceivedEntry {
    let mut id = [217; 32];
    id[..8].copy_from_slice(&index.to_be_bytes());
    super::super::received_log::ReceivedEntry {
        event_id: EventId::new(id),
        conversation: ConversationId::new([216; 32]),
        from: "unrelated".into(),
        wall_clock: 1,
        plaintext: vec![1],
    }
}

#[tokio::test]
async fn group_file_refill_failure_preserves_scan_and_attempts_independent_jobs() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let row = unrelated_file_row(218);
    node.received_files
        .lock()
        .unwrap()
        .record_durable(&row)
        .unwrap();
    node.storage.database().transaction(|c| {
        c.execute("DELETE FROM transcript_bodies WHERE ordinal IN (SELECT ordinal FROM transcript_rows WHERE kind=3 AND row_key=?1)", [row.event_id.as_bytes().as_slice()])?;
        Ok(())
    }).unwrap();
    assert!(node.refill_group_file_delivery().is_err());
    node.retry_group_file_delivery().await;
    assert!(
        node.group_file_delivery
            .jobs
            .lock()
            .unwrap()
            .values()
            .any(|job| job.retry_after.is_some()),
        "scan corruption cannot starve independently retained work"
    );
    assert!(
        node.group_file_delivery
            .scan
            .lock()
            .unwrap()
            .repair_after
            .is_none(),
        "failed scan must not certify EOF"
    );
}

#[tokio::test]
async fn group_file_cancelled_exchange_retains_launch_deadline_and_fair_cursor() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    // Publish real readiness after registration but before the current probe.
    // The active exchange must reserve its own deadline after consuming it.
    let weak = Arc::downgrade(node);
    let channel = fixture.channel;
    let publications = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = publications.clone();
    *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |peer, event| {
        if event == "group_file_dependency_registered"
            && let Some(node) = weak.upgrade()
        {
            node.wake_group_file_dependency(channel, Some((peer.ed25519_pub, peer.x25519_pub)));
            observed.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = DeviceIdentity::generate();
    let proof = Announce::new_post_office(
        &relay,
        "Stalled relay",
        listener.local_addr().unwrap().port(),
    );
    node.roster
        .lock()
        .unwrap()
        .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
    let retry_node = Arc::clone(node);
    let task = tokio::spawn(async move {
        retry_node.retry_group_file_delivery().await;
    });
    let (_socket, _) = tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(publications.load(Ordering::SeqCst) > 0);
    let key = node.group_file_delivery.cursor.lock().unwrap().unwrap();
    let jobs = node.group_file_delivery.jobs.lock().unwrap();
    assert!(
        jobs.get(&key).unwrap().retry_after.is_some(),
        "cancelling real pending handshake retains launch backoff"
    );
    assert!(
        jobs.iter()
            .any(|(other, job)| *other != key && job.retry_after.is_none()),
        "later independent jobs remain ready"
    );
}

#[tokio::test]
async fn group_file_unfinished_discovery_keeps_short_cadence_until_eof() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    // Match the producer's maintenance phase so unrelated legitimate work does
    // not invalidate the idle scheduling oracle below.
    for _ in 0..32 {
        node.retry_file_receives();
        node.maintain_media().unwrap();
        node.pull_pending_files().await;
        let downloading = node.files.lock().unwrap().download_work_pending().unwrap();
        if !downloading
            && !node.media_maintenance_pending().unwrap()
            && node.pending_channel_file().is_none()
        {
            break;
        }
    }
    assert!(!node.files.lock().unwrap().download_work_pending().unwrap());
    assert!(!node.media_maintenance_pending().unwrap());
    assert!(node.pending_channel_file().is_none());
    for job in node.group_file_delivery.jobs.lock().unwrap().values_mut() {
        job.retry_after = Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
    }
    for index in 0..super::super::transcript_repository::MAX_PAGE_ROWS {
        node.received_files
            .lock()
            .unwrap()
            .record_durable(&unrelated_file_row(index as u64))
            .unwrap();
    }
    node.refill_group_file_delivery().unwrap();
    assert!(
        node.group_file_delivery
            .scan
            .lock()
            .unwrap()
            .cursor
            .is_some()
    );
    assert!(
        node.group_file_work_pending(),
        "unfinished discovery retains short cadence despite cooled jobs"
    );
    let (wait, _) = node.file_pull_schedule(std::time::Duration::from_secs(60));
    assert!(
        wait <= std::time::Duration::from_secs(3),
        "the producer must shorten its current idle wait after discovering unfinished work: {wait:?}"
    );
    node.refill_group_file_delivery().unwrap();
    assert!(
        node.group_file_delivery
            .scan
            .lock()
            .unwrap()
            .cursor
            .is_none()
    );
    assert!(
        !node.group_file_work_pending(),
        "completed discovery and cooled jobs permit idle cadence"
    );
    let (wait, pending) = node.file_pull_schedule(std::time::Duration::from_secs(60));
    assert!(!pending);
    assert_eq!(
        wait,
        std::time::Duration::from_secs(300),
        "cooled EOF work permits sparse repair without shortening retained retry deadlines"
    );
    node.group_file_delivery
        .jobs
        .lock()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()
        .retry_after = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
    let (wait, _) = node.file_pull_schedule(std::time::Duration::from_secs(60));
    assert!(
        wait > std::time::Duration::ZERO && wait <= std::time::Duration::from_secs(10),
        "a retained future retry bounds the current idle wait"
    );
}

#[tokio::test]
async fn group_file_live_exchange_reuses_immutable_offer_between_current_checks() {
    assert_live_group_offer_checks("unchanged").await;
}

#[tokio::test]
async fn group_file_live_exchange_rechecks_consent_membership_and_source() {
    for change in ["consent", "membership", "source", "row_rewrite"] {
        assert_live_group_offer_checks(change).await;
    }
}

async fn assert_live_group_offer_checks(change: &'static str) {
    use futures_util::FutureExt;
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    node.refill_group_file_delivery().unwrap();
    let recipient = {
        let mut jobs = node.group_file_delivery.jobs.lock().unwrap();
        for job in jobs.values_mut() {
            job.retry_after =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        }
        let job = jobs.values_mut().next().unwrap();
        job.retry_after = None;
        job.recipient.clone()
    };
    let signer = fixture
        .peers
        .iter()
        .find(|peer| peer.public() == recipient)
        .unwrap();
    let proof = node.historical_author(&recipient.ed25519_pub).unwrap();
    let declined = super::super::file_consent::Decision {
        file: fixture.file,
        manifest: fixture.manifest,
        sender: node.identity.public(),
        recipient: recipient.clone(),
        sender_account: Some(node.account_id()),
        recipient_account: proof.account_id().unwrap(),
        decision: ReceiveDecision::Declined,
        completion: None,
    }
    .event(signer, 1)
    .unwrap();
    let leave = {
        let log = node.log.lock().unwrap();
        let (parents, lamport) = log.prepare(&fixture.channel).unwrap();
        Event::new(
            signer,
            fixture.channel,
            1,
            parents,
            lamport,
            1,
            EventKind::ChannelLeave,
            vec![],
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = DeviceIdentity::generate();
    let relay_public = relay.public();
    let relay_address = listener.local_addr().unwrap();
    let announce = Announce::new_post_office_with_account(
        &relay,
        &crate::identity::account::Account::generate(),
        "Cost relay",
        listener.local_addr().unwrap().port(),
    );
    assert!(announce.valid_presence());
    node.roster
        .lock()
        .unwrap()
        .update(&announce, "127.0.0.1".parse().unwrap(), &node.user_id());
    let relay_root = tempfile::tempdir().unwrap();
    let relay_keys = relay.secret_bytes();
    let store = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(
            &relay_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1),
        )
        .unwrap(),
    ));
    // Match the signed role advertised in the roster during authentication.
    // Own this one connection directly so fixture teardown also joins its server.
    store.lock().unwrap().require_opaque_envelopes();
    let expected_client = node.identity.public();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        crate::transport::net::set_tcp_keepalive(&stream);
        let channel = crate::transport::SecureChannel::accept_with_presence(
            stream,
            &relay,
            Some(&announce),
            |peer| peer.public_identity() == &expected_client,
        )
        .await
        .unwrap();
        super::super::postbox::serve_relay_connection(channel, store).await;
    });
    // Qualify real authentication and one complete framed mailbox exchange
    // outside the measured custody attempt. Never prime the file payload itself.
    let setup = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        std::panic::AssertUnwindSafe(async {
            let mut channel = node.privacy_sync_dial(relay_address, &relay_public).await?;
            let mailbox = RelayMailbox::new(&node.identity, &recipient, &relay_public).unwrap();
            // A decoded legacy matched=false reply completes the framed capability
            // probe. Malformed replies or transport failures still fail setup.
            let incremental_supported =
                super::super::session::mailbox::negotiate(&mut channel, mailbox.id()).await?;
            assert!(
                !incremental_supported,
                "{change}: setup must preserve this fixture's legacy relay protocol"
            );
            node.recycle_sync_channel(channel);
            let (channel, reused) = node
                .privacy_sync_dial_with_reuse(relay_address, &relay_public)
                .await?;
            assert!(
                reused,
                "{change}: authenticated setup channel was not reusable"
            );
            node.recycle_sync_channel(channel);
            Ok::<_, super::super::session::SessionError>(())
        })
        .catch_unwind(),
    )
    .await;
    if !matches!(setup, Ok(Ok(Ok(())))) {
        server.abort();
        let _ = server.await;
        panic!("{change}: authenticated relay setup failed before measured custody: {setup:?}");
    }
    let observations = Arc::new(Mutex::new(Vec::new()));
    let recorded = observations.clone();
    let weak = Arc::downgrade(node);
    let channel = fixture.channel;
    let manifest = fixture.manifest;
    *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, stage| {
        let Some(value) = stage.strip_prefix("group_file_custody_checked ") else {
            return;
        };
        let node = weak.upgrade().unwrap();
        let allowed = value == "true";
        let mut observations = recorded.lock().unwrap();
        observations.push((allowed, node.received_files.lock().unwrap().body_reads()));
        // Check zero is pre-dial. Check one is the actual mailbox owner's
        // callback after the authenticated channel has been checked out.
        let admitted_mailbox = observations.len() == 2;
        drop(observations);
        if admitted_mailbox && allowed {
            match change {
                "consent" => {
                    node.accept_file_consent_event(&declined, &recipient)
                        .unwrap();
                }
                "membership" => {
                    node.log.lock().unwrap().append(leave.clone()).unwrap();
                    crate::node::test_support::settle_group_metadata(&node, channel);
                }
                "source" => {
                    node.log
                        .lock()
                        .unwrap()
                        .repository()
                        .unwrap()
                        .drop_conversation(&channel)
                        .unwrap();
                }
                "row_rewrite" => {
                    node.received_files
                        .lock()
                        .unwrap()
                        .remove_where(|row| row.event_id == manifest)
                        .unwrap();
                }
                _ => {}
            }
        }
    }));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        std::panic::AssertUnwindSafe(node.retry_group_file_delivery()).catch_unwind(),
    )
    .await;
    *node.account_client_test_hook.lock().unwrap() = None;
    server.abort();
    let joined = server.await;
    if let Err(error) = joined {
        assert!(
            error.is_cancelled(),
            "{change}: owned relay server failed: {error}"
        );
    }
    result
        .unwrap_or_else(|error| panic!("{change}: measured live group mailbox watchdog: {error}"))
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    let observed = observations.lock().unwrap();
    assert!(
        observed.len() >= 3,
        "{change}: must observe preflight and multiple post-authentication mailbox current checks: {observed:?}"
    );
    assert!(
        observed[0].0,
        "{change}: preflight current check must be authorized"
    );
    assert!(
        observed[1].0,
        "{change}: admitted mailbox check must be authorized before mutation"
    );
    if change == "unchanged" {
        assert!(observed.iter().all(|step| step.0));
        assert_eq!(
            observed.last().unwrap().1,
            observed[0].1,
            "one admitted exchange must hydrate its immutable offer once; repeated current checks must retain fresh authority without rereading it: {observed:?}"
        );
    } else {
        assert!(
            observed.iter().skip(2).all(|step| !step.0),
            "{change} must revoke the next current check: {observed:?}"
        );
    }
}

#[tokio::test]
async fn group_file_control_decision_reads_authenticated_manifest_once() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let peer = fixture.peers[0].public();
    // Warm mutable authorization/completion projections before measuring.
    node.file_consent_controls(&peer, &[(fixture.file, fixture.manifest)], true)
        .unwrap();
    let before = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    let writes = node
        .storage
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    let (_, signatures) = crate::eventlog::event::signature_observations::observe(|| {
        for _ in 0..3 {
            assert!(
                node.file_consent_controls(&peer, &[(fixture.file, fixture.manifest)], true)
                    .unwrap()
                    .is_empty()
            );
        }
    });
    assert_eq!(
        signatures, 0,
        "warm authenticated manifests reuse only their intrinsic signature proof"
    );
    let after = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    assert_eq!(
        after.0 - before.0,
        3,
        "one authenticated body fetch per synchronous control decision"
    );
    assert_eq!(
        after.1 - before.1,
        3,
        "one authenticated decode per synchronous control decision"
    );
    assert_eq!(
        node.storage
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap(),
        writes,
        "unchanged consent must not write"
    );
}

#[tokio::test]
async fn group_file_incoming_control_reuses_exact_retained_manifest_and_preserves_read_failure() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let signer = &fixture.peers[0];
    let peer = signer.public();
    // Install a peer-signed equivalent offer in the same retained transcript,
    // using the existing immutable transfer so the actual control owner runs.
    let mut row = node
        .received_files
        .lock()
        .unwrap()
        .entry(fixture.manifest)
        .unwrap()
        .unwrap();
    let event = {
        let mut log = node.log.lock().unwrap();
        let source = log.get(&fixture.manifest).unwrap().unwrap();
        let (parents, lamport) = log.prepare(&fixture.channel).unwrap();
        let sequence = log
            .next_sequence(&fixture.channel, &Author::from_ed25519(peer.ed25519_pub))
            .unwrap();
        let event = Event::new(
            signer,
            fixture.channel,
            sequence,
            parents,
            lamport,
            source.wall_clock,
            EventKind::FileManifest,
            source.ciphertext,
        );
        log.append(event.clone()).unwrap();
        event
    };
    row.event_id = event.id;
    row.from = peer.user_id();
    row.wall_clock = event.wall_clock;
    node.received_files
        .lock()
        .unwrap()
        .record_file_with_reception(&row, Some(&event), None)
        .unwrap();
    crate::node::test_support::settle_group_metadata(node, fixture.channel);
    let offers = [(fixture.file, event.id)];
    let first = node.file_consent_controls(&peer, &offers, true).unwrap();
    assert_eq!(first.len(), 1);
    let decision = super::super::file_consent::Decision::open(&first[0]).unwrap();
    assert_eq!(decision.manifest, event.id);
    assert_eq!(decision.sender, peer);
    assert_eq!(decision.recipient, node.identity.public());
    // Initial publication has not yet crossed the persisted-control read
    // verifier. Warm that independent proof before measuring unchanged reads.
    assert_eq!(
        node.file_consent_controls(&peer, &offers, true).unwrap(),
        first
    );
    let before = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    let writes = node
        .storage
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    let (_, signatures) = crate::eventlog::event::signature_observations::observe(|| {
        for _ in 0..3 {
            let controls = node.file_consent_controls(&peer, &offers, true).unwrap();
            assert_eq!(controls.len(), 1);
            assert_eq!(controls[0].id, first[0].id);
            assert_eq!(controls[0].sig, first[0].sig);
        }
    });
    let after = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    assert_eq!((after.0 - before.0, after.1 - before.1), (3, 3));
    assert_eq!(
        signatures, 0,
        "warm unchanged controls reuse intrinsic proofs while every manifest is still read and authenticated"
    );
    assert_eq!(
        node.storage
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap(),
        writes
    );
    assert!(
        node.incoming_offer_with_retained_event(
            fixture.file,
            Some(event.id),
            Some(&peer),
            Some(&event)
        )
        .unwrap()
        .is_some()
    );
    node.log.lock().unwrap().fail_read_for(Some(event.id));
    assert!(
        node.incoming_offer_with_retained_event(
            fixture.file,
            Some(event.id),
            Some(&peer),
            Some(&event)
        )
        .is_err(),
        "retained-body reuse must preserve a current read failure"
    );
    node.log.lock().unwrap().fail_read_for(None);
    assert!(
        node.incoming_offer_with_retained_event(
            fixture.file,
            Some(event.id),
            Some(&peer),
            Some(&event)
        )
        .unwrap()
        .is_some(),
        "the same offer recovers when its owner becomes readable"
    );
}

#[tokio::test]
async fn group_file_blocked_checkpoint_keeps_backoff_without_repeated_offer_reads() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let jobs: Vec<_> = node
        .group_file_delivery
        .jobs
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    for job in &jobs {
        assert!(settle_job(node, job));
    }
    node.refill_group_file_delivery().unwrap();
    let (key, job) = {
        let mut queued = node.group_file_delivery.jobs.lock().unwrap();
        for job in queued.values_mut() {
            job.retry_after =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        }
        let (key, job) = queued.iter_mut().next().unwrap();
        job.retry_after = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        job.retry_delay = std::time::Duration::from_secs(12);
        (*key, job.clone())
    };
    // The fixture has no running channel owner. This real append leaves its
    // published checkpoint stale until that owner is explicitly advanced.
    append_history(node, fixture.channel, 1);
    assert!(
        matches!(
            node.group_file_custody_with_attempt(&job, Some(&AttemptOffer::new(None))),
            Err(LogError::StaleCursor)
        ),
        "the held dependency must actually reject current custody authorization"
    );
    node.retry_group_file_delivery().await;
    {
        let queued = node.group_file_delivery.jobs.lock().unwrap();
        assert_eq!(
            queued.len(),
            5,
            "unavailable authority must not discard durable pending work"
        );
        let waiting = queued.get(&key).unwrap();
        assert!(
            waiting
                .retry_after
                .is_some_and(|deadline| deadline > std::time::Instant::now()),
            "an unchanged blocked checkpoint is not finite preparation progress and must retain the launch deadline"
        );
        assert!(waiting.retry_delay >= std::time::Duration::from_secs(12));
    }
    let reads = node.received_files.lock().unwrap().body_reads();
    node.retry_group_file_delivery().await;
    assert_eq!(
        node.received_files.lock().unwrap().body_reads(),
        reads,
        "an observer wake before recovery must not hydrate the same blocked offer again"
    );
    crate::node::test_support::settle_group_metadata(node, fixture.channel);
    assert!(
        node.group_file_delivery
            .jobs
            .lock()
            .unwrap()
            .get(&key)
            .unwrap()
            .retry_after
            .is_none(),
        "actual checkpoint publication must wake the same waiting job before its backoff expires"
    );
    for _ in 0..4096 {
        node.retry_group_file_delivery().await;
        let settled = {
            let queued = node.group_file_delivery.jobs.lock().unwrap();
            let job = queued.get(&key).unwrap();
            !job.waiting_dependency && job.retry_after.is_some()
        };
        if settled {
            break;
        }
    }
    let queued = node.group_file_delivery.jobs.lock().unwrap();
    let resumed = queued.get(&key).unwrap();
    assert!(
        !resumed.waiting_dependency && resumed.retry_after.is_some(),
        "finite preparation must converge and resume normal custody retry after recovery"
    );
}

#[tokio::test]
async fn group_file_dependency_wake_matches_complete_recipient_and_channel() {
    let fixture = queued_fixture().await;
    let node = &fixture.node;
    let (key, recipient) = {
        let mut jobs = node.group_file_delivery.jobs.lock().unwrap();
        for job in jobs.values_mut() {
            job.waiting_dependency = true;
            job.retry_after = Some(std::time::Instant::now() + std::time::Duration::from_secs(60));
        }
        let (key, job) = jobs.iter().next().unwrap();
        (*key, job.recipient.clone())
    };
    node.wake_group_file_dependency(ConversationId::new([255; 32]), None);
    let mut wrong_key = recipient.x25519_pub;
    wrong_key[0] ^= 1;
    node.wake_group_file_dependency(fixture.channel, Some((recipient.ed25519_pub, wrong_key)));
    assert!(
        node.group_file_delivery
            .jobs
            .lock()
            .unwrap()
            .values()
            .all(|job| job.retry_after.is_some())
    );
    node.wake_group_file_dependency(
        fixture.channel,
        Some((recipient.ed25519_pub, recipient.x25519_pub)),
    );
    node.wake_group_file_dependency(
        fixture.channel,
        Some((recipient.ed25519_pub, recipient.x25519_pub)),
    );
    let jobs = node.group_file_delivery.jobs.lock().unwrap();
    assert!(jobs[&key].retry_after.is_none());
    assert!(!jobs[&key].waiting_dependency);
    assert!(
        jobs.iter()
            .filter(|(other, _)| **other != key)
            .all(|(_, job)| job.waiting_dependency && job.retry_after.is_some())
    );
}
