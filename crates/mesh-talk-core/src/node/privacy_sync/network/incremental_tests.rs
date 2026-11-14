use super::tests::{collect, prepare_upload, upload_page_budget};
use super::*;
use crate::{
    discovery::Announce,
    identity::{account::Account, device::DeviceIdentity},
};

#[test]
fn ready_history_upload_id_pages_do_not_rewrite_terminal_export_progress() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let conversation = ConversationId::new([202; 32]);
    let mut expected = Vec::new();
    for sequence in 1..=40 {
        let event = Event::new(
            &node.identity,
            conversation,
            sequence,
            vec![],
            sequence,
            0,
            EventKind::Message,
            vec![1; 32],
        );
        expected.push(event.id);
        node.log.lock().unwrap().append_durable(event).unwrap();
    }
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    let upload = store.open_history_upload(&conversation).unwrap();
    prepare_upload(&store, &upload);
    let database = node.storage.database();
    let changes = database.read(|c| Ok(c.total_changes())).unwrap();
    let commits = database.successful_write_commits();
    let reprepare = |reset| {
        database
            .read(|c| {
                let mut statement = c.prepare_cached("SELECT count(*) FROM retained_events")?;
                let _: i64 = statement.query_row([], |r| r.get(0))?;
                Ok(if reset {
                    statement.reset_status(rusqlite::StatementStatus::RePrepare)
                } else {
                    statement.get_status(rusqlite::StatementStatus::RePrepare)
                })
            })
            .unwrap()
    };
    reprepare(true);
    let bodies = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    let mut rank = -1;
    let mut actual = Vec::new();
    for _ in 0..8 {
        let step = store
            .prepare_history_upload_step(&upload, PreparationBudget::default())
            .unwrap();
        assert!(step.ready);
        assert_eq!((step.rows, step.edges, step.bytes), (0, 0, 0));
        let page = store
            .history_upload_id_page(
                &upload,
                rank,
                IdPageBudget {
                    ids: 16,
                    examined: 64,
                },
            )
            .unwrap();
        actual.extend(page.ids);
        rank = page.next_rank;
        assert_eq!(
            database.read(|c| Ok(c.total_changes())).unwrap(),
            changes,
            "ready ID pages must not rewrite terminal export progress"
        );
        assert_eq!(database.successful_write_commits(), commits);
        if page.complete {
            break;
        }
    }
    assert_eq!(
        reprepare(false),
        0,
        "completed preparation must not enter a guarded write transaction and expire cached reads"
    );
    assert_eq!(actual, expected);
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        bodies
    );
    // The direct network consumer shares the same deep export owner. Its
    // terminal calls must be quiet too, rather than relying on an upload flag.
    let direct = open_prepared(&store, &conversation);
    let have = HashSet::new();
    let mut reset_ready = false;
    for _ in 0..1000 {
        if NetworkSyncStore::reset_export_step(&*store, &direct, PreparationBudget::default())
            .unwrap()
            .ready
        {
            reset_ready = true;
            break;
        }
    }
    assert!(reset_ready);
    let mut export_ready = false;
    for _ in 0..1000 {
        if NetworkSyncStore::prepare_export_step(
            &*store,
            &direct,
            &have,
            PreparationBudget::default(),
        )
        .unwrap()
        .ready
        {
            export_ready = true;
            break;
        }
    }
    assert!(export_ready);
    let direct_changes = database.read(|c| Ok(c.total_changes())).unwrap();
    let direct_commits = database.successful_write_commits();
    reprepare(true);
    for _ in 0..8 {
        let step = NetworkSyncStore::prepare_export_step(
            &*store,
            &direct,
            &have,
            PreparationBudget::default(),
        )
        .unwrap();
        assert!(step.ready);
        assert_eq!((step.rows, step.edges, step.bytes), (0, 0, 0));
        assert_eq!(
            database.read(|c| Ok(c.total_changes())).unwrap(),
            direct_changes
        );
        assert_eq!(database.successful_write_commits(), direct_commits);
    }
    assert_eq!(
        reprepare(false),
        0,
        "direct terminal preparation must preserve cached read plans too"
    );
    // Ready is a work fact, never permission to bypass current authority.
    node.privacy.generation.fetch_add(1, Ordering::SeqCst);
    assert!(matches!(
        store.prepare_history_upload_step(&upload, PreparationBudget::default()),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        NetworkSyncStore::prepare_export_step(
            &*store,
            &direct,
            &have,
            PreparationBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
}

#[test]
fn participant_history_upload_finishes_captured_prefix_during_continuous_appends() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let conversation = ConversationId::new([205; 32]);
    let mut seq = 0;
    let mut append = || {
        seq += 1;
        let event = Event::new(
            &node.identity,
            conversation,
            seq,
            vec![],
            seq,
            0,
            EventKind::Message,
            vec![1; 32],
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(event.clone())
            .unwrap();
        event.id
    };
    let expected: Vec<_> = (0..32).map(|_| append()).collect();
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    let mut upload = store.open_history_upload(&conversation).unwrap();
    let mut ready = false;
    for _ in 0..1000 {
        append();
        let step = store
            .prepare_history_upload_step(
                &upload,
                PreparationBudget {
                    rows: 8,
                    edges: 16,
                    bytes: 65536,
                },
            )
            .unwrap();
        if step.ready {
            ready = true;
            break;
        }
    }
    assert!(
        ready,
        "ordinary appends must not starve a finite initial preparation"
    );
    let mut rank = -1;
    let mut actual = Vec::new();
    loop {
        append();
        store.validate_history_upload(&upload).unwrap();
        let page = store
            .history_upload_page(
                &upload,
                rank,
                ExportPageBudget {
                    examined: 4,
                    events: 4,
                    ..upload_page_budget()
                },
            )
            .unwrap();
        actual.extend(page.events.into_iter().map(|event| event.id));
        rank = page.next_rank;
        if page.complete {
            break;
        }
    }
    assert_eq!(actual, expected);
    assert_eq!(rank, 31);
    let upper = seq;
    assert!(store.resume_history_upload(&mut upload).unwrap());
    prepare_upload(&store, &upload);
    let mut tail = Vec::new();
    loop {
        let page = store
            .history_upload_page(&upload, rank, upload_page_budget())
            .unwrap();
        tail.extend(page.events);
        rank = page.next_rank;
        if page.complete {
            break;
        }
    }
    assert_eq!(tail.len(), upper as usize - 32);
    assert_eq!(rank, upper as i64 - 1);
}

#[test]
fn participant_history_upload_id_pages_are_bodyless_and_exact_fetch_is_budgeted() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let conversation = ConversationId::new([206; 32]);
    let event = Event::new(
        &node.identity,
        conversation,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![1; 8192],
    );
    node.log
        .lock()
        .unwrap()
        .append_durable(event.clone())
        .unwrap();
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    let upload = store.open_history_upload(&conversation).unwrap();
    prepare_upload(&store, &upload);
    let counts = || {
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts()
    };
    let before = counts();
    let page = store
        .history_upload_id_page(
            &upload,
            -1,
            IdPageBudget {
                ids: 1,
                examined: 1,
            },
        )
        .unwrap();
    assert_eq!(page.ids, vec![event.id]);
    assert!(page.complete);
    assert!(store.history_upload_contains(&upload, event.id).unwrap());
    assert!(
        !store
            .history_upload_contains(&upload, EventId::new([255; 32]))
            .unwrap()
    );
    let other = node.sync_store(&DeviceIdentity::generate().public());
    assert!(matches!(
        other
            .lock()
            .unwrap()
            .history_upload_contains(&upload, event.id),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(counts(), before);
    assert!(matches!(
        store.history_upload_event(&upload, event.id, 1),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert!(
        store
            .history_upload_event(&upload, EventId::new([255; 32]), 65536)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        counts(),
        before,
        "budget and membership precede body access"
    );
    assert_eq!(
        store
            .history_upload_event(&upload, event.id, 65536)
            .unwrap(),
        Some(event)
    );
    assert_eq!(counts().0 - before.0, 1);
    node.privacy.generation.fetch_add(1, Ordering::SeqCst);
    let before_stale = counts();
    assert!(matches!(
        store.history_upload_event(&upload, page.ids[0], 65536),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(counts(), before_stale);
}

#[test]
fn participant_history_upload_append_reuses_authorized_prefix_without_full_hash() {
    for count in [128, 512] {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let mut previous = None;
        for seq in 1..=count {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                previous.into_iter().collect(),
                seq,
                0,
                EventKind::Message,
                vec![7; 32],
            );
            previous = Some(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let mut upload = store.open_history_upload(&conversation).unwrap();
        prepare_upload(&store, &upload);
        let mut rank = -1;
        let mut ids = Vec::new();
        loop {
            let page = store
                .history_upload_page(&upload, rank, upload_page_budget())
                .unwrap();
            ids.extend(page.events.into_iter().map(|event| event.id));
            rank = page.next_rank;
            if page.complete {
                break;
            }
        }
        assert_eq!(ids.len(), count as usize);
        assert_eq!(rank, count as i64 - 1);
        let tail = Event::new(
            &node.identity,
            conversation,
            count + 1,
            previous.into_iter().collect(),
            count + 1,
            0,
            EventKind::Message,
            vec![8; 32],
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(tail.clone())
            .unwrap();
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(store.resume_history_upload(&mut upload).unwrap());
        let work = prepare_upload(&store, &upload);
        assert!(
            work <= 8,
            "one append must stay within the bounded incremental preparation work"
        );
        assert!(matches!(
            store.history_upload_page(
                &upload,
                rank,
                ExportPageBudget {
                    encoded_bytes: 1,
                    ..upload_page_budget()
                }
            ),
            Err(LogError::PageBudgetTooSmall { .. })
        ));
        let page = store
            .history_upload_page(&upload, rank, upload_page_budget())
            .unwrap();
        assert_eq!(page.events, vec![tail]);
        assert!(page.complete);
        assert_eq!(page.next_rank, count as i64);
        let after = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert_eq!(
            after.0 - before.0,
            1,
            "only the appended selected payload may be fetched"
        );
    }
}

#[test]
fn participant_history_upload_rejects_reordered_prefix_and_changed_authority() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let conversation = ConversationId::new([204; 32]);
    let first = Event::new(
        &node.identity,
        conversation,
        1,
        vec![],
        10,
        0,
        EventKind::Message,
        vec![1],
    );
    node.log.lock().unwrap().append_durable(first).unwrap();
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    let mut upload = store.open_history_upload(&conversation).unwrap();
    prepare_upload(&store, &upload);
    node.privacy.generation.fetch_add(1, Ordering::SeqCst);
    assert!(matches!(
        store.history_upload_page(&upload, -1, upload_page_budget()),
        Err(LogError::StaleCursor)
    ));
    assert!(!store.resume_history_upload(&mut upload).unwrap());
    let mut fresh = store.open_history_upload(&conversation).unwrap();
    prepare_upload(&store, &fresh);
    node.log
        .lock()
        .unwrap()
        .append_durable(Event::new(
            &node.identity,
            conversation,
            2,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![2],
        ))
        .unwrap();
    assert!(
        !store.resume_history_upload(&mut fresh).unwrap(),
        "changed canonical order must rebuild authority"
    );
}

#[tokio::test]
async fn participant_history_upload_refreshes_modern_authorization_for_new_tail() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let conversation = node
        .create_channel("Upload tail", vec![peer.public()])
        .await
        .unwrap();
    node.await_channel_metadata(conversation).await.unwrap();
    for _ in 0..128 {
        node.append_event(conversation, EventKind::Message, vec![1])
            .unwrap();
    }
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    let mut upload = store.open_history_upload(&conversation).unwrap();
    prepare_upload(&store, &upload);
    let mut rank = -1;
    loop {
        let page = store
            .history_upload_page(&upload, rank, upload_page_budget())
            .unwrap();
        rank = page.next_rank;
        if page.complete {
            break;
        }
    }
    let tail = node
        .append_event_with_profile_parent(conversation, EventKind::Message, vec![2], None)
        .unwrap()
        .1;
    assert!(store.resume_history_upload(&mut upload).unwrap());
    assert!(prepare_upload(&store, &upload) <= 16);
    let page = store
        .history_upload_page(&upload, rank, upload_page_budget())
        .unwrap();
    assert_eq!(
        page.events.iter().map(|event| event.id).collect::<Vec<_>>(),
        vec![tail]
    );
    assert!(page.complete);
}

#[tokio::test]
async fn participant_history_upload_modern_prefix_progresses_with_continuous_appends() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let conversation = node
        .create_channel("Finite upload", vec![peer.public()])
        .await
        .unwrap();
    node.await_channel_metadata(conversation).await.unwrap();
    for _ in 0..8 {
        node.append_event(conversation, EventKind::Message, vec![1])
            .unwrap();
    }
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    let expected = {
        let view = open_prepared(&store, &conversation);
        collect(&store, &view, &HashSet::new())
    };
    let upper = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .count(&conversation)
        .unwrap();
    let mut upload = store.open_history_upload(&conversation).unwrap();
    let mut ready = false;
    for _ in 0..1000 {
        node.append_event(conversation, EventKind::Message, vec![2])
            .unwrap();
        if store
            .prepare_history_upload_step(
                &upload,
                PreparationBudget {
                    rows: 8,
                    edges: 16,
                    bytes: 65536,
                },
            )
            .unwrap()
            .ready
        {
            ready = true;
            break;
        }
    }
    assert!(
        ready,
        "ordinary appends starved the finite Modern projection"
    );
    let mut ids = Vec::new();
    let mut rank = -1;
    loop {
        node.append_event(conversation, EventKind::Message, vec![3])
            .unwrap();
        let page = store
            .history_upload_id_page(
                &upload,
                rank,
                IdPageBudget {
                    ids: 2,
                    examined: 2,
                },
            )
            .unwrap();
        ids.extend(page.ids);
        rank = page.next_rank;
        if page.complete {
            break;
        }
    }
    assert_eq!(ids, expected);
    assert_eq!(rank, upper as i64 - 1);

    // A resumed window must project its newly appended rows without chasing
    // the still-growing whole source or restarting each graph decision.
    let resumed_upper = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .count(&conversation)
        .unwrap();
    assert!(store.resume_history_upload(&mut upload).unwrap());
    let mut resumed = false;
    for _ in 0..1000 {
        node.append_event(conversation, EventKind::Message, vec![4])
            .unwrap();
        // Ordinary recovery and this finite consumer share the same decision
        // scratch. Neither may retire the other's immutable graph answers just
        // because their source-revision observations differ.
        {
            let log = node.log.lock().unwrap();
            node.channel_checkpoints
                .project_step_with_budget(
                    log.repository().unwrap(),
                    &node.identity,
                    conversation,
                    PreparationBudget {
                        rows: 8,
                        edges: 16,
                        bytes: 65536,
                    },
                )
                .unwrap();
        }
        if store
            .prepare_history_upload_step(
                &upload,
                PreparationBudget {
                    rows: 8,
                    edges: 16,
                    bytes: 65536,
                },
            )
            .unwrap()
            .ready
        {
            resumed = true;
            break;
        }
    }
    assert!(
        resumed,
        "ordinary appends starved the finite resumed Modern projection"
    );
    loop {
        let page = store
            .history_upload_id_page(
                &upload,
                rank,
                IdPageBudget {
                    ids: 16,
                    examined: 64,
                },
            )
            .unwrap();
        rank = page.next_rank;
        if page.complete {
            break;
        }
    }
    assert_eq!(rank, resumed_upper as i64 - 1);

    // Raw controls remain an exact authority fence even while a finite prefix
    // is incomplete. Prefix progress cannot absorb a newly published policy.
    assert!(store.resume_history_upload(&mut upload).unwrap());
    assert!(
        !store
            .prepare_history_upload_step(
                &upload,
                PreparationBudget {
                    rows: 8,
                    edges: 16,
                    bytes: 65536,
                }
            )
            .unwrap()
            .ready
    );
    node.append_event(conversation, EventKind::ChannelReplica, vec![0])
        .unwrap();
    let before = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    assert!(matches!(
        store.prepare_history_upload_step(&upload, PreparationBudget::default()),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        store.history_upload_id_page(
            &upload,
            rank,
            IdPageBudget {
                ids: 16,
                examined: 64
            }
        ),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        before
    );
}

#[test]
fn participant_history_sync_stamp_is_metadata_only_and_exact() {
    for count in [128, 512] {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let append = |conv, seq, kind| {
            node.log
                .lock()
                .unwrap()
                .append_durable(Event::new(
                    &node.identity,
                    conv,
                    seq,
                    vec![],
                    seq,
                    0,
                    kind,
                    vec![7; 2048],
                ))
                .unwrap();
        };
        for seq in 1..=count {
            append(conversation, seq, EventKind::Message);
        }
        {
            let log = node.log.lock().unwrap();
            let repository = log.repository().unwrap();
            while !repository
                .prepare_kind_metadata_step(PreparationBudget::default())
                .unwrap()
                .ready
            {}
        }
        let counts = || {
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts()
        };
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let reads = counts();
        let initial = store
            .history_sync_stamp(&conversation)
            .unwrap()
            .expect("ready metadata must supply an exact history stamp");
        assert_eq!(
            store.history_sync_stamp(&conversation).unwrap(),
            Some(initial.clone())
        );
        let bytes = bincode::serialize(&initial).unwrap();
        assert_eq!(
            bincode::deserialize::<HistorySyncStamp>(&bytes).unwrap(),
            initial
        );
        let unrelated = ConversationId::new([203; 32]);
        append(unrelated, 1, EventKind::Message);
        assert_eq!(
            store.history_sync_stamp(&conversation).unwrap(),
            Some(initial.clone()),
            "unrelated ordinary history must not invalidate this conversation"
        );
        append(conversation, count + 1, EventKind::Message);
        let appended = store.history_sync_stamp(&conversation).unwrap().unwrap();
        assert_ne!(
            appended, initial,
            "finite-view append tolerance cannot certify idle history"
        );
        append(unrelated, 2, EventKind::ChannelAccountAuthority);
        let controlled = store.history_sync_stamp(&conversation).unwrap().unwrap();
        assert_ne!(
            controlled, appended,
            "even rejected cross-conversation controls fence authorization"
        );
        node.privacy.generation.fetch_add(1, Ordering::SeqCst);
        assert_ne!(
            store.history_sync_stamp(&conversation).unwrap().unwrap(),
            controlled
        );
        let other = node.sync_store(&DeviceIdentity::generate().public());
        assert_ne!(
            other
                .lock()
                .unwrap()
                .history_sync_stamp(&conversation)
                .unwrap(),
            store.history_sync_stamp(&conversation).unwrap(),
            "recipient identities bind stamps"
        );
        assert_ne!(
            store.history_sync_stamp(&unrelated).unwrap(),
            store.history_sync_stamp(&conversation).unwrap(),
            "conversation identities bind stamps"
        );
        assert_eq!(
            counts(),
            reads,
            "stamp capture must never load history bodies at either size"
        );
        let before_retirement = store.history_sync_stamp(&conversation).unwrap();
        node.log
            .lock()
            .unwrap()
            .drop_conversation(&unrelated)
            .unwrap();
        assert_ne!(
            store.history_sync_stamp(&conversation).unwrap(),
            before_retirement,
            "destructive changes in another conversation fence dependent file authority"
        );
    }
}

// Ordinary sync must distinguish presentation retirement from retained custody.
async fn ordinary_profile_compaction_round(
    sender: &std::sync::Arc<Node>,
    receiver: &std::sync::Arc<Node>,
    conversation: ConversationId,
) {
    use crate::{
        node::session::{Served, request_round_paged, serve_one_paged},
        transport::SecureChannel,
    };
    struct Server(Option<tokio::task::JoinHandle<()>>);
    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(task) = &self.0 {
                task.abort();
            }
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_node = sender.clone();
    let remote = receiver.identity.public();
    let mut server = Server(Some(tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &server_node.identity)
            .await
            .unwrap();
        let store = server_node.serve_sync_store(&remote);
        loop {
            if matches!(
                serve_one_paged(&mut channel, &store).await.unwrap(),
                Served::Closed
            ) {
                break;
            }
        }
    })));
    let socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut channel =
        SecureChannel::connect(socket, &receiver.identity, Some(&sender.identity.public()))
            .await
            .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        request_round_paged(
            &mut channel,
            &receiver.sync_store(&sender.identity.public()),
            conversation,
        ),
    )
    .await
    .expect("ordinary Profile round stalled")
    .unwrap();
    drop(channel);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        server.0.as_mut().unwrap(),
    )
    .await
    .expect("ordinary Profile server did not retire")
    .unwrap();
    server.0.take();
}

#[tokio::test]
async fn ordinary_sync_automatic_profile_maintenance_settles_after_legacy_replay() {
    assert_ordinary_profile_compaction(false).await;
}

#[tokio::test]
async fn ordinary_sync_compacted_profile_parent_remains_available_to_late_child() {
    assert_ordinary_profile_compaction(true).await;
}

async fn assert_ordinary_profile_compaction(repair: bool) {
    let sender_dir = tempfile::tempdir().unwrap();
    let receiver_dir = tempfile::tempdir().unwrap();
    let sender_identity = DeviceIdentity::generate();
    let receiver_identity = DeviceIdentity::generate();
    let sender_account = Account::generate();
    let receiver_account = Account::generate();
    let sender_proof = Announce::new_with_account(&sender_identity, &sender_account, "Sender", 9);
    let receiver_proof =
        Announce::new_with_account(&receiver_identity, &receiver_account, "Receiver", 9);
    let (sender, _, _) = super::super::super::test_support::node_with_files(
        sender_dir.path(),
        sender_identity,
        sender_account,
        &receiver_proof,
    );
    let (receiver, _, _) = super::super::super::test_support::node_with_files(
        receiver_dir.path(),
        receiver_identity,
        receiver_account,
        &sender_proof,
    );
    sender.cached_peer_snapshot_async().await.unwrap();
    receiver.cached_peer_snapshot_async().await.unwrap();
    let conversation = crate::node::conversation::dm_conversation_id(
        &sender.identity.public(),
        &receiver.identity.public(),
    );
    let mut profiles = Vec::new();
    for revision in 1..=12 {
        let payload = crate::node::profile::ProfilePayload::new(&sender.account, None, revision);
        let event = Event::new(
            &sender.identity,
            conversation,
            revision,
            vec![],
            revision,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &sender.identity,
                &receiver.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        profiles.push(event.id);
        sender.log.lock().unwrap().append_durable(event).unwrap();
    }
    ordinary_profile_compaction_round(&sender, &receiver, conversation).await;
    assert!(
        profiles
            .iter()
            .all(|id| receiver.log.lock().unwrap().has(id).unwrap()),
        "initial ordinary sync must actually admit all twelve validated Profiles"
    );
    receiver.process_profile_events(conversation);
    let processed: i64 = receiver
        .storage
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM profile_receive_processed WHERE account=?1",
                [receiver.account.public().ed25519_pub.as_slice()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        processed, 12,
        "fixture must establish real durable Profile presentation terminals"
    );
    let initial_dropped = if repair {
        receiver
            .log
            .lock()
            .unwrap()
            .compact_superseded_profiles(&conversation)
            .unwrap()
    } else {
        drain_ordinary_profile_maintenance(&receiver)
    };
    assert_eq!(
        initial_dropped, 10,
        "actual maintenance must retire ten superseded leaves"
    );
    let absent: Vec<_> = profiles
        .iter()
        .copied()
        .filter(|id| !receiver.log.lock().unwrap().has(id).unwrap())
        .collect();
    assert_eq!(absent.len(), 10);
    assert!(
        profiles
            .iter()
            .all(|id| sender.log.lock().unwrap().has(id).unwrap()),
        "sender must still retain the compacted receiver history"
    );
    let source = || {
        receiver
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .source_revision()
            .unwrap()
    };
    let before = source();
    if repair {
        let child = Event::new(
            &sender.identity,
            conversation,
            13,
            vec![absent[0]],
            13,
            0,
            EventKind::Message,
            crate::dm::seal(
                &sender.identity,
                &receiver.identity.public().x25519_pub,
                b"late child",
            )
            .unwrap(),
        );
        let child_id = child.id;
        sender.log.lock().unwrap().append_durable(child).unwrap();
        ordinary_profile_compaction_round(&sender, &receiver, conversation).await;
        assert!(
            receiver.log.lock().unwrap().has(&absent[0]).unwrap(),
            "presentation evidence cannot replace raw custody of a required Profile parent"
        );
        assert!(
            receiver.log.lock().unwrap().has(&child_id).unwrap(),
            "ordinary late-child admission must retain its exact required parent"
        );
    } else {
        // Deployed ordinary sync may restore raw leaves once. Presentation
        // terminals are not retained HAVE or custody; automatic maintenance
        // must instead avoid retiring the same replayed IDs in a feedback loop.
        ordinary_profile_compaction_round(&sender, &receiver, conversation).await;
        receiver.process_profile_events(conversation);
        assert_eq!(
            drain_ordinary_profile_maintenance(&receiver),
            0,
            "processed-ID legacy replay must not schedule another destructive epoch"
        );
        let settled = source();
        assert!(settled >= before);
        let retained = || {
            profiles
                .iter()
                .map(|id| receiver.log.lock().unwrap().has(id).unwrap())
                .collect::<Vec<_>>()
        };
        let settled_ids = retained();
        for _ in 0..3 {
            ordinary_profile_compaction_round(&sender, &receiver, conversation).await;
            receiver.process_profile_events(conversation);
            assert_eq!(drain_ordinary_profile_maintenance(&receiver), 0);
            assert_eq!(
                source(),
                settled,
                "unchanged automatic maintenance/sync must settle"
            );
            assert_eq!(retained(), settled_ids);
        }
        let payload = crate::node::profile::ProfilePayload::new(&sender.account, None, 13);
        let fresh = Event::new(
            &sender.identity,
            conversation,
            13,
            vec![],
            13,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &sender.identity,
                &receiver.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        let fresh_id = fresh.id;
        sender.log.lock().unwrap().append_durable(fresh).unwrap();
        ordinary_profile_compaction_round(&sender, &receiver, conversation).await;
        receiver.process_profile_events(conversation);
        assert_eq!(
            drain_ordinary_profile_maintenance(&receiver),
            11,
            "a genuinely new Profile must advance demand and compact the older eligible epoch"
        );
        assert!(receiver.log.lock().unwrap().has(&fresh_id).unwrap());
        ordinary_profile_compaction_round(&sender, &receiver, conversation).await;
        receiver.process_profile_events(conversation);
        assert_eq!(
            drain_ordinary_profile_maintenance(&receiver),
            0,
            "new-epoch legacy replay must also settle after one bounded backfill"
        );
        let refreshed = source();
        let refreshed_ids = retained();
        for _ in 0..2 {
            ordinary_profile_compaction_round(&sender, &receiver, conversation).await;
            receiver.process_profile_events(conversation);
            assert_eq!(drain_ordinary_profile_maintenance(&receiver), 0);
            assert_eq!(source(), refreshed);
            assert_eq!(retained(), refreshed_ids);
            assert!(receiver.log.lock().unwrap().has(&fresh_id).unwrap());
        }
    }
}

fn drain_ordinary_profile_maintenance(node: &Node) -> usize {
    // Use the actual bounded runtime owner, including participant cleanup and
    // compaction-lease exclusion; do not replace automatic demand with explicit
    // request_profile_compaction on every retry.
    (0..64)
        .map(|_| node.drain_profile_compaction().unwrap().unwrap_or(0))
        .sum()
}

#[tokio::test]
async fn ready_history_upload_owner_retirement_reuses_exact_principal_selection() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    let conversation = node
        .create_channel("Exact history preparation owner", vec![peer.public()])
        .await
        .unwrap();
    let mut messages = Vec::new();
    for _ in 0..32 {
        node.await_channel_metadata(conversation).await.unwrap();
        messages.push(
            node.enqueue_channel_message_reply_expiring_if(
                conversation,
                &[b'm'; 1024],
                None,
                None,
                |commit| commit(),
            )
            .unwrap(),
        );
    }
    node.await_channel_metadata(conversation).await.unwrap();
    let collect_ids = |store: &GuardedSyncStore<'_>, upload: &HistoryUpload| {
        let mut ids = Vec::new();
        let mut rank = -1;
        loop {
            let page = store
                .history_upload_id_page(
                    upload,
                    rank,
                    IdPageBudget {
                        ids: 16,
                        examined: 64,
                    },
                )
                .unwrap();
            ids.extend(page.ids);
            rank = page.next_rank;
            if page.complete {
                break;
            }
        }
        ids
    };
    let (expected, captured) = {
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        // Opaque history uses the recipient projection; the Noise peer is a
        // relay, so this adapter is intentionally not bound as that recipient.
        let upload = store.open_history_upload(&conversation).unwrap();
        prepare_upload(&store, &upload);
        let ids = collect_ids(&store, &upload);
        assert!(messages.iter().all(|id| ids.contains(id)));
        let stamp = store.history_sync_stamp(&conversation).unwrap().unwrap();
        // This is the actual ready HistoryUpload owner. No remote receipt,
        // CompletedUpload certificate or synthetic permission is installed.
        store.retain_history_upload(upload).unwrap();
        (ids, stamp)
    };
    let database = node.storage.database();
    let changes = database
        .read(|connection| Ok(connection.total_changes()))
        .unwrap();
    let commits = database.successful_write_commits();
    let body_reads = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    assert_eq!(
        store.history_sync_stamp(&conversation).unwrap(),
        Some(captured),
        "fresh adapter must observe the exact same full-principal source"
    );
    let upload = store.open_history_upload(&conversation).unwrap();
    let prepared_rows = prepare_upload(&store, &upload);
    assert_eq!(collect_ids(&store, &upload), expected);
    let written = database
        .read(|connection| Ok(connection.total_changes()))
        .unwrap()
        - changes;
    let committed = database.successful_write_commits() - commits;
    eprintln!(
        "history-owner-retirement prepared_rows={prepared_rows} sql_changes={written} write_commits={committed}"
    );
    assert_eq!(
        written, 0,
        "retiring a ready history owner must preserve its exact qualified SQL preparation for a fresh adapter"
    );
    assert_eq!(committed, 0);
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        body_reads,
        "an unchanged ready ID selection must not rehydrate canonical bodies"
    );
}

#[tokio::test]
async fn disposable_selection_prepare_export_and_drop_leave_main_storage_unchanged() {
    let (_directory, node, peer, conversation) = history_retention_fixture().await;
    let owner = node.sync_store(&peer.public());
    let store = owner.lock().unwrap();
    // Finish real canonical ordering, kind/policy classification and authority
    // preparation once. Only a NEW disposable selection is measured below.
    let warm = open_prepared(&store, &conversation);
    let expected = collect(&store, &warm, &HashSet::new());
    assert!(
        expected.len() >= 5,
        "real genesis and four messages must be selected"
    );
    let known = {
        let log = node.log.lock().unwrap();
        *expected
            .iter()
            .find(|id| {
                log.get(id)
                    .unwrap()
                    .is_some_and(|event| event.kind == EventKind::Message)
            })
            .expect("the real group must contain a message")
    };
    drop(warm);
    let database = node.storage.database();
    let storage_snapshot = || {
        let stamp = database.main_write_stamp().unwrap().unwrap();
        let path: String = database
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT file FROM pragma_database_list WHERE name='main'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        let wal = match std::fs::read(format!("{path}-wal")) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("read actual test MAIN WAL: {error}"),
        };
        (stamp, wal)
    };
    let before = storage_snapshot();
    let changes_before = database
        .read(|connection| Ok(connection.total_changes()))
        .unwrap();
    let fresh = open_prepared(&store, &conversation);
    let body_reads = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    let mut have_ids = Vec::new();
    let mut after = None;
    loop {
        let page = NetworkSyncStore::have_page(
            &*store,
            &fresh,
            after.as_ref(),
            IdPageBudget {
                ids: 2,
                examined: 4,
            },
        )
        .unwrap();
        have_ids.extend(page.items);
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(
        have_ids.iter().copied().collect::<HashSet<_>>(),
        expected.iter().copied().collect()
    );
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        body_reads,
        "Have pages must not hydrate historical bodies"
    );
    let have = HashSet::from([known]);
    let exported = collect(&store, &fresh, &have);
    assert_eq!(
        exported,
        expected
            .into_iter()
            .filter(|id| *id != known)
            .collect::<Vec<_>>(),
        "actual export must exclude the peer's real known message"
    );
    drop(fresh);
    assert!(
        database
            .read(|connection| Ok(connection.total_changes()))
            .unwrap()
            > changes_before,
        "the test must execute real disposable preparation/export/retirement SQL"
    );
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .selection_counts()
            .unwrap(),
        (0, 0, 0)
    );
    let after = storage_snapshot();
    assert!(
        after.1 == before.1,
        "fresh disposable preparation/export/drop must leave actual MAIN WAL bytes unchanged"
    );
    // SQLite can bump its version for a no-op MAIN transaction. The owning
    // qualified stamp distinguishes that from a page mutation; raw version alone
    // is not physical write evidence, especially with an attached pager.
    assert!(
        before.0.unchanged_since(&after.0),
        "MAIN page evidence must remain unchanged"
    );
    database.transaction(|connection| {
        connection.execute_batch("CREATE TABLE main.test_selection_business_write(value INTEGER); INSERT INTO main.test_selection_business_write VALUES(1)")?;
        Ok(())
    }).unwrap();
    let committed = storage_snapshot();
    assert!(
        !after.0.unchanged_since(&committed.0),
        "the same observer must detect a real MAIN write"
    );
    assert!(
        after.1 != committed.1,
        "the actual WAL observation must detect that MAIN commit"
    );
}

async fn history_retention_fixture() -> (
    tempfile::TempDir,
    std::sync::Arc<Node>,
    DeviceIdentity,
    ConversationId,
) {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
    let (node, _, _) = super::super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    let conversation = node
        .create_channel("History retention fences", vec![peer.public()])
        .await
        .unwrap();
    for _ in 0..4 {
        node.await_channel_metadata(conversation).await.unwrap();
        node.enqueue_channel_message_reply_expiring_if(
            conversation,
            b"retained original",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    }
    node.await_channel_metadata(conversation).await.unwrap();
    (directory, node, peer, conversation)
}

#[tokio::test]
async fn ready_history_retention_scopes_modes_purpose_and_cancelled_checkout() {
    let (_directory, node, peer, conversation) = history_retention_fixture().await;
    let owner = node.sync_store(&peer.public());
    let mut store = owner.lock().unwrap();
    let upload = store.open_history_upload(&conversation).unwrap();
    prepare_upload(&store, &upload);
    store.retain_history_upload(upload).unwrap();
    assert_eq!(node.prepared_sync_views.lock().unwrap().len(), 1);
    let mut changed_transport = peer.public();
    changed_transport.x25519_pub = DeviceIdentity::generate().public().x25519_pub;
    for other in [
        node.sync_store(&changed_transport),
        node.sync_store(&DeviceIdentity::generate().public()),
        node.serve_sync_store(&peer.public()),
        node.pull_sync_store(&peer.public()),
    ] {
        let other = other.lock().unwrap();
        let fresh = other.open_history_upload(&conversation).unwrap();
        assert!(
            !fresh.preparation_progress().2,
            "different full principal/mode must open fresh"
        );
        assert_eq!(node.prepared_sync_views.lock().unwrap().len(), 1);
    }
    store.denied = true;
    let denied = store.open_history_upload(&conversation).unwrap();
    assert!(!denied.preparation_progress().2);
    drop(denied);
    store.denied = false;
    NetworkSyncStore::bind_session_peer(&mut *store, &peer.public());
    assert!(
        NetworkSyncStore::take_custody_view(&mut *store, &conversation)
            .unwrap()
            .is_none(),
        "ordinary sync cannot consume a history-purpose plan"
    );
    let checked_out = store.open_history_upload(&conversation).unwrap();
    assert!(checked_out.preparation_progress().2);
    assert_eq!(node.prepared_sync_views.lock().unwrap().len(), 0);
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .active_selection_count(),
        1
    );
    // Cancellation owns this checkout, not an implicit Drop callback. Releasing
    // it frees admission and cannot republish into the idle cache.
    drop(checked_out);
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .active_selection_count(),
        0
    );
    assert_eq!(node.prepared_sync_views.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn ready_history_retention_revalidates_append_leave_and_physical_erase() {
    for mutation in ["append", "leave", "erase"] {
        let (_directory, node, peer, conversation) = history_retention_fixture().await;
        let owner = node.sync_store(&peer.public());
        {
            let store = owner.lock().unwrap();
            let upload = store.open_history_upload(&conversation).unwrap();
            prepare_upload(&store, &upload);
            store.retain_history_upload(upload).unwrap();
        }
        let new_message = match mutation {
            "append" => Some(
                node.enqueue_channel_message_reply_expiring_if(
                    conversation,
                    b"new source",
                    None,
                    None,
                    |commit| commit(),
                )
                .unwrap(),
            ),
            "leave" => {
                let leave = {
                    let log = node.log.lock().unwrap();
                    let (parents, lamport) = log.prepare(&conversation).unwrap();
                    Event::new(
                        &peer,
                        conversation,
                        1,
                        parents,
                        lamport,
                        1,
                        EventKind::ChannelLeave,
                        vec![],
                    )
                };
                node.log.lock().unwrap().append_durable(leave).unwrap();
                node.await_channel_metadata(conversation).await.unwrap();
                Some(
                    node.enqueue_channel_message_reply_expiring_if(
                        conversation,
                        b"after recipient left",
                        None,
                        None,
                        |commit| commit(),
                    )
                    .unwrap(),
                )
            }
            "erase" => {
                assert!(
                    node.log
                        .lock()
                        .unwrap()
                        .drop_conversation(&conversation)
                        .unwrap()
                        > 0
                );
                None
            }
            _ => unreachable!(),
        };
        let store = owner.lock().unwrap();
        let fresh = store.open_history_upload(&conversation).unwrap();
        assert!(
            !fresh.preparation_progress().2,
            "mutation={mutation}: stale plan must not be reused"
        );
        if mutation == "erase" {
            // Erasing the signed genesis removes canonical grant authority; a
            // stale ChannelBook cannot authorize a completed empty projection.
            assert_eq!(node.prepared_sync_views.lock().unwrap().len(), 0);
            assert!(
                node.log
                    .lock()
                    .unwrap()
                    .events(&conversation)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                matches!(
                    store.history_upload_id_page(
                        &fresh,
                        -1,
                        IdPageBudget {
                            ids: 16,
                            examined: 64
                        }
                    ),
                    Err(LogError::StaleCursor)
                ),
                "an erased canonical group must expose no cached IDs"
            );
            continue;
        }
        prepare_upload(&store, &fresh);
        if let Some(event) = new_message {
            assert_eq!(
                store.history_upload_contains(&fresh, event).unwrap(),
                mutation == "append"
            );
        }
    }
}

#[tokio::test]
async fn ready_history_retention_incomplete_pending_unknown_read_failure_and_close_are_not_cached()
{
    for state in ["incomplete", "pending", "unknown", "read_failure", "closed"] {
        let (_directory, node, peer, conversation) = history_retention_fixture().await;
        let owner = node.sync_store(&peer.public());
        let mut store = owner.lock().unwrap();
        let upload = store.open_history_upload(&conversation).unwrap();
        if state != "incomplete" {
            prepare_upload(&store, &upload);
        }
        if state == "closed" {
            store.retain_history_upload(upload).unwrap();
            assert_eq!(node.prepared_sync_views.lock().unwrap().len(), 1);
            let work = node.runtime_work.admit().unwrap();
            node.runtime_work.close();
            let fresh = store.open_history_upload(&conversation).unwrap();
            assert!(
                !fresh.preparation_progress().2,
                "closed runtime cannot check out a parked plan"
            );
            assert_eq!(node.prepared_sync_views.lock().unwrap().len(), 1);
            assert_eq!(
                node.log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .active_selection_count(),
                0
            );
            drop(fresh);
            drop(node.take_prepared_sync_views());
            drop(work);
            continue;
        }
        match state {
            "pending" => {
                store
                    .pending_channels
                    .insert(conversation, super::super::PendingChannel::default());
            }
            "unknown" => {
                node.storage
                    .database()
                    .transaction(|c| {
                        c.execute_batch("DELETE FROM participant_kind_scan")?;
                        Ok(())
                    })
                    .unwrap();
            }
            "read_failure" => {
                node.storage.database().transaction(|c| {c.execute_batch("ALTER TABLE participant_semantic_compaction_revisions RENAME TO unavailable_compaction_classification")?; Ok(())}).unwrap();
            }
            _ => {}
        }
        let result = store.retain_history_upload(upload);
        if state == "read_failure" {
            assert!(
                result.is_err(),
                "actual missing SQL source must fail closed"
            );
        }
        assert_eq!(
            node.prepared_sync_views.lock().unwrap().len(),
            0,
            "state={state}: no unqualified plan may be parked"
        );
        if state == "read_failure" {
            node.storage.database().transaction(|c| {c.execute_batch("ALTER TABLE unavailable_compaction_classification RENAME TO participant_semantic_compaction_revisions")?; Ok(())}).unwrap();
            let fresh = store.open_history_upload(&conversation).unwrap();
            prepare_upload(&store, &fresh);
            store.retain_history_upload(fresh).unwrap();
            assert_eq!(
                node.prepared_sync_views.lock().unwrap().len(),
                1,
                "recovered SQL source may qualify a new ready plan"
            );
        }
    }
}
