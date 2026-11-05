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
        std::time::Duration::from_secs(60),
        "cooled EOF work permits the full idle wait"
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
    let announce =
        Announce::new_post_office(&relay, "Cost relay", listener.local_addr().unwrap().port());
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
    let server = tokio::spawn(super::super::postbox::run_relay_accept_loop(
        relay, listener, store,
    ));
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
        let first = observations.is_empty();
        observations.push((allowed, node.received_files.lock().unwrap().body_reads()));
        drop(observations);
        if first && allowed {
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
        node.retry_group_file_delivery(),
    )
    .await;
    *node.account_client_test_hook.lock().unwrap() = None;
    server.abort();
    let joined = server.await;
    assert!(joined.is_err_and(|error| error.is_cancelled()));
    result.expect("live group mailbox cost fixture watchdog");
    let observed = observations.lock().unwrap();
    assert!(
        observed.len() >= 2,
        "must observe multiple real current checks in one admitted attempt: {observed:?}"
    );
    assert!(observed[0].0, "first current check must be authorized");
    if change == "unchanged" {
        assert!(observed.iter().all(|step| step.0));
        assert_eq!(
            observed.last().unwrap().1,
            observed[0].1,
            "one admitted exchange must hydrate its immutable offer once; repeated current checks must retain fresh authority without rereading it: {observed:?}"
        );
    } else {
        assert!(
            observed.iter().skip(1).all(|step| !step.0),
            "{change} must revoke the next current check: {observed:?}"
        );
    }
}
