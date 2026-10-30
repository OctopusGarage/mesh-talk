//! Explicit control receipts must not wait behind a held account directory page.
use super::selected_same_source_control_latency::ControlTestPages;
use super::*;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

struct ReleaseDirectoryOnDrop(Arc<Semaphore>);
impl Drop for ReleaseDirectoryOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incoming_selected_source_request_overtakes_another_siblings_held_control_over_noise() {
    incoming_request_over_held_control(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incoming_request_overtakes_a_naturally_hinted_control_and_repays_its_background_turn() {
    incoming_request_over_held_control(true).await;
}

async fn incoming_request_over_held_control(naturally_hinted: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let other = node(&dir.path().join("other"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = Event::new(
        &owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Before directory hold".into(),
            members: vec![owner.identity.public(), external.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
    let pending = DeviceIdentity::generate();
    let invitation = Event::new(
        &owner.identity,
        group,
        2,
        vec![genesis.id],
        2,
        2,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Before directory hold".into(),
            members: vec![
                owner.identity.public(),
                external.identity.public(),
                pending.public(),
            ],
            epoch: 1,
            owner: owner.user_id(),
        }
        .encode_invitation(),
    );
    for target in [&owner, &external] {
        install(target, &genesis);
        // A real pending invitation keeps this an owner-signed legacy effect.
        install(target, &invitation);
    }
    link(&owner, &reference);
    link(&reference, &owner);
    for (left, right) in [
        (&owner, &other),
        (&other, &owner),
        (&reference, &other),
        (&other, &reference),
    ] {
        link(left, right);
    }
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for (name, target) in [
        ("owner", &owner),
        ("reference", &reference),
        ("external", &external),
        ("other", &other),
    ] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push((target.clone(), listener));
    }
    for left in [&owner, &reference, &external, &other] {
        for (right, address) in [&owner, &reference, &external, &other]
            .into_iter()
            .zip(&addresses)
        {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Mandatory control fixture", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    let mut servers = tokio::task::JoinSet::new();
    let mut inbound_servers = Vec::new();
    for (target, listener) in listeners {
        if naturally_hinted {
            // Receive authenticated setup exchanges without starting a control
            // producer. A real Wake is queued before the owner's first turn.
            inbound_servers.push(super::inbound_server(target, listener));
        } else {
            servers.spawn(target.run_accept_loop(listener));
        }
    }
    owner.publish_account_group_access(&mut None);
    super::message_action_reference::pull_reference_action_control(
        &reference,
        &owner,
        addresses[0],
        group,
    )
    .await;
    if naturally_hinted {
        let peer = owner.routing_peer(&reference.user_id()).unwrap();
        owner
            .pull_account_documents_page(
                &peer,
                &mut crate::node::account_documents::sync::DocumentSyncCursor::control(),
            )
            .await
            .unwrap();
    }
    super::cold_account_setup(async {
        loop {
            let ready = [&owner, &reference].into_iter().all(|node| {
                let peer = if node.user_id() == owner.user_id() {
                    &reference
                } else {
                    &owner
                };
                node.group_operation_versions
                    .lock()
                    .unwrap()
                    .get(&peer.user_id())
                    .is_some_and(|support| {
                        support.public == peer.identity.public()
                            && support.kinds & AccountGroupOperationKind::Rename.capability_bit()
                                != 0
                    })
            });
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both certified devices must bootstrap real operation support");
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(reference.channel_members(group).is_empty());

    let received_wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pages = Arc::new(Mutex::new(ControlTestPages::default()));
    let held_task = Arc::new(Mutex::new(None::<tokio::task::Id>));
    let held_passive = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sender = reference.identity.public();
    *owner.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let received = received_wakes.clone();
        move |peer, frame| {
            if peer == sender && matches!(frame, AccountHistoryFrame::Wake { .. }) {
                received.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Box::pin(async {})
        }
    }));
    let diagnostic_started = std::time::Instant::now();
    let diagnostic_tail = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    *owner.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let pages = pages.clone();
        let other_public = other.identity.public();
        let reference_public = reference.identity.public();
        let diagnostic_tail = diagnostic_tail.clone();
        move |peer, event| {
            if event.starts_with("page_end") || event.starts_with("control_owned_abort") {
                let peer_label = if peer == other_public {
                    "other"
                } else if peer == reference_public {
                    "reference"
                } else {
                    "unknown"
                };
                let mut tail = diagnostic_tail.lock().unwrap();
                if tail.len() == 64 {
                    tail.pop_front();
                }
                tail.push_back((
                    diagnostic_started.elapsed().as_millis(),
                    peer_label,
                    event.clone(),
                ));
            }
            if peer != other_public {
                return;
            }
            pages.lock().unwrap().observe(&event);
        }
    }));
    let release = Arc::new(Semaphore::new(0));
    let _release_on_drop = ReleaseDirectoryOnDrop(release.clone());
    let (arrived, held) = oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let checkpoints = Arc::new(Mutex::new(Vec::new()));
    let exact_reference = owner.identity.public();
    *other.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let release = release.clone();
        let checkpoints = checkpoints.clone();
        let pages = pages.clone();
        let held_task = held_task.clone();
        let held_passive = held_passive.clone();
        move |peer, frame| {
            let release = release.clone();
            let arrived = arrived.clone();
            let checkpoints = checkpoints.clone();
            let pages = pages.clone();
            let held_task = held_task.clone();
            let held_passive = held_passive.clone();
            let exact_reference = exact_reference.clone();
            Box::pin(async move {
                if peer != exact_reference {
                    return;
                }
                let AccountHistoryFrame::DocumentsRequest { body } = frame else {
                    return;
                };
                let request = crate::node::account_documents::wire::DocumentRequest::decode(&body)
                    .expect("authenticated request must decode");
                if request.domain == AccountDocumentDomain::GroupControl {
                    let Some(page) = pages.lock().unwrap().live_request(&request) else {
                        return;
                    };
                    let Some(dispatch) = page.dispatch() else {
                        return;
                    };
                    let eligible = if naturally_hinted {
                        dispatch.natural_origin() && dispatch.hint
                    } else {
                        !dispatch.protected && !dispatch.hint
                    };
                    if arrived.lock().unwrap().is_some() && !eligible {
                        return;
                    }
                    checkpoints.lock().unwrap().push((
                        request.version,
                        request.after.clone(),
                        request.known_digest,
                        request.operation.as_ref().map(|probe| probe.known_digest),
                    ));
                    let signal = arrived.lock().unwrap().take();
                    if let Some(signal) = signal {
                        *held_task.lock().unwrap() = Some(page.task);
                        held_passive.store(true, std::sync::atomic::Ordering::SeqCst);
                        let _ = signal.send(());
                        // SessionReady was sent; hold only the unselected sibling control response.
                        let _ = release.acquire().await;
                    }
                }
            })
        }
    }));
    if naturally_hinted {
        other
            .set_device_hostname(Some("natural-control-hint"))
            .unwrap();
        let peer = other.routing_peer(&owner.user_id()).unwrap();
        let mut wake = other
            .begin_account_exchange(
                &peer,
                crate::node::account_sync_wire::AccountSyncVersion::V2,
                AccountHistoryFrame::Wake {
                    account_id: account.account_id(),
                    epoch: [7; 16],
                    revision: 1,
                },
            )
            .await
            .unwrap();
        let reply = wake.recv().await.unwrap();
        assert!(matches!(
            AccountHistoryFrame::decode_versioned(&reply),
            Some((
                crate::node::account_sync_wire::AccountSyncVersion::V2,
                AccountHistoryFrame::WakeAck {
                    epoch,
                    revision: 1
                }
            )) if epoch == [7; 16]
        ));
        wake.recycle(&other);
        servers.spawn(owner.clone().run_account_history_loop());
    }
    owner.publish_group_inventory_for(group);
    // Bootstrap may already have published this exact Inventory head. Its
    // unchanged publication correctly emits no wake; schedule the reference's
    // next ordinary refresh only after the response hook is installed.
    owner.account_sync_notify.notify_one();
    super::cold_account_setup(held)
        .await
        .expect("the selected executor must reach the unrelated authenticated Control response")
        .unwrap();

    assert!(
        held_passive.load(std::sync::atomic::Ordering::SeqCst),
        "the held Control must have the exact requested passive or natural-hint origin"
    );
    if naturally_hinted {
        for node in [&reference, &other, &external] {
            servers.spawn(node.clone().run_account_history_loop());
        }
        for node in [&owner, &reference, &other, &external] {
            servers.spawn(node.clone().run_account_wake_loop());
        }
    }
    let wakes_before = received_wakes.load(std::sync::atomic::Ordering::SeqCst);
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "After directory hold".into(),
            },
            |commit| {
                let task = held_task
                    .lock()
                    .unwrap()
                    .expect("the exact passive request is held");
                assert!(
                    pages.lock().unwrap().is_active(task),
                    "the exact passive Control task must be live at admission"
                );
                commit()
            },
        )
        .unwrap();
    let converged = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let requester = reference
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            let source = owner
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            if let (Some(requester), Some(source)) = (requester, source)
                && requester.phase == AccountGroupOperationPhase::Committed
                && source.phase == AccountGroupOperationPhase::Committed
            {
                break (requester, source);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let wakes_while_held = received_wakes
        .load(std::sync::atomic::Ordering::SeqCst)
        .saturating_sub(wakes_before);
    let source_while_held = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let reference_while_held = reference
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let source_effects_while_held = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .filter(|event| event.kind == EventKind::ChannelRename)
        .cloned()
        .collect::<Vec<_>>();
    // Always release, verify directory recovery and drain owned work before the
    // held-period assertion can fail. No direct pull or result adoption is used.
    release.close();
    let control_resumed = tokio::time::timeout(Duration::from_secs(8), async {
        while checkpoints.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    *other.account_session_test_hook.lock().unwrap() = None;
    let preserved_checkpoint = {
        let checkpoints = checkpoints.lock().unwrap();
        checkpoints.len() >= 2 && checkpoints[0] == checkpoints[1]
    };
    let inventory_key = crate::node::AccountDocumentKey::GroupInventory {
        channel: hex::encode(group.as_bytes()),
        source_device: owner.user_id(),
    };
    let resumed =
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if reference.account_document(&inventory_key).unwrap().is_some_and(|document| {
                serde_json::from_slice::<serde_json::Value>(document.payload()).unwrap()["name"]
                    == "After directory hold"
            }) {
                break;
            }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
    let error_snapshot = || {
        owner
            .device_sync_status()
            .into_iter()
            .map(|status| {
                let peer = if status.device_id == other.user_id() {
                    "other"
                } else if status.device_id == reference.user_id() {
                    "reference"
                } else {
                    "unknown"
                };
                let error = match status.document_error.as_deref() {
                    None => "none",
                    Some("account document exchange interrupted") => "exchange_interrupted",
                    Some("account document worker terminated") => "worker_terminated",
                    Some(_) => "other_error",
                };
                (
                    peer,
                    status.document_phase,
                    error,
                    status.document_requires_upgrade,
                )
            })
            .collect::<Vec<_>>()
    };
    eprintln!(
        "selected-request before_teardown errors={:?}",
        error_snapshot()
    );
    for node in [&owner, &reference, &external, &other] {
        node.runtime_work.close();
    }
    servers.shutdown().await;
    for server in inbound_servers {
        server.abort();
        let _ = server.await;
    }
    for node in [&owner, &reference, &external, &other] {
        tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .expect("the released fixture must drain its owned work");
    }
    eprintln!(
        "selected-request after_teardown errors={:?} bounded_peer_domain_outcomes={:?}",
        error_snapshot(),
        diagnostic_tail.lock().unwrap()
    );
    assert!(
        wakes_while_held > 0,
        "the requester must send an actual authenticated wake while passive Control is held"
    );
    assert!(
        source_while_held.is_some(),
        "the selected executor has not imported the incoming intent while another sibling Control is held"
    );
    assert!(
        source_while_held
            .as_ref()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed),
        "the selected executor must commit the incoming private intent while another sibling Control is held"
    );
    assert!(
        reference_while_held
            .as_ref()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed),
        "the requester must receive its exact private terminal receipt independently of the held Control"
    );
    assert!(resumed.is_ok(), "the released Inventory page must resume");
    assert!(
        control_resumed.is_ok(),
        "unrelated sibling control must resume after the selected operation settles"
    );
    assert!(
        preserved_checkpoint,
        "preemption must retain the exact protocol scope, cursor and digests"
    );
    assert!(
        owner
            .device_sync_status()
            .iter()
            .all(|status| status.document_error.is_none()),
        "owned preemption is not a network failure"
    );
    assert!(
        source_while_held
            .as_ref()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed),
        "the original owner must commit through the real request path while Inventory is held"
    );
    assert_eq!(source_effects_while_held.len(), 1);
    let effect = &source_effects_while_held[0];
    assert!(effect.verify_integrity() && effect.verify_signature());
    assert_eq!(
        effect.author.ed25519_pub(),
        &owner.identity.public().ed25519_pub
    );
    let (requester, source) = converged.unwrap_or_else(|_| {
        panic!("selected executor request waited behind another sibling Control: requester={:?}, source={:?}, local_effects={}", reference_while_held.map(|status| status.phase), source_while_held.map(|status| status.phase), source_effects_while_held.len())
    });
    assert_eq!(requester.committed_events, vec![effect.id]);
    assert_eq!(source.committed_events, requester.committed_events);
    assert!(reference.channel_members(group).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_terminal_publication_retries_a_locally_busy_wake_without_remote_cooldown() {
    use crate::node::account_documents::{AccountDocumentKey, sync::DocumentSyncCursor};
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let reference = node(&directory.path().join("reference"), &account);
    link(&source, &reference);
    link(&reference, &source);
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for (name, node) in [("source", &source), ("reference", &reference)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(node, &directory.path().join(name), address.port());
        node.set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        addresses.push(address);
        listeners.push((node.clone(), listener));
    }
    let group = source
        .create_channel("Wake result fixture", vec![])
        .await
        .unwrap();
    source
        .maintain_account_channels(
            &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
        )
        .await;
    source.publish_account_group_access_for(group);
    for (left, right, address) in [
        (&source, &reference, addresses[1]),
        (&reference, &source, addresses[0]),
    ] {
        left.roster.lock().unwrap().update(
            &right.signed_announce("Wake capacity fixture", address.port()),
            address.ip(),
            &left.user_id(),
        );
    }
    let mut capacity = Vec::new();
    while let Some(permit) = source.sessions.try_outbound() {
        capacity.push(permit);
    }
    assert_eq!(
        capacity.len(),
        crate::node::session_pool::MAX_DIRECTION_CONNECTIONS
    );
    let (busy_tx, busy_rx) = oneshot::channel();
    let busy_tx = Arc::new(Mutex::new(Some(busy_tx)));
    *source.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, event| {
        // The only source client task in this fixture is the real Wake owner.
        if event.starts_with("negotiation_end ")
            && event.ends_with("outcome=capacity_busy")
            && let Some(tx) = busy_tx.lock().unwrap().take()
        {
            let _ = tx.send(());
        }
    }));
    let (wake_tx, wake_rx) = oneshot::channel();
    let wake_tx = Arc::new(Mutex::new(Some(wake_tx)));
    *reference.account_session_test_hook.lock().unwrap() = Some(Arc::new(move |_, frame| {
        if matches!(frame, AccountHistoryFrame::Wake { .. })
            && let Some(tx) = wake_tx.lock().unwrap().take()
        {
            let _ = tx.send(());
        }
        Box::pin(async {})
    }));
    let mut servers = tokio::task::JoinSet::new();
    for (node, listener) in listeners {
        servers.spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let node = node.clone();
                        connections.spawn(async move {
                            let channel = node.privacy_accept(stream).await.unwrap();
                            node.serve_connection(channel).await;
                        });
                    }
                    finished = connections.join_next(), if !connections.is_empty() => { finished.unwrap().unwrap(); }
                }
            }
        });
    }
    let wake_worker = tokio::spawn(source.clone().run_account_wake_loop());
    let busy = tokio::time::timeout(Duration::from_secs(5), busy_rx).await;
    assert!(
        busy.is_ok_and(|result| result.is_ok()),
        "the real Wake must first exhaust local TCP capacity"
    );
    drop(capacity);
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    source
        .submit_account_group_text_if(
            &account.account_id(),
            id,
            group,
            crate::node::AccountGroupTextInput {
                text: b"real terminal result after local admission recovery".to_vec(),
                reply_to: None,
                expires_at_ms: None,
                protection: None,
            },
            |commit| commit(),
        )
        .unwrap();
    source.maintain_account_group_operation_effects();
    let status = source
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(status.committed_events.len(), 1);
    source.publish_account_group_operations_for(group);
    let key = AccountDocumentKey::GroupOperation {
        channel: hex::encode(group.as_bytes()),
        source_device: source.user_id(),
    };
    let expected = source.account_document_meta(&key).unwrap();
    let peer = reference.routing_peer(&source.user_id()).unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(8), async {
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), wake_rx).await,
            Ok(Ok(()))
        ) {
            return false;
        }
        let mut cursor = DocumentSyncCursor::control();
        loop {
            reference
                .pull_account_documents_page(&peer, &mut cursor)
                .await
                .unwrap();
            if reference.account_document_meta(&key).as_ref() == Some(&expected) {
                break;
            }
        }
        true
    })
    .await;
    wake_worker.abort();
    let _ = wake_worker.await;
    servers.shutdown().await;
    assert!(
        matches!(receipt, Ok(true)),
        "local capacity recovery must not suppress a real new terminal publication for the remote-failure cooldown"
    );
}
