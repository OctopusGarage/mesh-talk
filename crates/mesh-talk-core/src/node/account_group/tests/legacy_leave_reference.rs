//! A reference-only device leaves through the original physical member.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reference_only_legacy_account_leave_uses_original_source_over_noise_with_history_off() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let offline_owner = node(&dir.path().join("offline-owner"), &Account::generate());
    let source = node(&dir.path().join("original-member"), &account);
    let reference = node(&dir.path().join("reference-only"), &account);
    let genesis = legacy_group(&offline_owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    link(&source, &reference);
    link(&reference, &source);
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for (name, node) in [("original-member", &source), ("reference-only", &reference)] {
        node.set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(node, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push(listener);
    }
    for (receiver, peer, address) in [
        (&source, &reference, addresses[1]),
        (&reference, &source, addresses[0]),
    ] {
        receiver.roster.lock().unwrap().update(
            &peer.signed_announce("Legacy reference fixture", address.port()),
            address.ip(),
            &receiver.user_id(),
        );
    }
    let mut tasks = tokio::task::JoinSet::new();
    for (node, listener) in [&source, &reference].into_iter().zip(listeners) {
        tasks.spawn(node.clone().run_accept_loop(listener));
    }
    source.publish_account_group_access(&mut None);
    pull_group_control(&reference, &source, addresses[0], group).await;
    assert!(reference.legacy_group_leave_ready(&source.identity.public()));
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert!(reference.channel_members(group).is_empty());
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let admitted = reference
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::LeaveAccount,
        )
        .unwrap();
    assert_eq!(admitted.executions[0].device_id, source.user_id());
    // Permit one normal ten-second repair tick plus bounded local exchanges;
    // no owner listener, forced pairing, manual maintenance or archive import.
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if reference
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed)
                && reference.account_group_access(group).unwrap().role == AccountGroupRole::Left
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("reference-only legacy leave must finish through its certified original source");
    let status = reference
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.committed_events.len(), 1);
    assert!(
        reference.channel_members(group).is_empty(),
        "a private leave receipt cannot invent public membership"
    );
    let leaves = source
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .filter(|event| event.kind == EventKind::ChannelLeave)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(leaves.len(), 1);
    assert_eq!(
        leaves[0].author.ed25519_pub(),
        &source.identity.public().ed25519_pub
    );
    assert!(leaves[0].ciphertext.is_empty() && leaves[0].verify_signature());
    for node in [&source, &reference] {
        let operations = node
            .list_account_group_operations(
                &account.account_id(),
                AccountGroupOperationQuery {
                    group: Some(group),
                    pending_only: false,
                    after: None,
                    limit: 64,
                },
            )
            .unwrap();
        assert_eq!(
            operations.len(),
            1,
            "hidden physical children are not separate user operations"
        );
        assert_eq!(operations[0].id, id);
    }
    tasks.shutdown().await;
}
