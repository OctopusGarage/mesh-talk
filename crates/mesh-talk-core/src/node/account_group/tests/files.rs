//! File intents retain one identity through preparation, delivery and retries.
use super::*;
use crate::file::FileKind;

#[tokio::test]
async fn physical_member_file_intent_is_id_bound_and_uses_the_real_manifest_sender() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    assert_eq!(
        source.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    let path = dir.path().join("member-attachment.bin");
    let bytes = b"original explicit member attachment";
    std::fs::write(&path, bytes).unwrap();
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let admitted = source
        .submit_account_group_file_if(
            &account.account_id(),
            id,
            group,
            &path,
            FileKind::File,
            None,
            None,
            |_| {},
            |commit| commit(),
        )
        .await
        .expect("an ordinary account member must durably admit its explicit file intent");
    assert!(admitted.committed_events.is_empty());
    source.maintain_account_group_operation_effects();
    let status = source
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    let event = source
        .log
        .lock()
        .unwrap()
        .get(&status.primary_event.unwrap())
        .cloned()
        .unwrap();
    assert_eq!(event.kind, EventKind::FileManifest);
    assert!(event.verify_integrity() && event.verify_signature());
    assert_eq!(
        event.author.ed25519_pub(),
        &source.identity.public().ed25519_pub
    );
    let row = source
        .received_files
        .lock()
        .unwrap()
        .entry(event.id)
        .cloned()
        .unwrap();
    let manifest = crate::node::files::validated_manifest(&row.plaintext).unwrap();
    let file = manifest.file_conv();
    let before = source.log.lock().unwrap().fingerprint_and_len(&file);
    let saved = dir.path().join("saved.bin");
    source.save_file(file, &saved).unwrap();
    assert_eq!(std::fs::read(&saved).unwrap(), bytes);
    assert_eq!(
        source
            .submit_account_group_file_if(
                &account.account_id(),
                id,
                group,
                &path,
                FileKind::File,
                None,
                None,
                |_| {},
                |commit| commit()
            )
            .await
            .unwrap(),
        status
    );
    assert_eq!(
        source.log.lock().unwrap().fingerprint_and_len(&file),
        before,
        "an uncertain-reply retry must reuse the original staged nonce/key/conversation"
    );
    std::fs::write(&path, b"changed raw input").unwrap();
    assert!(
        source
            .submit_account_group_file_if(
                &account.account_id(),
                id,
                group,
                &path,
                FileKind::File,
                None,
                None,
                |_| {},
                |commit| commit()
            )
            .await
            .is_err()
    );
    assert_eq!(
        source
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap(),
        status
    );
    assert_eq!(
        source
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::FileManifest)
            .count(),
        1
    );
}

#[tokio::test]
async fn file_asset_request_requires_original_preparation_and_exact_current_executor() {
    use crate::node::account_documents::wire::GroupFileAssetRequest;
    use crate::node::account_group::operations::journal::OperationReference;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let origin = node(&dir.path().join("origin"), &account);
    let executor = node(&dir.path().join("executor"), &account);
    let other = node(&dir.path().join("other"), &account);
    let genesis = legacy_group(&origin, &executor);
    let group = genesis.conversation_id;
    install(&origin, &genesis);
    install(&executor, &genesis);
    for peer in [&executor, &other] {
        link(&origin, peer);
    }
    configure(&origin, &dir.path().join("origin"), 9);
    origin.roster.lock().unwrap().update(
        &executor.signed_announce("Executor", 9),
        "127.0.0.1".parse().unwrap(),
        &origin.user_id(),
    );
    origin.cache_discovered_peers().unwrap();
    assert!(
        origin
            .historical_author(&executor.identity.public().ed25519_pub)
            .is_some()
    );
    let path = dir.path().join("explicit.bin");
    std::fs::write(&path, b"explicit delegated file payload").unwrap();
    let id = origin
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let payload = origin
        .prepare_account_group_file(
            &account.account_id(),
            id,
            group,
            &path,
            FileKind::File,
            None,
            None,
            |_| {},
        )
        .await
        .unwrap();
    let body = AccountGroupOperation::Send {
        message: payload.prepared().unwrap(),
    };
    let reference = OperationReference::new(
        &origin,
        id,
        group,
        &body,
        executor
            .account_group_access(group)
            .unwrap()
            .sources
            .into_iter()
            .next()
            .unwrap(),
    );
    let manifest =
        crate::node::files::validated_manifest(&payload.descriptor.as_ref().unwrap().manifest)
            .unwrap();
    let request = GroupFileAssetRequest {
        id,
        group,
        digest: reference.digest,
        file_conv: *manifest.file_conv().as_bytes(),
        start_chunk: 0,
        result: false,
    };
    // A private staged artifact alone never authorizes network disclosure.
    assert!(
        origin
            .group_operation_asset_payload(&request, &executor.identity.public())
            .is_none()
    );
    origin
        .install_original_file_request_for_test(reference, body)
        .unwrap();
    assert!(
        origin
            .group_operation_asset_payload(&request, &executor.identity.public())
            .is_some(),
        "only the exact pinned executor may fetch this original explicit artifact"
    );
    assert!(
        origin
            .group_operation_asset_payload(&request, &other.identity.public())
            .is_none()
    );
    let mut changed = request.clone();
    changed.digest[0] ^= 1;
    assert!(
        origin
            .group_operation_asset_payload(&changed, &executor.identity.public())
            .is_none()
    );
    let mut changed = request.clone();
    changed.file_conv[0] ^= 1;
    assert!(
        origin
            .group_operation_asset_payload(&changed, &executor.identity.public())
            .is_none()
    );
    let mut changed = request.clone();
    changed.start_chunk = u32::MAX;
    assert!(
        origin
            .group_operation_asset_payload(&changed, &executor.identity.public())
            .is_none()
    );
    origin
        .cancel_account_group_operation(&account.account_id(), id)
        .unwrap();
    assert!(
        origin
            .group_operation_asset_payload(&request, &executor.identity.public())
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_file_assets_resume_over_noise_with_optional_history_off_and_survive_restart() {
    exercise_file_asset_receive(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_file_intent_cannot_append_a_chunk_received_after_cancellation() {
    exercise_file_asset_receive(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetched_file_cannot_publish_ready_before_ciphertext_sync() {
    exercise_file_asset_receive(false, true).await;
}

async fn exercise_file_asset_receive(cancel_during_receive: bool, fail_ready_sync: bool) {
    use crate::node::account_documents::wire::DocumentRequest;
    use crate::node::account_group::operations::journal::OperationReference;
    use crate::node::account_sync_wire::{
        AccountHistoryFrame, AccountSessionRequest, AccountSyncVersion, MAX_CHUNK,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let origin = node(&dir.path().join("origin"), &account);
    let target_path = dir.path().join("target");
    let target = node(&target_path, &account);
    let genesis = legacy_group(&origin, &target);
    let group = genesis.conversation_id;
    install(&origin, &genesis);
    install(&target, &genesis);
    link(&origin, &target);
    link(&target, &origin);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&origin, &dir.path().join("origin"), address.port());
    configure(&target, &target_path, 9);
    for (left, right, port) in [(&origin, &target, 9), (&target, &origin, address.port())] {
        left.roster.lock().unwrap().update(
            &right.signed_announce("Asset fixture", port),
            address.ip(),
            &left.user_id(),
        );
        left.cache_discovered_peers().unwrap();
        left.set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        left.set_device_sync_category(SyncCategory::Attachments, false)
            .unwrap();
    }
    let bytes = vec![
        0x53;
        if cancel_during_receive {
            73
        } else {
            crate::file::CHUNK_SIZE * 2 + 73
        }
    ];
    let path = dir.path().join("explicit.bin");
    std::fs::write(&path, &bytes).unwrap();
    let id = origin
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let payload = origin
        .prepare_account_group_file(
            &account.account_id(),
            id,
            group,
            &path,
            FileKind::File,
            None,
            None,
            |_| {},
        )
        .await
        .unwrap();
    let body = AccountGroupOperation::Send {
        message: payload.prepared().unwrap(),
    };
    let reference = OperationReference::new(
        &origin,
        id,
        group,
        &body,
        target
            .account_group_access(group)
            .unwrap()
            .sources
            .into_iter()
            .find(|source| source.device == target.identity.public())
            .filter(|source| source.role == AccountGroupRole::Member)
            .expect("pin the actual accepted member executor, not the same-account owner source"),
    );
    origin
        .install_original_file_request_for_test(reference.clone(), body.clone())
        .unwrap();
    target
        .install_original_file_request_for_test(reference.clone(), body)
        .unwrap();
    assert!(
        origin.account_group_file_cached_assets_ready(&reference, &payload),
        "the original requester may reuse its own verified ciphertext for local saving"
    );
    assert!(
        !origin.account_group_file_assets_ready(&reference, &payload),
        "requester content readiness does not establish executor authority"
    );
    let manifest =
        crate::node::files::validated_manifest(&payload.descriptor.as_ref().unwrap().manifest)
            .unwrap();
    let file = manifest.file_conv();
    assert!(
        target
            .fetch_account_group_file_assets(reference.clone(), payload.clone())
            .await
            .is_err(),
        "an unprobed source must not receive an asset extension"
    );
    assert_eq!(target.log.lock().unwrap().conversation_len(&file), 0);
    let held = Arc::new(tokio::sync::Semaphore::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let first = Arc::new(AtomicBool::new(true));
    let server_node = origin.clone();
    let server_held = held.clone();
    let server_requests = requests.clone();
    let server_first = first.clone();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let node = server_node.clone(); let held = server_held.clone(); let requests = server_requests.clone(); let first = server_first.clone();
            connections.spawn(async move {
                let Ok(mut channel) = node.privacy_accept(stream).await else {return;};
                while let Ok(bytes) = channel.recv().await {
                    let Some((_, AccountHistoryFrame::SessionRequest {request_id, request: AccountSessionRequest::Documents {body}})) = AccountHistoryFrame::decode_versioned(&bytes) else {break;};
                    let Some(request) = DocumentRequest::decode(&body) else {break;};
                    channel.send(&AccountHistoryFrame::SessionReady {request_id}.encode_version(AccountSyncVersion::V2).unwrap()).await.unwrap();
                    if let Some(asset) = request.operation.as_ref().and_then(|probe|probe.asset.as_ref()) {
                        requests.lock().unwrap().push(asset.start_chunk);
                        if first.swap(false, Ordering::AcqRel) {
                            let payload = node.group_operation_asset_payload(asset, channel.peer_identity()).unwrap();
                            let descriptor = payload.descriptor.as_ref().unwrap();
                            let file = crate::node::files::validated_manifest(&descriptor.manifest).unwrap().file_conv();
                            let ciphertext = node.log.lock().unwrap().events(&file)[0].ciphertext.clone();
                            channel.send(&AccountHistoryFrame::AttachmentStart {file_conv: *file.as_bytes(), name:"operation-file".into(), size:payload.logical.size, sha256:descriptor.asset_ciphertext_digest}.encode_version(AccountSyncVersion::V2).unwrap()).await.unwrap();
                            for bytes in ciphertext.chunks(MAX_CHUNK) { channel.send(&AccountHistoryFrame::AttachmentChunk {bytes:bytes.to_vec()}.encode_version(AccountSyncVersion::V2).unwrap()).await.unwrap(); }
                            let _ = held.acquire().await;
                            break;
                        }
                        if !node.serve_account_documents(&mut channel, request).await {break;}
                    } else {
                        // This transport-only provider fixture advertises its
                        // implemented File asset path over an actual authenticated
                        // nonce exchange; no role or public grant is invented.
                        let probe = request.operation.as_ref().unwrap();
                        let complete = serde_json::json!({"enabled":true,"domain":"group_control","next":null,"digest":([0u8;32]),
                            "operation":{"version":1,"nonce":probe.nonce,"supported_kinds":AccountGroupOperationKind::File.capability_bit()}});
                        channel.send(&AccountHistoryFrame::DocumentsComplete {body:serde_json::to_vec(&complete).unwrap()}.encode_version(AccountSyncVersion::V2).unwrap()).await.unwrap();
                    }
                }
            });
        }
    });
    let peer = target.routing_peer(&origin.user_id()).unwrap();
    let mut cursor = crate::node::account_documents::sync::DocumentSyncCursor::default();
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    assert!(target.account_group_operation_ready_for(
        &origin.identity.public(),
        AccountGroupOperationKind::File
    ));
    if cancel_during_receive {
        let weak = Arc::downgrade(&target);
        let account_id = account.account_id();
        *target.group_file_asset_recv_test_hook.lock().unwrap() = Some(Box::new(move || {
            weak.upgrade()
                .unwrap()
                .cancel_account_group_operation(&account_id, id)
                .unwrap();
        }));
        let result = target
            .fetch_account_group_file_assets(reference.clone(), payload.clone())
            .await;
        servers.abort_all();
        while servers.join_next().await.is_some() {}
        assert!(
            result.is_err(),
            "cancelled intent must not become asset-ready"
        );
        assert_eq!(
            target.log.lock().unwrap().conversation_len(&file),
            0,
            "a chunk arriving after cancellation must not be appended"
        );
        assert!(!target.account_group_file_assets_ready(&reference, &payload));
        return;
    }
    let task_node = target.clone();
    let task_ref = reference.clone();
    let task_payload = payload.clone();
    let mut fetch = tokio::spawn(async move {
        task_node
            .fetch_account_group_file_assets(task_ref, task_payload)
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while target.log.lock().unwrap().conversation_len(&file) != 1 {
            if fetch.is_finished() {
                let result = (&mut fetch).await;
                panic!(
                    "asset fetch ended before chunk1: {result:?}; requests={:?}",
                    requests.lock().unwrap().as_slice()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fetch.abort();
    let _ = fetch.await;
    tokio::time::timeout(Duration::from_secs(2), target.runtime_work.drain())
        .await
        .unwrap();
    let original_first = target.log.lock().unwrap().events(&file)[0].id;
    held.add_permits(1);
    if fail_ready_sync {
        target.log.lock().unwrap().fail_sync_for_test(true);
    }
    let resumed = tokio::time::timeout(
        Duration::from_secs(5),
        target.fetch_account_group_file_assets(reference.clone(), payload.clone()),
    )
    .await
    .unwrap();
    if fail_ready_sync {
        servers.abort_all();
        while servers.join_next().await.is_some() {}
        assert!(
            resumed.is_err(),
            "ciphertext sync failure must prevent fetched-ready publication"
        );
        assert!(!target.account_group_file_assets_ready(&reference, &payload));
        return;
    }
    resumed.unwrap();
    assert_eq!(*requests.lock().unwrap(), vec![0, 1]);
    assert!(target.account_group_file_assets_ready(&reference, &payload));
    let mut opened = Vec::new();
    {
        let log = target.log.lock().unwrap();
        let events = log.events(&file);
        assert_eq!(events[0].id, original_first);
        for (index, event) in events.into_iter().enumerate() {
            assert_eq!(
                event.author.ed25519_pub(),
                &target.identity.public().ed25519_pub
            );
            assert!(event.verify_integrity() && event.verify_signature());
            opened.extend(
                crate::file::open_chunk_for(&manifest, index as u32, &event.ciphertext).unwrap(),
            );
        }
    }
    assert_eq!(opened, bytes);
    let identity = target.identity.secret_bytes();
    servers.abort_all();
    while servers.join_next().await.is_some() {}
    drop(target);
    let (dm, _) = mpsc::unbounded_channel();
    let (channels, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let reopened = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(identity.0, identity.1),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channels,
        files,
        &target_path.join("events.log"),
        &target_path.join("sent.log"),
        "pw",
    )
    .unwrap();
    assert!(reopened.account_group_file_assets_ready(&reference, &payload));
    assert!(
        reopened
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::AwaitingAcceptance)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reference_file_and_fresh_sibling_retry_save_the_original_bytes_without_optional_history() {
    file_result_roundtrip(0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matching_file_retry_preserves_a_result_adopted_during_lookup_admission() {
    file_result_roundtrip(4).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflicting_file_retry_cannot_replace_a_result_adopted_during_lookup_admission() {
    file_result_roundtrip(5).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_signed_file_prefix_prepares_cache_without_holding_delivery() {
    file_result_roundtrip(1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retired_file_copy_cannot_publish_a_prepared_managed_cache() {
    file_result_roundtrip(2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_signed_public_chunk_cannot_corrupt_a_same_account_file_result() {
    file_result_roundtrip(3).await;
}

/// Static stage labels diagnose scheduling failures without printing file or intent data.
fn file_result_timeout_stage(
    fresh: &Node,
    source: &Node,
    account: &Account,
    id: AccountGroupOperationId,
    file: crate::eventlog::ConversationId,
) -> &'static str {
    if !fresh.account_group_operation_ready_for(
        &source.identity.public(),
        AccountGroupOperationKind::File,
    ) {
        return "result timeout: authenticated source File capability unavailable";
    }
    let Ok(Some(status)) = fresh.account_group_operation_status(&account.account_id(), id) else {
        return "result timeout: operation status unavailable";
    };
    if status.phase != AccountGroupOperationPhase::Committed {
        return "result timeout: operation has not committed";
    }
    let Some(progress) = fresh.file_progress(file) else {
        return "result timeout: committed result has no installed file card";
    };
    if progress.done != progress.total {
        return "result timeout: committed card awaits complete managed bytes";
    }
    "result timeout: complete managed bytes did not reach the preparation observation"
}

async fn file_result_roundtrip(mode: u8) {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let offline_owner = node(&dir.path().join("offline-owner"), &Account::generate());
    let source = node(&dir.path().join("source"), &account);
    let origin = node(&dir.path().join("origin"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = Event::new(
        &offline_owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Explicit account file".into(),
            members: vec![
                offline_owner.identity.public(),
                source.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: offline_owner.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
    install(&source, &genesis);
    install(&external, &genesis);
    link(&source, &origin);
    link(&origin, &source);
    let mut tasks = tokio::task::JoinSet::new();
    let mut addresses = Vec::new();
    let mut listeners = Vec::new();
    for (name, target) in [
        ("source", &source),
        ("origin", &origin),
        ("external", &external),
    ] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        target
            .set_device_sync_category(SyncCategory::Attachments, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push(listener);
    }
    for left in [&source, &origin, &external] {
        for (right, address) in [&source, &origin, &external].into_iter().zip(&addresses) {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Explicit file fixture", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
        left.cache_discovered_peers().unwrap();
    }
    for (target, listener) in [&source, &origin, &external].into_iter().zip(listeners) {
        // The accept-loop owner already runs delivery; a second producer has
        // its own scheduler and competes for the same bounded session pool.
        tasks.spawn(target.clone().run_accept_loop(listener));
    }
    source.publish_account_group_access(&mut None);
    pull_group_control(&origin, &source, addresses[0]).await;
    assert_eq!(
        origin.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert!(origin.channel_members(group).is_empty());
    let bytes = vec![0x61; crate::file::CHUNK_SIZE * 2 + 137];
    let original_path = dir.path().join("origin").join("explicit.bin");
    std::fs::write(&original_path, &bytes).unwrap();
    let id = origin
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let admitted = origin
        .submit_account_group_file_if(
            &account.account_id(),
            id,
            group,
            &original_path,
            FileKind::File,
            None,
            None,
            |_| {},
            |commit| commit(),
        )
        .await
        .expect("reference account member must admit its explicit file intent");
    assert_eq!(admitted.executions[0].device_id, source.user_id());
    let status = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status = origin
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .unwrap();
            if status.phase == AccountGroupOperationPhase::Committed
                && status
                    .primary_event
                    .is_some_and(|event| external.log.lock().unwrap().get(&event).is_some())
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("explicit File must commit on its pinned source and reach the external member");
    let event_id = status.primary_event.unwrap();
    let event = external
        .log
        .lock()
        .unwrap()
        .get(&event_id)
        .cloned()
        .unwrap();
    assert_eq!(event.kind, EventKind::FileManifest);
    assert_eq!(
        event.author.ed25519_pub(),
        &source.identity.public().ed25519_pub
    );
    assert!(event.verify_integrity() && event.verify_signature());
    let original_card = origin
        .received_files
        .lock()
        .unwrap()
        .entry(event_id)
        .cloned()
        .expect("explicit own result must install a real file card before reporting Committed");
    assert_eq!(original_card.from, source.user_id());
    let manifest = crate::node::files::validated_manifest(&original_card.plaintext).unwrap();
    let file = manifest.file_conv();
    tokio::time::timeout(Duration::from_secs(8), async {
        while external.file_progress(file).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the external member must receive a real file card");
    external.decide_attachment(file, true).unwrap();
    assert_eq!(
        external.attachment_consent_status(file),
        Some(crate::node::ReceiveDecision::Accepted)
    );
    assert!(
        source
            .file_progress(file)
            .is_some_and(|progress| progress.done == progress.total)
    );
    source.flush_channel_file(group, file).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        while !external
            .file_progress(file)
            .is_some_and(|progress| progress.done == progress.total)
        {
            // The native participant's separate file-pull producer drives this
            // same consent-checked public API; the accept loop does not own it.
            external.pull_pending_files().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the accepted external recipient must receive actual signed file chunks");
    let external_saved = dir.path().join("external-saved.bin");
    external.save_file(file, &external_saved).unwrap();
    assert_eq!(std::fs::read(external_saved).unwrap(), bytes);
    let original_cache = origin.log.lock().unwrap().fingerprint_and_len(&file);
    assert_eq!(original_cache.1, manifest.chunk_count() as usize);
    assert!(
        origin
            .log
            .lock()
            .unwrap()
            .events(&file)
            .iter()
            .all(|chunk| chunk.author.ed25519_pub() == &origin.identity.public().ed25519_pub),
        "the original cache must not mix copied executor chunks with creator chunks"
    );
    let progress = origin
        .file_progress(file)
        .expect("real original card must expose progress");
    assert_eq!(progress.done, progress.total);
    let origin_saved = dir.path().join("origin-saved.bin");
    origin.save_file(file, &origin_saved).unwrap();
    assert_eq!(std::fs::read(origin_saved).unwrap(), bytes);
    // Existing save completion may reclaim a terminal local cache. Later B
    // hydration must neither recreate it nor append another signer's chunks.
    let after_origin_save = origin.log.lock().unwrap().fingerprint_and_len(&file);
    assert!(origin.channel_members(group).is_empty());

    // B has never seen A's operation head or staged the original artifact.
    let fresh_path = dir.path().join("fresh");
    let fresh = node(&fresh_path, &account);
    for sibling in [&source, &origin] {
        link(sibling, &fresh);
        link(&fresh, sibling);
    }
    fresh
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    fresh
        .set_device_sync_category(SyncCategory::Attachments, false)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&fresh, &fresh_path, address.port());
    for (peer, peer_address) in [(&source, addresses[0]), (&origin, addresses[1])] {
        fresh.roster.lock().unwrap().update(
            &peer.signed_announce("Existing executor", peer_address.port()),
            peer_address.ip(),
            &fresh.user_id(),
        );
        peer.roster.lock().unwrap().update(
            &fresh.signed_announce("Fresh sibling", address.port()),
            address.ip(),
            &peer.user_id(),
        );
        peer.cache_discovered_peers().unwrap();
    }
    fresh.cache_discovered_peers().unwrap();
    assert!(
        fresh
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .is_none()
    );
    assert_eq!(fresh.log.lock().unwrap().conversation_len(&file), 0);
    let prepare_observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    if mode == 1 || mode == 2 {
        let original = source.log.lock().unwrap().events(&file)[0].clone();
        assert!(original.verify_integrity() && original.verify_signature());
        fresh.log.lock().unwrap().append_durable(original).unwrap();
        let weak = Arc::downgrade(&fresh);
        let observed = prepare_observed.clone();
        fresh.media.before_prepare_for_test(Box::new(move || {
            let node = weak.upgrade().unwrap();
            observed.store(
                if node.delivery.try_lock().is_ok() {
                    1
                } else {
                    2
                },
                std::sync::atomic::Ordering::SeqCst,
            );
            if mode == 2 {
                node.runtime_work.close();
            }
        }));
    }
    if mode == 3 {
        let original = source.log.lock().unwrap().events(&file)[0].clone();
        assert!(original.verify_integrity() && original.verify_signature());
        let weak = Arc::downgrade(&fresh);
        let observed = prepare_observed.clone();
        *fresh.group_file_asset_recv_test_hook.lock().unwrap() = Some(Box::new(move || {
            // Simulate an authenticated public pull completing after the result
            // worker captured an empty prefix, using the real C-signed event.
            weak.upgrade()
                .unwrap()
                .log
                .lock()
                .unwrap()
                .append_durable(original)
                .unwrap();
            observed.store(3, std::sync::atomic::Ordering::SeqCst);
        }));
    }
    // Import only role/directory observations before asking for A's immutable ID.
    // Production operation maintenance starts after the explicit local retry.
    assert!(
        !fresh.account_group_operation_ready_for(
            &source.identity.public(),
            AccountGroupOperationKind::File,
        ),
        "discovery and sibling certification alone cannot authorize the File protocol"
    );
    // Document installation wakes account work before the exchange finishes
    // installing its authenticated capability receipt. Establish this fixture
    // prerequisite before starting the worker, rather than racing its retry.
    pull_group_control(&fresh, &source, addresses[0]).await;
    assert!(
        fresh.account_group_operation_ready_for(
            &source.identity.public(),
            AccountGroupOperationKind::File,
        ),
        "the actual authenticated capability exchange must precede result work"
    );
    let mut fresh_listener = Some(listener);
    if mode < 4 {
        let fresh_clone = fresh.clone();
        let listener = fresh_listener.take().unwrap();
        tasks.spawn(async move { Box::pin(fresh_clone.run_accept_loop(listener)).await });
    }
    let fresh_raw = fresh_path.join("explicit.bin");
    let mut retry_bytes = bytes.clone();
    if mode == 5 {
        retry_bytes[0] ^= 1;
    }
    std::fs::write(&fresh_raw, &retry_bytes).unwrap();
    if mode >= 4 {
        let result = source
            .operation_projection(group)
            .results
            .into_iter()
            .find(|result| result.reference.id == id)
            .expect("source committed result");
        let proof = source.own_group_proof();
        // The importer releases delivery after card materialization, before its
        // final journal adoption. Stop at precisely that production boundary.
        fresh
            .import_explicit_account_group_file_card(
                &result.reference,
                result.message_event.as_ref().unwrap(),
                result.file_descriptor.as_ref().unwrap(),
                &proof,
            )
            .unwrap();
        let observed = prepare_observed.clone();
        super::super::operations::before_lookup_commit_for_test(id, move |node| {
            super::super::operations::finish_imported_result_for_test(node, result)
                .expect("authenticated original result must adopt in the retry gap");
            observed.store(4, std::sync::atomic::Ordering::SeqCst);
        });
    }
    let retry = fresh
        .submit_account_group_file_if(
            &account.account_id(),
            id,
            group,
            &fresh_raw,
            FileKind::File,
            None,
            None,
            |_| {},
            |commit| commit(),
        )
        .await;
    if mode >= 4 {
        assert_eq!(
            prepare_observed.load(std::sync::atomic::Ordering::SeqCst),
            4,
            "the original result must arrive after the initial retry lookup"
        );
    }
    if mode == 5 {
        assert!(
            matches!(retry, Err(crate::node::NodeError::InvalidInput(_))),
            "a concurrent original result must still reject different retry content"
        );
        let retained = fresh
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        assert_eq!(retained.phase, AccountGroupOperationPhase::Committed);
        assert_eq!(retained.primary_event, Some(event_id));
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        return;
    }
    let retry = retry.expect("a foreign-origin matching retry must resolve the original reference");
    if mode >= 4 {
        assert_eq!(retry.phase, AccountGroupOperationPhase::Committed);
        let fresh_clone = fresh.clone();
        let listener = fresh_listener.take().unwrap();
        tasks.spawn(async move { Box::pin(fresh_clone.run_accept_loop(listener)).await });
    }
    assert!(retry.primary_event.is_none() || retry.primary_event == Some(event_id));
    if mode == 2 {
        tokio::time::timeout(Duration::from_secs(8), async {
            while prepare_observed.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{}",
                file_result_timeout_stage(&fresh, &source, &account, id, file)
            )
        });
        tokio::time::timeout(Duration::from_secs(2), fresh.runtime_work.drain())
            .await
            .unwrap();
        assert!(
            !fresh.media.contains(file),
            "retired preparation cannot publish managed bytes"
        );
        assert_eq!(
            fresh.file_copy_complete_for_test(id),
            Some(false),
            "retirement cannot durably complete the result copy"
        );
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        return;
    }
    let fresh_status = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status = fresh
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .unwrap();
            if status.phase == AccountGroupOperationPhase::Committed
                && fresh
                    .file_progress(file)
                    .is_some_and(|progress| progress.done == progress.total)
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{}",
            file_result_timeout_stage(&fresh, &source, &account, id, file)
        )
    });
    assert_eq!(fresh_status.primary_event, Some(event_id));
    assert!(
        fresh_status
            .executions
            .iter()
            .all(|execution| execution.device_id == source.user_id())
    );
    let saved = dir.path().join("fresh-saved.bin");
    fresh.save_file(file, &saved).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), bytes);
    if mode == 3 {
        assert_eq!(
            prepare_observed.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "the signed chunk must arrive after the result prefix snapshot"
        );
    }
    if mode == 1 {
        assert_eq!(
            prepare_observed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "bulk cache encryption and verification must not hold the delivery gate"
        );
        assert_eq!(
            fresh.log.lock().unwrap().conversation_len(&file),
            1,
            "the signed source prefix must not be mixed with locally authored chunks"
        );
    }
    assert!(fresh.channel_members(group).is_empty());
    assert_eq!(
        origin.log.lock().unwrap().fingerprint_and_len(&file),
        after_origin_save
    );
    assert_eq!(
        source
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::FileManifest)
            .count(),
        1
    );
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}
