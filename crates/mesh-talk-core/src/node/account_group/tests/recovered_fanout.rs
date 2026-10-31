//! A real crash/reopen must publish its exact group before public distribution.
use super::*;
use crate::node::{AccountDocumentKey, GroupInventoryReference};
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

fn inventory(node: &Node, group: ConversationId) -> GroupInventoryReference {
    let document = node
        .account_document(&AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: node.user_id(),
        })
        .unwrap()
        .unwrap();
    serde_json::from_slice(document.payload()).unwrap()
}

fn reopen(path: &Path, account: &Account, identity: DeviceIdentity) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        identity,
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        files,
        &path.join("events.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovered_group_beyond_publication_slice_refreshes_before_exact_network_fanout() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let path = dir.path().join("owner");
    let owner = node(&path, &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    // The affected group is after the first sixteen ordinary publication rows.
    for index in 0..17u8 {
        let mut bytes = [0; 32];
        bytes[0] = index;
        let event = Event::new(
            &owner.identity,
            ConversationId::new(bytes),
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: format!("Unrelated group {index}"),
                members: vec![owner.identity.public()],
                epoch: 0,
                owner: owner.user_id(),
            }
            .encode(),
        );
        install(&owner, &event);
    }
    let group = ConversationId::new([255; 32]);
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Before recovery".into(),
            members: vec![owner.identity.public(), external.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let invitation = Event::new(
        &owner.identity,
        group,
        2,
        vec![genesis.id],
        2,
        2,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Before recovery".into(),
            members: vec![
                owner.identity.public(),
                external.identity.public(),
                DeviceIdentity::generate().public(),
            ],
            epoch: 1,
            owner: owner.user_id(),
        }
        .encode_invitation(),
    );
    for target in [&owner, &external] {
        install(target, &genesis);
        install(target, &invitation);
    }
    let old_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&owner, &path, old_listener.local_addr().unwrap().port());
    owner.publish_account_group_access_for(group);
    owner.publish_group_inventory_for(group);
    assert_eq!(inventory(&owner, group).name, "Before recovery");
    let id = owner
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    owner
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "Recovered exact name".into(),
            },
        )
        .unwrap();
    assert!(
        owner
            .interrupt_account_group_completion_for_test(group, id)
            .is_err()
    );
    assert_eq!(
        owner.delivery.lock().unwrap().pending_transactions().len(),
        1,
        "the completion fault must retain the exact accepted WAL transaction"
    );
    let effect = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == EventKind::ChannelRename)
        .unwrap()
        .clone();
    assert_eq!(
        inventory(&owner, group).name,
        "Before recovery",
        "the simulated crash must precede canonical private publication"
    );
    let secrets = owner.identity.secret_bytes();
    drop(old_listener);
    drop(owner);
    let owner = reopen(
        &path,
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    assert!(
        owner
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    let status = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(status.committed_events, vec![effect.id]);
    assert_eq!(
        inventory(&owner, group).name,
        "Before recovery",
        "reopening alone does not certify descriptive cache freshness"
    );
    let owner_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let external_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = external_listener.local_addr().unwrap();
    configure(&owner, &path, owner_listener.local_addr().unwrap().port());
    configure(&external, &dir.path().join("external"), address.port());
    owner.roster.lock().unwrap().update(
        &external.signed_announce("External member", address.port()),
        address.ip(),
        &owner.user_id(),
    );
    let gate = Arc::new(Semaphore::new(0));
    let (arrived, first_sync) = oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let mut tasks = tokio::task::JoinSet::new();
    let peer = external.clone();
    let released = gate.clone();
    let expected = owner.identity.public();
    tasks.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                incoming = external_listener.accept() => {
                    let (stream, _) = incoming.unwrap();
                    let peer = peer.clone(); let gate = released.clone();
                    let arrived = arrived.clone(); let expected = expected.clone();
                    connections.spawn(async move {
                        let mut channel = peer.privacy_accept(stream).await.unwrap();
                        assert_eq!(channel.peer_identity(), &expected);
                        let first = channel.recv().await.unwrap();
                        if let Some(request) = crate::node::channel_account_bootstrap::AccountGroupRequest::decode(&first) {
                            peer.serve_account_group_request(&mut channel, request).await;
                            return;
                        }
                        assert!(crate::node::session::requests_response(&first));
                        if let Some(signal) = arrived.lock().unwrap().take() { let _ = signal.send(()); }
                        let _permit = gate.acquire().await.unwrap();
                        let store = peer.sync_store(channel.peer_identity());
                        if crate::node::session::serve_wire_bytes(&mut channel, &store, &first).await.is_err() { return; }
                        peer.process_channel(group);
                        while let Ok(crate::node::session::Served::Handled(conversation)) =
                            crate::node::session::serve_one(&mut channel, &store).await {
                            peer.process_channel(conversation);
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let (published, first_fanout) = oneshot::channel();
    let weak_owner = Arc::downgrade(&owner);
    *owner.control_fanout_start_test_hook.lock().unwrap() = Some(Box::new(move || {
        let source = weak_owner.upgrade().unwrap();
        let _ = published.send(inventory(&source, group).name);
    }));
    tasks.spawn(owner.clone().run_accept_loop(owner_listener));
    let at_start = tokio::time::timeout(Duration::from_secs(8), first_fanout)
        .await
        .expect("recovered work must enter its actual public fanout owner")
        .unwrap();
    assert_eq!(
        at_start, "Recovered exact name",
        "the exact recovered group must publish synchronously before fanout starts"
    );
    tokio::time::timeout(Duration::from_secs(8), first_sync)
        .await
        .expect("recovered durable work must start a real authenticated external exchange")
        .unwrap();
    assert_eq!(
        inventory(&owner, group).name,
        "Recovered exact name",
        "the recovered exact group must publish before its public fanout starts"
    );
    let operation = owner
        .account_document(&AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: owner.user_id(),
        })
        .unwrap()
        .unwrap();
    let frame = crate::node::account_group::operations::wire::OperationFrame::decode(
        operation.payload(),
        &account.public().ed25519_pub,
    )
    .unwrap();
    assert!(
        frame
            .projection
            .results
            .iter()
            .any(|result| result.status == status)
    );
    gate.add_permits(4);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if external
                .log
                .lock()
                .unwrap()
                .get(&effect.id)
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the restarted runtime must distribute the original recovered event");
    let received = external
        .log
        .lock()
        .unwrap()
        .get(&effect.id)
        .unwrap()
        .unwrap()
        .clone();
    assert_eq!(received.id, effect.id);
    assert_eq!(received.ciphertext, effect.ciphertext);
    assert!(received.verify_integrity() && received.verify_signature());
    assert_eq!(
        received.author.ed25519_pub(),
        &owner.identity.public().ed25519_pub
    );
    assert_eq!(
        owner
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == EventKind::ChannelRename)
            .count(),
        1
    );
}
async fn assert_stalled_fanout_retires_with_account_loop(adopt: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = legacy_group(&owner, &external);
    let group = genesis.conversation_id;
    for target in [&owner, &external] {
        install(target, &genesis);
    }
    let owner_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(
        &owner,
        &dir.path().join("owner"),
        owner_listener.local_addr().unwrap().port(),
    );
    configure(&external, &dir.path().join("external"), address.port());
    owner.roster.lock().unwrap().update(
        &external.signed_announce("External member", address.port()),
        address.ip(),
        &owner.user_id(),
    );
    // Run the production account producer by itself: the only external exchange
    // can be its owned public fanout, without unrelated recovery producers.
    let mut loops = tokio::task::JoinSet::new();
    loops.spawn(owner.clone().run_account_history_loop());
    let id = owner
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    owner
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "Committed before retirement".into(),
            },
        )
        .unwrap();
    let (stream, _) = tokio::time::timeout(Duration::from_secs(8), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut channel = external.privacy_accept(stream).await.unwrap();
    assert_eq!(channel.peer_identity(), &owner.identity.public());
    let first = channel.recv().await.unwrap();
    assert!(crate::node::session::requests_response(&first));
    let original = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(original.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(original.committed_events.len(), 1);
    if adopt {
        let kept = Account::generate();
        owner
            .persist_account_adoption(&kept.public(), || Ok(()), || Ok(kept.public()))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), loops.join_next())
            .await
            .expect("adoption must wake and retire the actual account producer")
            .unwrap()
            .unwrap();
        owner.runtime_work.close();
    } else {
        // These are the participant owner's actual retirement primitives.
        owner.runtime_work.close();
        loops.abort_all();
        while loops.join_next().await.is_some() {}
    }
    tokio::time::timeout(Duration::from_secs(2), owner.runtime_work.drain())
        .await
        .expect("owned fanout captures must drop before runtime admission drains");
    tokio::time::timeout(Duration::from_secs(2), async {
        while channel.recv().await.is_ok() {}
    })
    .await
    .expect("retirement must close the actual stalled authenticated stream");
    let terminal = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(
        terminal, original,
        "retirement cannot rewrite the committed public effect"
    );
}

#[tokio::test]
async fn adoption_cancels_stalled_fanout_and_drains_its_runtime_admission() {
    assert_stalled_fanout_retires_with_account_loop(true).await;
}

#[tokio::test]
async fn closing_account_loop_cancels_stalled_fanout_and_drains_runtime_admission() {
    assert_stalled_fanout_retires_with_account_loop(false).await;
}
