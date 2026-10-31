use super::*;
use crate::node::account_sync::selection_tests::{sibling_nodes, sibling_peer};
use tokio::net::TcpListener;

async fn pull_real_group_head(receiver: &Arc<Node>, peer: &PeerRecord, group: ConversationId) {
    let mut cursor = crate::node::account_documents::sync::DocumentSyncCursor::default();
    for _ in 0..12 {
        receiver
            .pull_account_documents_page(peer, &mut cursor)
            .await
            .unwrap();
        if receiver
            .account_group_access(group)
            .is_ok_and(|access| access.role == crate::node::AccountGroupRole::Owner)
        {
            return;
        }
    }
    panic!("the authenticated mandatory exchange must deliver the real owner head");
}

#[tokio::test]
async fn certified_reference_receives_owner_history_and_attachment_without_device_membership() {
    let (dir, source, receiver) = sibling_nodes();
    for node in [&source, &receiver] {
        node.set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
    }
    // This invited device never starts a listener or accepts. The requesting
    // sibling is a different device and has no physical channel membership.
    let offline = crate::identity::device::DeviceIdentity::generate();
    let group = source
        .create_channel_invited("account private history", vec![offline.public()])
        .await
        .unwrap();
    let text = b"preserve the original authored identity";
    let message = source
        .enqueue_channel_message_reply_expiring_if(group, text, None, None, |commit| commit())
        .unwrap();
    let contents = b"private reference attachment bytes";
    let path = dir.path().join("reference.bin");
    std::fs::write(&path, contents).unwrap();
    let (file_event, file) = source
        .enqueue_file_channel_progress_expiring_if(
            group,
            &path,
            crate::file::FileKind::File,
            |_| {},
            None,
            |commit| commit(),
        )
        .await
        .unwrap();
    source.publish_account_group_access(&mut None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    sibling_peer(&source, &receiver, "127.0.0.1:9".parse().unwrap());
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    pull_real_group_head(&receiver, &peer, group).await;
    assert!(receiver.channel_members(group).is_empty());
    assert!(
        receiver
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .is_empty()
    );

    let result = receiver
        .pull_account_history_page(&peer, None, [0; 32])
        .await;
    // Release the listener even when an import assertion fails.
    if let Err(error) = &result {
        server.abort();
        panic!("certified private history import failed: {error}");
    }
    let history = receiver.history(group, 10).unwrap();
    let imported = history.iter().find(|entry| entry.id == message);
    assert!(
        imported.is_some(),
        "an accepted source owner entitles its certified reference sibling"
    );
    let imported = imported.unwrap();
    assert_eq!(imported.text, text);
    assert!(imported.from_me);
    {
        let received = receiver.received.lock().unwrap();
        let original = received.entry_in(&group, message).unwrap().unwrap();
        assert_eq!(original.from, source.user_id());
    }
    assert!(history.iter().any(|entry| entry.id == file_event));
    assert!(receiver.channel_members(group).is_empty());
    receiver
        .pull_account_attachment(&peer, file, AccountSyncVersion::V2)
        .await
        .unwrap();
    let saved = dir.path().join("received-reference.bin");
    receiver.save_directory_archive(file, &saved).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), contents);
    assert!(receiver.channel_members(group).is_empty());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cached_private_channel_archive_rechecks_signed_sharing_and_dissolution() {
    let (_dir, source, receiver) = sibling_nodes();
    let group = source
        .create_channel("current source authority", vec![])
        .await
        .unwrap();
    let message = source
        .enqueue_channel_message_reply_expiring_if(
            group,
            b"retained private history",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    let contains = || {
        source.export_sync_records(&receiver.identity.public(), None, true, false, AccountSyncVersion::V2)
            .0.iter().any(|record| matches!(record, HistoryRecord::Channel(row) if row.event_id == *message.as_bytes()))
    };
    assert!(
        contains(),
        "populate the selector while the accepted source shares"
    );
    source
        .set_channel_replica_enabled(group, false)
        .await
        .unwrap();
    assert!(
        !contains(),
        "a signed sharing change invalidates cached private selectors"
    );
    source
        .set_channel_replica_enabled(group, true)
        .await
        .unwrap();
    assert!(contains());
    // Admit the actual signed owner control without refreshing ChannelBook: an
    // accepted cached role must not outlive the authoritative public event.
    source
        .append_event(
            group,
            crate::eventlog::EventKind::ChannelDissolve,
            Vec::new(),
        )
        .unwrap();
    assert!(
        !source
            .channels
            .lock()
            .unwrap()
            .state(&group)
            .unwrap()
            .is_dissolved()
    );
    let body_reads = || {
        (
            source.sentlog.lock().unwrap().body_reads(),
            source.received.lock().unwrap().body_reads(),
            source.received_files.lock().unwrap().body_reads(),
        )
    };
    let before = body_reads();
    assert!(matches!(
        source.prepare_account_export(
            &receiver.identity.public(),
            None,
            true,
            false,
            AccountSyncVersion::V2,
        ),
        Err(NodeError::Log(LogError::StaleCursor))
    ));
    assert_eq!(
        body_reads(),
        before,
        "unprepared dissolution must refuse the cached archive before reading bodies"
    );
    settle_read_checkpoint(&source, group);
    assert!(
        !contains(),
        "a terminal public control revokes the retained source archive"
    );
}

#[tokio::test]
async fn history_off_reference_receives_mandatory_role_without_private_content() {
    let (_dir, source, receiver) = sibling_nodes();
    let group = source
        .create_channel("mandatory role only", vec![])
        .await
        .unwrap();
    source
        .enqueue_channel_message_reply_expiring_if(
            group,
            b"must remain private",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    for node in [&source, &receiver] {
        node.set_device_sync_category(SyncCategory::History, false)
            .unwrap();
    }
    source.publish_account_group_access(&mut None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    pull_real_group_head(&receiver, &peer, group).await;
    receiver
        .pull_account_history_page(&peer, None, [0; 32])
        .await
        .unwrap();
    assert!(receiver.history(group, 10).unwrap().is_empty());
    assert!(receiver.channel_members(group).is_empty());
    assert!(
        receiver
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .is_empty()
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn recalled_channel_content_is_not_disclosed_from_a_cached_private_archive() {
    let (dir, source, receiver) = sibling_nodes();
    source
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    let group = source
        .create_channel("recall source boundary", vec![])
        .await
        .unwrap();
    let message = source
        .enqueue_channel_message_reply_expiring_if(
            group,
            b"recall before copying",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    let image = dir.path().join("recalled-channel.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nprivate recalled channel image").unwrap();
    let (file_event, file) = source
        .enqueue_protected_file_channel_progress_expiring_if(
            group,
            &image,
            crate::file::FileKind::Media,
            |_| {},
            None,
            crate::protection::MessageProtection::Click,
            |commit| commit(),
        )
        .await
        .unwrap();
    let exported = || {
        source
            .export_sync_records(
                &receiver.identity.public(),
                None,
                true,
                true,
                AccountSyncVersion::V2,
            )
            .0
    };
    let before = exported();
    assert!(before.iter().any(|record| matches!(record, HistoryRecord::Channel(row) if row.event_id == *message.as_bytes())));
    assert!(before.iter().any(|record| matches!(record, HistoryRecord::FileCard(row) if row.event_id == *file_event.as_bytes())));
    assert!(
        source
            .attachment_for_sibling(file, &receiver.identity.public())
            .unwrap()
            .is_some()
    );

    source.recall_channel(group, message).await.unwrap();
    source.recall_channel(group, file_event).await.unwrap();
    settle_read_checkpoint(&source, group);
    source.process_channel_recalls(group).unwrap();
    let after = exported();
    assert!(!after.iter().any(|record| matches!(record, HistoryRecord::Channel(row) if row.event_id == *message.as_bytes())), "a canonical recall must invalidate the cached plaintext selector");
    assert!(!after.iter().any(|record| matches!(record, HistoryRecord::FileCard(row) if row.event_id == *file_event.as_bytes())), "a recalled protected manifest must not be copied to a new reference");
    assert!(
        source
            .attachment_for_sibling(file, &receiver.identity.public())
            .unwrap()
            .is_none(),
        "recalled source bytes must not remain available through the private attachment path"
    );
    // This intentionally proves no new disclosure. Existing reference copies
    // require the separate private group-recall control bridge to tombstone them.
}

#[tokio::test]
async fn retired_account_source_cannot_stream_private_group_history_after_adoption() {
    let (_dir, source, receiver) = sibling_nodes();
    let group = source
        .create_channel("retired private source", vec![])
        .await
        .unwrap();
    source
        .enqueue_channel_message_reply_expiring_if(
            group,
            b"old account source must retire",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    source.publish_account_group_access(&mut None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    sibling_peer(&source, &receiver, "127.0.0.1:9".parse().unwrap());
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    pull_real_group_head(&receiver, &peer, group).await;
    receiver
        .pull_account_history_page(&peer, None, [0; 32])
        .await
        .expect("the current source must actually deliver a private page before adoption");
    assert_eq!(receiver.history(group, 10).unwrap().len(), 1);
    let destination = crate::identity::account::Account::generate();
    source
        .persist_account_adoption(
            &destination.public(),
            || Ok(()),
            || Ok(destination.public()),
        )
        .unwrap();
    let result = receiver
        .pull_account_history_page(&peer, None, [0; 32])
        .await;
    server.abort();
    let _ = server.await;
    assert!(
        result.is_err(),
        "a retired old-account node cannot continue serving its private account stream"
    );
    assert_eq!(receiver.history(group, 10).unwrap().len(), 1);
}

#[tokio::test]
async fn channel_recall_invalidates_an_already_selected_private_plaintext_frame() {
    let (_dir, source, receiver) = sibling_nodes();
    let group = source
        .create_channel("recall a selected page", vec![])
        .await
        .unwrap();
    let target = source
        .enqueue_channel_message_reply_expiring_if(
            group,
            b"selected before recall",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    let page = source
        .prepare_account_export(
            &receiver.identity.public(),
            None,
            true,
            false,
            AccountSyncVersion::V2,
        )
        .unwrap();
    assert!(page.0.iter().any(
        |row| matches!(row, HistoryRecord::Channel(record) if record.event_id == *target.as_bytes())
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    sibling_peer(&source, &receiver, "127.0.0.1:9".parse().unwrap());
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let serving = source.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = serving.privacy_accept(stream).await.unwrap();
        serving
            .send_account_history_frame(
                &mut channel,
                AccountSyncVersion::V2,
                serving.device_sync_policy_snapshot().1,
                page.3,
                &AccountHistoryFrame::RecordChunk {
                    bytes: b"permitted before recall".to_vec(),
                },
            )
            .await
            .expect("the identical selected snapshot must permit its first frame before recall");
        release_rx.await.unwrap();
        serving
            .send_account_history_frame(
                &mut channel,
                AccountSyncVersion::V2,
                serving.device_sync_policy_snapshot().1,
                page.3,
                &AccountHistoryFrame::RecordChunk {
                    bytes: b"must not disclose a recalled prepared chunk".to_vec(),
                },
            )
            .await
    });
    let mut channel = receiver
        .privacy_dial(peer.addr, &peer.public)
        .await
        .unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), channel.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(AccountHistoryFrame::decode_versioned(&first), Some((AccountSyncVersion::V2, AccountHistoryFrame::RecordChunk { bytes })) if bytes == b"permitted before recall")
    );
    source.recall_channel(group, target).await.unwrap();
    release.send(()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), channel.recv())
            .await
            .unwrap()
            .is_err(),
        "a recall after page selection must stop the next plaintext frame"
    );
    assert!(server.await.unwrap().is_err());
}

async fn private_stream_stops_after_its_monotonic_deadline(attachment: bool, alias: bool) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (dir, source, receiver) = sibling_nodes();
    for node in [&source, &receiver] {
        node.set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
    }
    let group = source
        .create_channel("deadline boundary", vec![])
        .await
        .unwrap();
    let deadline = super::super::super::node::now_millis() + 60_000;
    let content = vec![b'x'; MAX_CHUNK * 3];
    let (event, file) = if attachment {
        let path = dir.path().join("expires-during-stream.bin");
        std::fs::write(&path, &content).unwrap();
        let (event, file) = source
            .enqueue_file_channel_progress_expiring_if(
                group,
                &path,
                crate::file::FileKind::File,
                |_| {},
                Some(deadline),
                |commit| commit(),
            )
            .await
            .unwrap();
        (event, Some(file))
    } else {
        (
            source
                .enqueue_channel_message_reply_expiring_if(
                    group,
                    &content,
                    None,
                    Some(deadline),
                    |commit| commit(),
                )
                .unwrap(),
            None,
        )
    };
    if alias {
        let original = source
            .received_files
            .lock()
            .unwrap()
            .entries_ref(&group)
            .iter()
            .find(|row| row.event_id == event)
            .unwrap()
            .plaintext
            .clone();
        let (_, manifest) = crate::expiry::split(&original).unwrap();
        let body = crate::expiry::wrap(manifest, Some(deadline + 60_000));
        let alias_event = source
            .enqueue_channel_payload_if(
                group,
                body,
                crate::eventlog::EventKind::FileManifest,
                None,
                &mut |commit| commit(),
            )
            .unwrap();
        assert_ne!(alias_event, event);
        assert!(
            source
                .received_files
                .lock()
                .unwrap()
                .entries_ref(&group)
                .iter()
                .any(|row| row.event_id == alias_event)
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    sibling_peer(&source, &receiver, "127.0.0.1:9".parse().unwrap());
    let (blocked, blocked_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let blocked = Arc::new(std::sync::Mutex::new(Some(blocked)));
    let release_rx = Arc::new(std::sync::Mutex::new(Some(release_rx)));
    let chunks = Arc::new(AtomicUsize::new(0));
    *source.account_session_test_hook.lock().unwrap() = Some(Arc::new(move |_, frame| {
        let blocked = blocked.clone();
        let release_rx = release_rx.clone();
        let chunks = chunks.clone();
        Box::pin(async move {
            let matching = if attachment {
                matches!(frame, AccountHistoryFrame::AttachmentChunk { .. })
            } else {
                matches!(frame, AccountHistoryFrame::RecordChunk { .. })
            };
            if matching && chunks.fetch_add(1, Ordering::SeqCst) == 1 {
                blocked.lock().unwrap().take().unwrap().send(()).unwrap();
                let released = release_rx.lock().unwrap().take().unwrap();
                released.await.unwrap();
            }
        })
    }));
    let mut tasks = tokio::task::JoinSet::new();
    let serving = source.clone();
    tasks.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                incoming = listener.accept() => {
                    let (stream, _) = incoming.unwrap();
                    let node = serving.clone();
                    connections.spawn(async move {
                        let channel = node.privacy_accept(stream).await.unwrap();
                        node.serve_connection(channel).await;
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let request = if let Some(file) = file {
        AccountHistoryFrame::AttachmentRequestChecked {
            account_id: receiver.account_id(),
            file_conv: *file.as_bytes(),
        }
    } else {
        AccountHistoryFrame::Request {
            version: 2,
            account_id: receiver.account_id(),
            cursor: None,
            known_digest: [0; 32],
            wanted: DeviceSyncPolicy {
                history: true,
                attachments: false,
                ..DeviceSyncPolicy::default()
            },
        }
    };
    let mut channel = receiver
        .begin_account_exchange(&peer, AccountSyncVersion::V2, request)
        .await
        .unwrap();
    loop {
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(3), channel.recv())
            .await
            .unwrap()
            .unwrap();
        let (_, frame) = AccountHistoryFrame::decode_versioned(&bytes).unwrap();
        if if attachment {
            matches!(frame, AccountHistoryFrame::AttachmentChunk { .. })
        } else {
            matches!(frame, AccountHistoryFrame::RecordChunk { .. })
        } {
            break;
        }
    }
    blocked_rx.await.unwrap();
    // Advance only this event's existing monotonic anti-rollback lifetime. The
    // authenticated deadline is unchanged, and the first chunk already passed.
    source.expiry_first_seen.lock().unwrap().insert(
        event,
        std::time::Instant::now() - std::time::Duration::from_secs(120),
    );
    let (wire, wall) = if attachment {
        let rows = source.received_files.lock().unwrap();
        let row = rows.entry_in(&group, event).unwrap().unwrap();
        (row.plaintext.clone(), row.wall_clock)
    } else {
        let wall = source
            .log
            .lock()
            .unwrap()
            .get(&event)
            .unwrap()
            .unwrap()
            .wall_clock;
        (
            crate::expiry::wrap(
                &crate::node::message::MessageBody::new(content, None).encode(),
                Some(deadline),
            ),
            wall,
        )
    };
    assert!(source.recipient_expired(&wire, event, wall));
    if alias {
        assert!(
            source
                .attachment_for_sibling(file.unwrap(), &receiver.identity.public())
                .unwrap()
                .is_some(),
            "another authorized live manifest remains available, but cannot replace this stream's original context"
        );
    }
    release.send(()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), channel.recv())
            .await
            .unwrap()
            .is_err(),
        "a selected private stream must stop before its next expired plaintext chunk"
    );
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

#[tokio::test]
async fn selected_private_history_stops_at_monotonic_expiry() {
    private_stream_stops_after_its_monotonic_deadline(false, false).await;
}

#[tokio::test]
async fn staged_private_attachment_stops_at_monotonic_expiry() {
    private_stream_stops_after_its_monotonic_deadline(true, false).await;
}

#[tokio::test]
async fn staged_attachment_cannot_replace_its_expired_original_with_a_live_alias() {
    private_stream_stops_after_its_monotonic_deadline(true, true).await;
}

fn settle_read_checkpoint(node: &Node, channel: ConversationId) {
    for _ in 0..256 {
        if matches!(
            node.channel_checkpoints.source_binding(channel),
            Ok(Some(_))
        ) {
            return;
        }
        let log = node.log.lock().unwrap();
        node.channel_checkpoints
            .project_step(log.repository().unwrap(), &node.identity, channel)
            .unwrap();
    }
    panic!("finite channel checkpoint did not settle");
}

#[tokio::test]
async fn selected_history_and_archive_wait_for_admitted_recall_before_body_reads() {
    let directory = tempfile::tempdir().unwrap();
    let open = |name: &str| {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let profile = directory.path().join(name);
        std::fs::create_dir(&profile).unwrap();
        Node::open(
            crate::identity::device::DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            channel,
            file,
            &profile.join("events"),
            &profile.join("sent"),
            "pw",
        )
        .unwrap()
    };
    let sender = open("sender");
    let source = open("receiver");
    let channel = sender
        .create_channel("read readiness", vec![source.identity.public()])
        .await
        .unwrap();
    let target = sender
        .enqueue_channel_message_reply_expiring_if(
            channel,
            b"withhold recalled plaintext",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    for event in sender.log.lock().unwrap().events(&channel).unwrap() {
        source.log.lock().unwrap().append(event).unwrap();
    }
    // This bare receiver has no recovery worker. Complete only the initial
    // injected message before beginning the unapplied-recall privacy boundary.
    for _ in 0..256 {
        source
            .process_channel_reception_inner(channel, "live", false)
            .unwrap();
        if source
            .received
            .lock()
            .unwrap()
            .contains_in(&channel, target)
            .unwrap()
        {
            break;
        }
    }
    assert!(
        source
            .history(channel, 10)
            .unwrap()
            .iter()
            .any(|row| row.id == target && !row.recalled)
    );
    let row = ArchiveRow {
        key: HistoryCursor {
            tag: 1,
            id: *target.as_bytes(),
        },
        kind: RECEIVED,
        ordinal: source
            .received
            .lock()
            .unwrap()
            .header_scan_page(Some(&channel), None, TranscriptPageLimit::default())
            .unwrap()
            .rows[0]
            .ordinal,
        conversation: channel,
        peer: None,
        charge: 4096,
    };
    assert!(source.materialize_archive_row(&row).unwrap().is_some());
    let captured = {
        let _log = source.log.lock().unwrap();
        source.capture_history_read_source(channel).unwrap()
    };
    sender.recall_channel(channel, target).await.unwrap();
    let recall = sender
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == crate::eventlog::EventKind::Delete)
        .unwrap();
    source.log.lock().unwrap().append(recall).unwrap();
    settle_read_checkpoint(&source, channel);
    assert!(
        !source
            .channel_checkpoints
            .history_read_ready(channel)
            .unwrap()
    );
    // Presentation state may not yet exist during startup; retained signed
    // membership must still classify this as a channel.
    *source.channels.lock().unwrap() = crate::node::channel::ChannelBook::new();
    let reads = source.received.lock().unwrap().body_reads();
    assert!(matches!(
        source.history(channel, 10),
        Err(NodeError::Log(LogError::StaleCursor))
    ));
    assert!(matches!(
        source.materialize_archive_row(&row),
        Err(NodeError::Log(LogError::StaleCursor))
    ));
    assert_eq!(
        source.received.lock().unwrap().body_reads(),
        reads,
        "pending recall hydrated retained plaintext"
    );
    source.process_channel_recalls(channel).unwrap();
    assert!(
        source
            .channel_checkpoints
            .history_read_ready(channel)
            .unwrap()
    );
    // Ready may have returned again after a concurrent Delete fully completed.
    // The original source, not just a fresh readiness boolean, fences the read.
    assert!(matches!(
        source.check_history_read_source(channel, captured.as_ref()),
        Err(LogError::StaleCursor)
    ));
    let history = source.history(channel, 10).unwrap();
    let recalled = history.iter().find(|row| row.id == target).unwrap();
    assert!(recalled.recalled);
    assert!(recalled.text.is_empty());
    assert!(source.materialize_archive_row(&row).unwrap().is_none());
}

#[tokio::test]
async fn retained_channel_without_checkpoint_cannot_publish_saved_plaintext() {
    let (_dir, source, receiver) = sibling_nodes();
    let channel = source
        .create_account_channel_invited_if("pending initial projection", vec![], |commit| commit())
        .await
        .unwrap();
    let target = source
        .enqueue_channel_message_reply_expiring_if(
            channel,
            b"wait for authority",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    let sent = source
        .sentlog
        .lock()
        .unwrap()
        .entries(&channel)
        .pop()
        .unwrap();
    for event in source.log.lock().unwrap().events(&channel).unwrap() {
        receiver.log.lock().unwrap().append(event).unwrap();
    }
    receiver
        .received
        .lock()
        .unwrap()
        .record(
            channel,
            source.user_id(),
            sent.wall_clock,
            &sent.plaintext,
            target,
        )
        .unwrap();
    assert!(receiver.channels.lock().unwrap().state(&channel).is_none());
    assert!(
        receiver
            .channel_checkpoints
            .source_binding(channel)
            .unwrap()
            .is_none()
    );
    let reads = receiver.received.lock().unwrap().body_reads();
    assert!(matches!(
        receiver.history(channel, 10),
        Err(NodeError::Log(LogError::StaleCursor))
    ));
    assert_eq!(receiver.received.lock().unwrap().body_reads(), reads);
}
