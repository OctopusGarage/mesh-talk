use super::*;
use crate::{
    discovery::Roster,
    identity::{account::Account, device::DeviceIdentity},
};
use std::sync::Mutex;
fn open(dir: &Path, account: &Account) -> Arc<Node> {
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (ch, _) = tokio::sync::mpsc::unbounded_channel();
    let (file, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open_with_account(
        DeviceIdentity::from_secret_bytes([42; 32], [43; 32]),
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
    let history = node.account_history(&node.account_id(), 100);
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
            .into_iter()
            .cloned()
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
            accepted.items.iter().all(|item| item.status == "accepted"),
            "Every group forwarding item must be accepted"
        );
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
        while recipient.channel_history(group, 10).len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let history = recipient.channel_history(group, 10);
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
            .forward_messages_if(request, |commit| commit())
            .await
            .unwrap();
        assert!(
            merged.items[0].status == "accepted",
            "Merged group forwarding must be accepted"
        );
        let archive_id = merged.items[0].message_id.as_ref().unwrap();
        let archive = sender
            .channel_history(group, 10)
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
        while !recipient.file_ready_to_save(archive) {
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
    assert!(node.account_history(&node.account_id(), 100).is_empty());
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
