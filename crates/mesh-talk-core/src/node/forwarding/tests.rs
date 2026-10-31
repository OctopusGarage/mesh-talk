use super::*;
use crate::{
    discovery::Roster,
    identity::{account::Account, device::DeviceIdentity},
};
use std::sync::Mutex;
fn open(dir: &Path, account: &Account) -> Arc<Node> {
    open_device(
        dir,
        account,
        DeviceIdentity::from_secret_bytes([42; 32], [43; 32]),
    )
}
fn open_device(dir: &Path, account: &Account, identity: DeviceIdentity) -> Arc<Node> {
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (ch, _) = tokio::sync::mpsc::unbounded_channel();
    let (file, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open_with_account(
        identity,
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}
fn source(node: &Node) -> FavoriteSource {
    let peer = "ab".repeat(16);
    let conv = super::super::conversation::account_conversation_id(&node.account_id(), &peer);
    for (seq, id, text) in [(1, [1; 32], "first"), (2, [2; 32], "second")] {
        let env = super::super::DmEnvelope::new(
            node.account_id(),
            peer.clone(),
            id,
            super::super::MessageBody::new(text.as_bytes().to_vec(), None).encode(),
        );
        node.sentlog
            .lock()
            .unwrap()
            .record(conv, seq, seq, &env.encode())
            .unwrap();
    }
    FavoriteSource {
        kind: "account".into(),
        target: peer,
        message_ids: vec![hex::encode([2; 32]), hex::encode([1; 32])],
    }
}
#[tokio::test]
async fn canonical_forwarding_retry_restart_and_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let selected = source(&node);
    assert_eq!(
        node.inspect_forward_messages(&selected).unwrap().messages[0].text,
        "first"
    );
    let request = ForwardRequest {
        operation_id: format!("{}-test-operation", super::super::node::now_millis()),
        source: selected,
        destinations: vec![ForwardTarget {
            kind: "account".into(),
            target: node.account_id(),
        }],
        mode: "individual".into(),
    };
    let first = node
        .forward_messages_if(request.clone(), |c| c())
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.items.iter().all(|i| i.status == "accepted"));
    let repeated = node
        .forward_messages_if(request.clone(), |c| c())
        .await
        .unwrap();
    assert_eq!(first.items[0].message_id, repeated.items[0].message_id);
    drop(node);
    let node = open(dir.path(), &account);
    let replay = node
        .forward_messages_if(request.clone(), |c| c())
        .await
        .unwrap();
    assert_eq!(first.items[1].message_id, replay.items[1].message_id);
    let mut changed = request;
    changed.mode = "merged".into();
    assert!(matches!(
        node.forward_messages_if(changed, |c| c()).await,
        Err(NodeError::RequestConflict)
    ));
}
#[tokio::test]
async fn file_forward_has_fresh_crypto_and_merged_bytes_export() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let path = dir.path().join("note.txt");
    std::fs::write(&path, b"independent payload").unwrap();
    let (source_id, source_file) = node
        .enqueue_file_to_account_progress_expiring_if(
            &node.account_id(),
            &path,
            crate::file::FileKind::File,
            |_| {},
            None,
            |c| c(),
        )
        .await
        .unwrap();
    let selected = FavoriteSource {
        kind: "account".into(),
        target: node.account_id(),
        message_ids: vec![source_id.to_hex()],
    };
    let mut request = ForwardRequest {
        operation_id: format!("{}-file-op", super::super::node::now_millis()),
        source: selected.clone(),
        destinations: vec![ForwardTarget {
            kind: "account".into(),
            target: node.account_id(),
        }],
        mode: "individual".into(),
    };
    let sent = node
        .forward_messages_if(request.clone(), |c| c())
        .await
        .unwrap();
    assert!(
        sent.items[0].status == "accepted",
        "Individual file forwarding must be accepted"
    );
    let new_source = FavoriteSource {
        message_ids: vec![sent.items[0].message_id.clone().unwrap()],
        ..selected.clone()
    };
    let forwarded = node.inspect_forward_messages(&new_source).unwrap();
    let file = forwarded.messages[0].file.as_ref().unwrap();
    assert_ne!(file.file_conv, hex::encode(source_file.as_bytes()));
    assert_eq!(
        node.read_file(super::super::favorites::conversation(&file.file_conv).unwrap())
            .unwrap(),
        b"independent payload"
    );
    request.operation_id = format!("{}-merged-op", super::super::node::now_millis());
    request.mode = "merged".into();
    let merged = node.forward_messages_if(request, |c| c()).await.unwrap();
    let archive = node
        .inspect_forward_messages(&FavoriteSource {
            message_ids: vec![merged.items[0].message_id.clone().unwrap()],
            ..selected
        })
        .unwrap_err();
    assert!(archive.to_string().contains("Nested"));
    let history = node.account_history(&node.account_id(), 100).unwrap();
    let entry = history
        .iter()
        .find(|m| Some(m.id.to_hex()) == merged.items[0].message_id)
        .unwrap();
    let conv = entry.file.as_ref().unwrap().file_conv;
    assert_eq!(
        node.forwarded_attachment_bytes(&hex::encode(conv.as_bytes()), 0)
            .unwrap(),
        b"independent payload"
    );
}

#[tokio::test]
async fn partial_batch_restart_retries_failed_only_and_receives_over_loopback() {
    use crate::discovery::Announce;
    let dir = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let receiver_identity = DeviceIdentity::generate();
    let receiver_account = Account::generate();
    let receiver_id = receiver_account.account_id();
    let selected = source(&node);
    let request = ForwardRequest {
        operation_id: format!("{}-partial-loopback", super::super::node::now_millis()),
        source: selected,
        destinations: vec![
            ForwardTarget {
                kind: "account".into(),
                target: node.account_id(),
            },
            ForwardTarget {
                kind: "account".into(),
                target: receiver_id.clone(),
            },
        ],
        mode: "individual".into(),
    };
    let initial = node
        .forward_messages_if(request.clone(), |c| c())
        .await
        .unwrap();
    assert_eq!(
        initial
            .items
            .iter()
            .filter(|i| i.status == "accepted")
            .count(),
        2
    );
    assert_eq!(
        initial
            .items
            .iter()
            .filter(|i| i.status == "failed")
            .count(),
        2
    );
    let accepted = initial
        .items
        .iter()
        .filter(|i| i.status == "accepted")
        .map(|i| i.message_id.clone())
        .collect::<Vec<_>>();
    drop(node);
    let node = open(dir.path(), &account);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proof = Announce::new_with_account(
        &receiver_identity,
        &receiver_account,
        "Recipient",
        listener.local_addr().unwrap().port(),
    );
    let sender_proof = Announce::new_with_account(&node.identity, &account, "Forwarder", 1);
    let (receiver, mut received, _) = super::super::test_support::node_with_files(
        remote_dir.path(),
        receiver_identity,
        receiver_account,
        &sender_proof,
    );
    let server = tokio::spawn(receiver.clone().run_accept_loop(listener));
    node.roster
        .lock()
        .unwrap()
        .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
    let retried = node.forward_messages_if(request, |c| c()).await.unwrap();
    assert!(retried.items.iter().all(|i| i.status == "accepted"));
    assert_eq!(
        retried
            .items
            .iter()
            .filter(|i| i.target.target == node.account_id())
            .map(|i| i.message_id.clone())
            .collect::<Vec<_>>(),
        accepted
    );
    for item in retried
        .items
        .iter()
        .filter(|i| i.target.target == receiver_id)
    {
        let bytes: [u8; 32] = hex::decode(item.message_id.as_ref().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        node.flush_delivery(crate::eventlog::EventId::new(bytes))
            .await;
    }
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.text, b"first");
    assert_eq!(second.text, b"second");
    server.abort();
}

#[tokio::test]
async fn expired_operations_and_denied_admission_never_create_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let selected = source(&node);
    let mut request = ForwardRequest {
        operation_id: "0-expired".into(),
        source: selected,
        destinations: vec![ForwardTarget {
            kind: "account".into(),
            target: node.account_id(),
        }],
        mode: "individual".into(),
    };
    assert!(
        node.forward_messages_if(request.clone(), |c| c())
            .await
            .is_err()
    );
    assert!(node.list_forward_batches().is_empty());
    request.operation_id = format!("{}-denied", super::super::node::now_millis());
    assert!(
        node.forward_messages_if(request, |_| Err(NodeError::Authorization("denied".into())))
            .await
            .is_err()
    );
    assert!(node.list_forward_batches().is_empty());
}
#[test]
fn completed_batch_eviction_keeps_request_fingerprint_across_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forward.log");
    let now = super::super::node::now_millis();
    let mut store = ForwardStore::open(&path, "pw").unwrap();
    for index in 0..140 {
        let id = format!("{now}-{index:04}");
        let target = ForwardTarget {
            kind: "account".into(),
            target: "ab".repeat(16),
        };
        let request = ForwardRequest {
            operation_id: id.clone(),
            source: FavoriteSource {
                kind: "account".into(),
                target: "cd".repeat(16),
                message_ids: vec!["ef".repeat(32)],
            },
            destinations: vec![target.clone()],
            mode: "individual".into(),
        };
        let batch = ForwardBatch {
            request,
            result: ForwardResult {
                operation_id: id,
                items: vec![ForwardItem {
                    target,
                    source_id: Some("ef".repeat(32)),
                    message_id: Some("12".repeat(32)),
                    status: "accepted".into(),
                    error: None,
                }],
            },
        };
        store.save(&batch).unwrap();
    }
    assert_eq!(store.batches.len(), 128);
    assert_eq!(store.fingerprints.len(), 140);
    drop(store);
    let store = ForwardStore::open(&path, "pw").unwrap();
    assert_eq!(store.batches.len(), 128);
    assert_eq!(store.fingerprints.len(), 140);
    assert!(store.fingerprints.contains_key(&format!("{now}-0000")));
}

#[tokio::test]
async fn forwarding_to_group_delivers_ordered_text_and_independent_merged_file() {
    use crate::discovery::Announce;
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir().unwrap();
        let sender_account = Account::generate();
        let sender = open(&directory.path().join("sender"), &sender_account);
        let selected = source(&sender);
        let sender_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let recipient_identity = DeviceIdentity::generate();
        let recipient_account = Account::generate();
        let recipient_proof = Announce::new_with_account(
            &recipient_identity,
            &recipient_account,
            "Reader",
            listener.local_addr().unwrap().port(),
        );
        let sender_proof = Announce::new_with_account(
            &sender.identity,
            &sender_account,
            "Forwarder",
            sender_listener.local_addr().unwrap().port(),
        );
        let (recipient, _, mut offers) = super::super::test_support::node_with_files(
            &directory.path().join("recipient"),
            recipient_identity,
            recipient_account,
            &sender_proof,
        );
        sender.roster.lock().unwrap().update(
            &recipient_proof,
            "127.0.0.1".parse().unwrap(),
            &sender.user_id(),
        );
        let group = sender
            .create_channel("Forwarded discussion", vec![recipient_proof.public()])
            .await
            .unwrap();
        // Only signed membership controls are seeded; all forwarded content uses TCP.
        let controls = sender
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>();
        for event in controls {
            recipient.log.lock().unwrap().append_durable(event).unwrap();
        }
        recipient.process_channel(group);
        let sender_task = tokio::spawn(sender.clone().run_accept_loop(sender_listener));
        let recipient_task = tokio::spawn(recipient.clone().run_accept_loop(listener));
        let mut request = ForwardRequest {
            operation_id: format!("{}-group-text", super::super::node::now_millis()),
            source: selected,
            destinations: vec![ForwardTarget {
                kind: "channel".into(),
                target: hex::encode(group.as_bytes()),
            }],
            mode: "individual".into(),
        };
        let accepted = sender
            .forward_messages_if(request.clone(), |commit| commit())
            .await
            .unwrap();
        assert!(
            accepted
                .items
                .iter()
                .all(|item| item.status == "pending" || item.status == "accepted")
        );
        let accepted = wait_forward_receipts(&sender, &request).await;
        for item in &accepted.items {
            let id = crate::eventlog::EventId::new(
                hex::decode(item.message_id.as_ref().unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap(),
            );
            sender
                .flush_accepted_message(&MessageDestination::Channel(group), id)
                .await;
        }
        while recipient.channel_history(group, 10).unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let history = recipient.channel_history(group, 10).unwrap();
        assert_eq!(
            history
                .iter()
                .map(|row| row.text.clone())
                .collect::<Vec<_>>(),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
        request.operation_id = format!("{}-group-merged", super::super::node::now_millis());
        request.mode = "merged".into();
        let merged = sender
            .forward_messages_if(request.clone(), |commit| commit())
            .await
            .unwrap();
        assert!(merged.items[0].status == "pending" || merged.items[0].status == "accepted");
        let merged = wait_forward_receipts(&sender, &request).await;
        let archive_id = merged.items[0].message_id.as_ref().unwrap();
        let archive = sender
            .channel_history(group, 10)
            .unwrap()
            .into_iter()
            .find(|row| row.id.to_hex() == *archive_id)
            .unwrap()
            .file
            .unwrap()
            .file_conv;
        sender.flush_channel_file(group, archive).await;
        let offer = offers.recv().await.unwrap();
        assert_eq!(offer.name, "Chat records.mtchat");
        recipient.decide_attachment(archive, true).unwrap();
        while !recipient.file_ready_to_save(archive).unwrap() {
            recipient.pull_pending_files().await;
            sender.pull_pending_files().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let received = recipient.read_file(archive).unwrap();
        let record: serde_json::Value = serde_json::from_slice(&received).unwrap();
        assert_eq!(record["format"], "MTCHAT1");
        assert_eq!(record["messages"][0]["text"], "first");
        assert_eq!(record["messages"][1]["text"], "second");
        assert!(
            recipient
                .account_history(&sender.account_id(), 10)
                .unwrap()
                .is_empty(),
            "receiver never joined original direct conversation"
        );
        sender_task.abort();
        recipient_task.abort();
    })
    .await
    .expect("group forwarding completes on encrypted loopback");
}

#[tokio::test]
async fn recall_at_final_guard_prevents_forward_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let selected = source(&node);
    let conv =
        super::super::conversation::account_conversation_id(&node.account_id(), &selected.target);
    let request = ForwardRequest {
        operation_id: format!("{}-guard-recall", super::super::node::now_millis()),
        source: selected,
        destinations: vec![ForwardTarget {
            kind: "account".into(),
            target: node.account_id(),
        }],
        mode: "individual".into(),
    };
    let mut calls = 0;
    let result = node
        .forward_messages_if(request, |commit| {
            calls += 1;
            if calls == 6 {
                node.recalls
                    .lock()
                    .unwrap()
                    .record(
                        conv,
                        crate::eventlog::EventId::new([1; 32]),
                        super::super::node::now_millis(),
                    )
                    .unwrap();
            }
            commit()
        })
        .await
        .unwrap();
    assert!(calls >= 6);
    assert!(result.items.iter().all(|item| item.status == "failed"));
    assert!(
        node.account_history(&node.account_id(), 100)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn protected_and_expired_canonical_sources_are_not_inspectable() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let mut selected = source(&node);
    let conv =
        super::super::conversation::account_conversation_id(&node.account_id(), &selected.target);
    let protected = super::super::MessageBody::protected(
        b"secret".to_vec(),
        None,
        super::super::MessageProtection::Click,
    )
    .encode();
    let expired = crate::expiry::wrap(
        &super::super::MessageBody::new(b"expired".to_vec(), None).encode(),
        Some(1),
    );
    for (index, body) in [protected, expired].into_iter().enumerate() {
        let id = [(index + 3) as u8; 32];
        let envelope =
            super::super::DmEnvelope::new(node.account_id(), selected.target.clone(), id, body);
        node.sentlog
            .lock()
            .unwrap()
            .record(
                conv,
                (index + 3) as u64,
                (index + 3) as u64,
                &envelope.encode(),
            )
            .unwrap();
        selected.message_ids = vec![hex::encode(id)];
        assert!(node.inspect_forward_messages(&selected).is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reference_owner_forwards_with_one_retained_account_intent() {
    reference_forwarding(crate::node::AccountGroupRole::Owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reference_member_forwards_with_one_retained_account_intent() {
    reference_forwarding(crate::node::AccountGroupRole::Member).await;
}

async fn reference_forwarding(role: crate::node::AccountGroupRole) {
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        eventlog::{Event, EventKind},
        node::{AccountDevice, SyncCategory},
    };
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = open(&directory.path().join("executor"), &account);
    let reference = open_device(
        &directory.path().join("reference"),
        &account,
        DeviceIdentity::from_secret_bytes([52; 32], [53; 32]),
    );
    let outsider = open_device(
        &directory.path().join("outsider"),
        &Account::generate(),
        DeviceIdentity::generate(),
    );
    let owner = if role == crate::node::AccountGroupRole::Owner {
        &source
    } else {
        &outsider
    };
    let group = crate::channel::new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Account forwarding".into(),
            members: vec![source.identity.public(), outsider.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    source.log.lock().unwrap().append_durable(genesis).unwrap();
    source.process_channel(group);
    for (left, right) in [(&source, &reference), (&reference, &source)] {
        left.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: right.identity.public(),
                certificate: account.certify(&right.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let mut servers = tokio::task::JoinSet::new();
    let mut addresses = Vec::new();
    for (label, node) in [("executor", &source), ("reference", &reference)] {
        node.set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        node.configure_privacy(
            &directory.path().join(label),
            "pw",
            &node.signed_announce("Forwarding", address.port()),
            Arc::new(crate::discovery::DiscoveryVisibility::default()),
        )
        .unwrap();
        addresses.push(address);
        let node = node.clone();
        servers.spawn(async move {
            Box::pin(node.run_accept_loop(listener)).await;
        });
    }
    for (left, right, address) in [
        (&source, &reference, addresses[1]),
        (&reference, &source, addresses[0]),
    ] {
        left.roster.lock().unwrap().update(
            &Announce::new_with_account(&right.identity, &account, "Forwarding", address.port()),
            address.ip(),
            &left.user_id(),
        );
        left.account_sync_notify.notify_one();
    }
    source.publish_account_group_access_for(group);
    source.publish_account_group_operations_for(group);
    tokio::time::timeout(Duration::from_secs(30), async {
        while !reference
            .account_group_access(group)
            .is_ok_and(|access| access.role == role)
            || !reference.account_group_operation_ready_for(
                &source.identity.public(),
                AccountGroupOperationKind::Text,
            )
            || !reference.account_group_operation_ready_for(
                &source.identity.public(),
                AccountGroupOperationKind::File,
            )
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real authenticated account control must establish the reference role");
    assert!(reference.channels.lock().unwrap().state(&group).is_none());
    let selected = self::source(&reference);
    // Queue both real intents before allowing another private document read.
    // Choose the ordinary stable item IDs in reverse scheduler order so the
    // old independent-request publication deterministically exposes the bug.
    let reads = Arc::new(tokio::sync::Semaphore::new(0));
    struct ReleaseReads(Arc<tokio::sync::Semaphore>);
    impl Drop for ReleaseReads {
        fn drop(&mut self) {
            self.0.add_permits(16);
        }
    }
    let release_reads = ReleaseReads(reads.clone());
    for node in [&source, &reference] {
        *node.account_session_test_hook.lock().unwrap() = Some(Arc::new({
            let reads = reads.clone();
            move |_, frame| {
                let reads = reads.clone();
                Box::pin(async move {
                    if matches!(
                        frame,
                        crate::node::account_sync_wire::AccountHistoryFrame::DocumentsRequest { .. }
                    ) {
                        let _permit = reads.acquire().await.unwrap();
                    }
                })
            }
        }));
    }
    let target = ForwardTarget {
        kind: "channel".into(),
        target: hex::encode(group.as_bytes()),
    };
    let now = super::super::node::now_millis();
    let operation_id = (0..100)
        .map(|attempt| format!("{now}-reference-text-{attempt}"))
        .find(|operation| {
            item_identity(operation, &target, &Some(hex::encode([1; 32])))
                > item_identity(operation, &target, &Some(hex::encode([2; 32])))
        })
        .expect("a bounded deterministic ID search must reverse the old scheduler");
    let text_request = ForwardRequest {
        operation_id,
        source: selected.clone(),
        destinations: vec![ForwardTarget {
            kind: "channel".into(),
            target: hex::encode(group.as_bytes()),
        }],
        mode: "individual".into(),
    };
    let first = reference
        .forward_messages_if(text_request.clone(), |commit| commit())
        .await
        .unwrap();
    assert!(
        first
            .items
            .iter()
            .all(|item| item.status == "pending" || item.status == "accepted"),
        "logical forwarding must be pending or accepted"
    );
    let first_id = reference.forwarding_group_id(&item_identity(
        &text_request.operation_id,
        &target,
        &Some(hex::encode([1; 32])),
    ));
    let second_id = reference.forwarding_group_id(&item_identity(
        &text_request.operation_id,
        &target,
        &Some(hex::encode([2; 32])),
    ));
    for id in [first_id, second_id] {
        let status = reference
            .retained_account_group_operation_status(&reference.account_id(), id)
            .unwrap();
        assert!(
            status.as_ref().is_some_and(
                |status| status.phase == AccountGroupOperationPhase::AwaitingAcceptance
            ),
            "both gated real intents must await acceptance"
        );
    }
    assert_eq!(
        reference
            .operation_projection(group)
            .unwrap()
            .requests
            .iter()
            .map(|request| request.reference.id)
            .collect::<Vec<_>>(),
        vec![first_id],
        "only the first admitted forwarding leg may be published before its terminal result"
    );
    let published = reference
        .account_document(&crate::node::AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: reference.user_id(),
        })
        .unwrap()
        .unwrap();
    let frame = crate::node::account_group::operations::wire::OperationFrame::decode(
        published.payload(),
        &account.public().ed25519_pub,
    )
    .expect("actual stored publication retains its verified signature");
    assert_eq!(
        frame
            .projection
            .requests
            .iter()
            .map(|request| request.reference.id)
            .collect::<Vec<_>>(),
        vec![first_id],
        "passive document sync cannot export a later leg"
    );
    for node in [&source, &reference] {
        *node.account_session_test_hook.lock().unwrap() = None;
    }
    drop(release_reads);
    let text_result = wait_forward_receipts(&reference, &text_request).await;
    assert_eq!(
        source
            .channel_history(group, 10)
            .unwrap()
            .iter()
            .map(|row| row.text.clone())
            .collect::<Vec<_>>(),
        vec![b"first".to_vec(), b"second".to_vec()],
        "logical forwarding preserves the selected source order"
    );
    let text_replay = reference
        .forward_messages_if(text_request.clone(), |commit| commit())
        .await
        .unwrap();
    assert_eq!(text_replay, text_result);

    let path = directory.path().join("payload.txt");
    std::fs::write(&path, b"reference file payload").unwrap();
    let (file_message, original_file) = reference
        .enqueue_file_to_account_progress_expiring_if(
            &reference.account_id(),
            &path,
            crate::file::FileKind::File,
            |_| {},
            None,
            |commit| commit(),
        )
        .await
        .unwrap();
    let file_request = ForwardRequest {
        operation_id: format!("{}-reference-file", super::super::node::now_millis()),
        source: FavoriteSource {
            kind: "account".into(),
            target: reference.account_id(),
            message_ids: vec![file_message.to_hex()],
        },
        destinations: text_request.destinations.clone(),
        mode: "individual".into(),
    };
    let file_admission = reference
        .forward_messages_if(file_request.clone(), |commit| commit())
        .await
        .unwrap();
    assert!(
        file_admission
            .items
            .iter()
            .all(|item| item.status == "pending" || item.status == "accepted"),
        "file forwarding must be pending or accepted"
    );
    let file_result = wait_forward_receipts(&reference, &file_request).await;
    let event = crate::eventlog::EventId::new(
        hex::decode(file_result.items[0].message_id.as_ref().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let row = reference
        .received_files
        .lock()
        .unwrap()
        .entry(event)
        .unwrap()
        .unwrap()
        .clone();
    let manifest = crate::node::files::validated_manifest(&row.plaintext).unwrap();
    assert_ne!(
        manifest.file_conv(),
        original_file,
        "forwarding prepares fresh ciphertext identity"
    );
    assert_eq!(
        reference.read_file(manifest.file_conv()).unwrap(),
        b"reference file payload"
    );

    let merged_request = ForwardRequest {
        operation_id: format!("{}-reference-merged", super::super::node::now_millis()),
        source: selected.clone(),
        destinations: text_request.destinations.clone(),
        mode: "merged".into(),
    };
    let merged_admission = reference
        .forward_messages_if(merged_request.clone(), |commit| commit())
        .await
        .unwrap();
    assert!(
        merged_admission
            .items
            .iter()
            .all(|item| item.status == "pending" || item.status == "accepted"),
        "merged forwarding must be pending or accepted"
    );
    let merged_result = wait_forward_receipts(&reference, &merged_request).await;
    let count = source
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Message | EventKind::FileManifest))
        .count();
    let source_conversation = crate::node::conversation::account_conversation_id(
        &reference.account_id(),
        &selected.target,
    );
    reference
        .recalls
        .lock()
        .unwrap()
        .record(
            source_conversation,
            crate::eventlog::EventId::new([1; 32]),
            crate::node::node::now_millis(),
        )
        .unwrap();
    assert!(reference.inspect_forward_messages(&selected).is_err());
    assert_eq!(
        reference
            .list_forward_batches()
            .into_iter()
            .find(|batch| batch.request == merged_request)
            .unwrap()
            .result,
        merged_result,
        "observing an admitted operation never reopens recalled source contents"
    );
    for node in [&source, &reference] {
        node.runtime_work.close();
    }
    servers.shutdown().await;
    for node in [&source, &reference] {
        tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .unwrap();
    }
    assert_eq!(count, 4, "one original effect per text/file/merged leg");
    assert!(reference.channels.lock().unwrap().state(&group).is_none());
    drop(reference);
    let reopened = open_device(
        &directory.path().join("reference"),
        &account,
        DeviceIdentity::from_secret_bytes([52; 32], [53; 32]),
    );
    assert_eq!(
        reopened
            .forward_messages_if(merged_request, |commit| commit())
            .await
            .unwrap(),
        merged_result,
        "restart/replay reuses the committed operation even after source recall"
    );
    assert_eq!(
        source
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Message | EventKind::FileManifest))
            .count(),
        4
    );
}

#[tokio::test]
async fn unknown_group_leg_fails_without_network_admission_or_file_staging() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(directory.path(), &account);
    let foreign = DeviceIdentity::generate();
    let nonmember_group = crate::channel::new_channel_id();
    let genesis = crate::eventlog::Event::new(
        &foreign,
        nonmember_group,
        1,
        vec![],
        1,
        1,
        crate::eventlog::EventKind::MembershipChange,
        crate::channel::ChannelMeta {
            name: "Foreign forwarding target".into(),
            members: vec![foreign.public()],
            epoch: 0,
            owner: foreign.public().user_id(),
        }
        .encode(),
    );
    node.log.lock().unwrap().append_durable(genesis).unwrap();
    node.process_channel(nonmember_group);
    for (label, target) in [
        ("unknown", "fe".repeat(32)),
        ("nonmember", hex::encode(nonmember_group.as_bytes())),
    ] {
        let request = ForwardRequest {
            operation_id: format!("{}-{label}-group", super::super::node::now_millis()),
            source: source(&node),
            destinations: vec![ForwardTarget {
                kind: "channel".into(),
                target,
            }],
            mode: "merged".into(),
        };
        let before = node.log.lock().unwrap().all_event_ids().unwrap();
        let identity = item_identity(&request.operation_id, &request.destinations[0], &None);
        let result = node
            .forward_messages_if(request, |commit| commit())
            .await
            .unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].status, "failed", "{label}");
        assert!(
            node.retained_account_group_operation_status(
                &node.account_id(),
                node.forwarding_group_id(&identity)
            )
            .unwrap()
            .is_none(),
            "{label} must not admit a network operation"
        );
        assert_eq!(
            node.log.lock().unwrap().all_event_ids().unwrap(),
            before,
            "no ciphertext staging or outgoing effect for the {label} leg"
        );
    }
}

async fn wait_forward_receipts(node: &Node, request: &ForwardRequest) -> ForwardResult {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(batch) = node
                .list_forward_batches()
                .into_iter()
                .find(|batch| batch.request == *request)
                && batch
                    .result
                    .items
                    .iter()
                    .all(|item| item.status == "accepted" && item.message_id.is_some())
            {
                return batch.result;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "forwarding must expose verified committed receipts: {error:?}; batches={:?}",
            node.list_forward_batches()
        )
    })
}

#[tokio::test]
async fn unauthorized_group_leg_preserves_an_accepted_account_leg() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(directory.path(), &account);
    let request = ForwardRequest {
        operation_id: format!("{}-mixed-group", super::super::node::now_millis()),
        source: source(&node),
        destinations: vec![
            ForwardTarget {
                kind: "channel".into(),
                target: "fd".repeat(32),
            },
            ForwardTarget {
                kind: "account".into(),
                target: node.account_id(),
            },
        ],
        mode: "individual".into(),
    };
    let result = node
        .forward_messages_if(request.clone(), |commit| commit())
        .await
        .unwrap();
    assert_eq!(
        result
            .items
            .iter()
            .filter(|item| item.status == "accepted")
            .count(),
        2
    );
    assert_eq!(
        result
            .items
            .iter()
            .filter(|item| item.status == "failed")
            .count(),
        2
    );
    assert_eq!(
        node.account_history(&node.account_id(), 100).unwrap().len(),
        2
    );
    let replay = node
        .forward_messages_if(request, |commit| commit())
        .await
        .unwrap();
    let by_identity = |result: ForwardResult| {
        let mut items = result.items;
        items.sort_by(|a, b| {
            (&a.target.kind, &a.target.target, &a.source_id).cmp(&(
                &b.target.kind,
                &b.target.target,
                &b.source_id,
            ))
        });
        items
    };
    assert_eq!(by_identity(replay), by_identity(result));
    assert_eq!(
        node.account_history(&node.account_id(), 100).unwrap().len(),
        2
    );
}

#[tokio::test]
async fn pending_forwarding_order_survives_restart_and_cancelled_predecessor() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(directory.path(), &account);
    let group = node
        .create_channel("Ordered forwarding", vec![])
        .await
        .unwrap();
    let target = ForwardTarget {
        kind: "channel".into(),
        target: hex::encode(group.as_bytes()),
    };
    let request = ForwardRequest {
        operation_id: format!("{}-pending-order", super::super::node::now_millis()),
        source: source(&node),
        destinations: vec![target.clone()],
        mode: "individual".into(),
    };
    let mut batch = ForwardBatch {
        request,
        result: ForwardResult {
            operation_id: String::new(),
            items: Vec::new(),
        },
    };
    batch.result.operation_id = batch.request.operation_id.clone();
    let mut ids = Vec::new();
    // Exercise the existing progress/journal boundary with real admitted sends,
    // leaving source execution parked until both encrypted stores reopen.
    for (index, text) in [(1, "first"), (2, "second")] {
        let source_id = Some(hex::encode([index; 32]));
        let id = node.forwarding_group_id(&item_identity(
            &batch.request.operation_id,
            &target,
            &source_id,
        ));
        let status = node
            .submit_account_group_text_if(
                &node.account_id(),
                id,
                group,
                AccountGroupTextInput {
                    text: text.as_bytes().to_vec(),
                    reply_to: None,
                    expires_at_ms: None,
                    protection: None,
                },
                |commit| commit(),
            )
            .unwrap();
        let mut item = ForwardItem {
            target: target.clone(),
            source_id,
            message_id: None,
            status: String::new(),
            error: None,
        };
        apply_group_status(&mut item, &status);
        batch.result.items.push(item);
        ids.push(id);
    }
    node.save_forward_progress(&batch).unwrap();
    drop(node);
    let reopened = open(directory.path(), &account);
    let exported = || {
        reopened
            .operation_projection(group)
            .unwrap()
            .requests
            .into_iter()
            .map(|request| request.reference.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        exported(),
        vec![ids[0]],
        "restart preserves the pending publication fence"
    );
    reopened
        .cancel_account_group_operation(&reopened.account_id(), ids[0])
        .unwrap();
    reopened.maintain_account_group_operation_effects();
    assert!(
        reopened
            .retained_account_group_operation_status(&reopened.account_id(), ids[0])
            .unwrap()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::Cancelled)
    );
    assert_eq!(
        exported(),
        vec![ids[1]],
        "terminal cancellation releases the following leg"
    );
    reopened.maintain_account_group_operation_effects();
    assert_eq!(
        reopened
            .channel_history(group, 10)
            .unwrap()
            .iter()
            .map(|row| row.text.clone())
            .collect::<Vec<_>>(),
        vec![b"second".to_vec()]
    );
}

#[tokio::test]
async fn committed_group_progress_frees_batch_budget_and_survives_restart() {
    use crate::{
        channel::ChannelMeta,
        eventlog::{Event, EventKind},
    };
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(directory.path(), &account);
    // Exercise the unchanged production retirement branch with fewer real WAL
    // effects; the separate 128-record store regression retains the full bound.
    node.forwarding.lock().unwrap().batch_limit = 2;
    let group = crate::channel::new_channel_id();
    let genesis = Event::new(
        &node.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Retirement".into(),
            members: vec![node.identity.public(), DeviceIdentity::generate().public()],
            epoch: 0,
            owner: node.user_id(),
        }
        .encode(),
    );
    node.log.lock().unwrap().append_durable(genesis).unwrap();
    node.process_channel(group);
    let mut selected = source(&node);
    selected.message_ids.truncate(1);
    let mut requests = Vec::new();
    for index in 0..3 {
        let request = ForwardRequest {
            operation_id: format!("{}-retirement-{index}", super::super::node::now_millis()),
            source: selected.clone(),
            destinations: vec![ForwardTarget {
                kind: "channel".into(),
                target: hex::encode(group.as_bytes()),
            }],
            mode: "merged".into(),
        };
        let result = node
            .forward_messages_if(request.clone(), |commit| commit())
            .await
            .unwrap();
        assert_eq!(result.items[0].status, "pending");
        node.maintain_account_group_operation_effects();
        let path = directory
            .path()
            .join(format!("forwarding-{}.log", node.account_id()));
        let before = std::fs::metadata(&path).unwrap().len();
        assert_eq!(
            node.list_forward_batches()
                .into_iter()
                .find(|batch| batch.request == request)
                .unwrap()
                .result
                .items[0]
                .status,
            "accepted"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            before,
            "list is a read-only projection"
        );
        requests.push(request);
    }
    assert_eq!(
        node.list_forward_batches().len(),
        2,
        "verified committed progress must permit retiring the oldest batch"
    );
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .iter()
            .filter(|event| event.kind == EventKind::FileManifest)
            .count(),
        3
    );
    let last = requests.pop().unwrap();
    drop(node);
    let reopened = open(directory.path(), &account);
    assert_eq!(
        reopened
            .forward_messages_if(last, |commit| commit())
            .await
            .unwrap()
            .items[0]
            .status,
        "accepted"
    );
    assert_eq!(
        reopened
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .iter()
            .filter(|event| event.kind == EventKind::FileManifest)
            .count(),
        3
    );
    assert!(
        reopened
            .forward_messages_if(requests.remove(0), |commit| commit())
            .await
            .is_err(),
        "retired batch identity cannot become a new send"
    );
}
