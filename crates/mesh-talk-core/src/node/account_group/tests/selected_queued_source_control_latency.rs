//! A queued pre-operation suffix does not witness a newly admitted request.
use super::*;
use crate::node::account_documents::wire::DocumentRequest;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

struct ReleaseQueuedReads(Arc<Semaphore>);
impl Drop for ReleaseQueuedReads {
    fn drop(&mut self) {
        self.0.close();
    }
}
fn effective_after(request: &DocumentRequest) -> Option<crate::node::AccountDocumentKey> {
    use crate::node::AccountDocumentKey;
    request
        .operation
        .as_ref()
        .and_then(|probe| probe.after.as_ref())
        .map(|after| {
            if after.legacy_leave {
                AccountDocumentKey::GroupLegacyOperation {
                    channel: after.channel.clone(),
                    source_device: after.source_device.clone(),
                }
            } else {
                AccountDocumentKey::GroupOperation {
                    channel: after.channel.clone(),
                    source_device: after.source_device.clone(),
                }
            }
        })
        .or_else(|| request.after.clone())
}
fn install_owner_group(owner: &Node, external: &Node, group: ConversationId) {
    let pending = DeviceIdentity::generate();
    let first = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Queued source".into(),
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
        vec![first.id],
        2,
        2,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Queued source".into(),
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
    for target in [owner, external] {
        install(target, &first);
        install(target, &invitation);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn newly_selected_queued_source_starts_fresh_before_replaying_its_old_partial_page() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let a = node(&dir.path().join("a"), &account);
    let b = node(&dir.path().join("b"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let group_a = new_channel_id();
    let group_b = new_channel_id();
    install_owner_group(&a, &external, group_a);
    install_owner_group(&b, &external, group_b);
    for left in [&a, &b, &reference] {
        for right in [&a, &b, &reference] {
            if left.user_id() != right.user_id() {
                link(left, right);
            }
        }
    }
    let devices = [
        ("a", a.clone()),
        ("b", b.clone()),
        ("reference", reference.clone()),
        ("external", external.clone()),
    ];
    let mut endpoints = Vec::new();
    let mut listeners = Vec::new();
    for (name, target) in &devices {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        endpoints.push(address);
        listeners.push((target.clone(), listener));
    }
    for (_, left) in &devices {
        for ((_, right), address) in devices.iter().zip(&endpoints) {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Queued source fixture", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    let mut servers = tokio::task::JoinSet::new();
    for (target, listener) in listeners {
        servers.spawn(async move {
            Box::pin(target.run_accept_loop(listener)).await;
        });
    }
    a.publish_account_group_access(&mut None);
    b.publish_account_group_access(&mut None);
    super::message_action_reference::pull_reference_action_control(
        &reference,
        &a,
        endpoints[0],
        group_a,
    )
    .await;
    super::message_action_reference::pull_reference_action_control(
        &reference,
        &b,
        endpoints[1],
        group_b,
    )
    .await;
    super::cold_account_setup(async {
        loop {
            if [
                (&reference, &a),
                (&a, &reference),
                (&reference, &b),
                (&b, &reference),
            ]
            .into_iter()
            .all(|(node, peer)| {
                node.group_operation_versions
                    .lock()
                    .unwrap()
                    .get(&peer.user_id())
                    .is_some_and(|proof| {
                        proof.public == peer.identity.public()
                            && proof.kinds & AccountGroupOperationKind::Rename.capability_bit() != 0
                    })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cold real capabilities must precede queued-source admission");
    assert_eq!(
        reference.account_group_access(group_b).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(!reference.channel_can_manage(group_b));

    let release = Arc::new(Semaphore::new(0));
    let _release = ReleaseQueuedReads(release.clone());
    let (first_tx, first_rx) = oneshot::channel();
    let first_tx = Arc::new(Mutex::new(Some(first_tx)));
    let (resume_tx, resume_rx) = oneshot::channel();
    let resume_tx = Arc::new(Mutex::new(Some(resume_tx)));
    let b_partial = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saved_after = Arc::new(Mutex::new(None));
    let b_requests = Arc::new(Mutex::new(Vec::new()));
    let a_requests = Arc::new(Mutex::new(Vec::new()));
    let origins = Arc::new(Mutex::new(Vec::new()));
    let b_retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *reference.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let peer_a = a.identity.public();
        let peer_b = b.identity.public();
        let origins = origins.clone();
        let b_partial = b_partial.clone();
        let b_retired = b_retired.clone();
        move |peer, phase| {
            if peer == peer_a
                && (phase.starts_with("control_turn_origin")
                    || phase.starts_with("control_dispatch"))
            {
                let mut rows = origins.lock().unwrap();
                if rows.len() == 16 {
                    rows.remove(0);
                }
                rows.push(phase.clone());
            }
            if peer == peer_b
                && phase.starts_with("page_end")
                && phase.contains("pending=true after=true")
            {
                b_partial.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if peer == peer_b && phase == "control_pair_retired" {
                b_retired.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }));
    *a.account_session_test_hook.lock().unwrap() =
        Some(Arc::new({
            let expected = reference.identity.public();
            let release = release.clone();
            let first_tx = first_tx.clone();
            let resume_tx = resume_tx.clone();
            let b_partial = b_partial.clone();
            let origins = origins.clone();
            let a_requests = a_requests.clone();
            move |peer, frame| {
                let expected = expected.clone();
                let release = release.clone();
                let first_tx = first_tx.clone();
                let resume_tx = resume_tx.clone();
                let b_partial = b_partial.clone();
                let origins = origins.clone();
                let a_requests = a_requests.clone();
                Box::pin(async move {
                    if peer != expected {
                        return;
                    }
                    let AccountHistoryFrame::DocumentsRequest { body } = frame else {
                        return;
                    };
                    let Some(request) = DocumentRequest::decode(&body)
                        .filter(|request| request.domain == AccountDocumentDomain::GroupControl)
                    else {
                        return;
                    };
                    let first_pending = first_tx.lock().unwrap().is_some();
                    if first_pending
                        && !origins.lock().unwrap().last().is_some_and(|phase| {
                            phase.starts_with("control_dispatch protected=false")
                        })
                    {
                        return;
                    }
                    a_requests.lock().unwrap().push((
                        request.version,
                        effective_after(&request),
                        request.known_digest,
                        request.operation.as_ref().map(|probe| probe.known_digest),
                    ));
                    let first = first_tx.lock().unwrap().take();
                    if let Some(first) = first {
                        assert!(effective_after(&request).is_none());
                        let _ = first.send(());
                        let _ = release.acquire().await;
                    } else if b_partial.load(std::sync::atomic::Ordering::Relaxed) {
                        let resumed = resume_tx.lock().unwrap().take();
                        if let Some(resumed) = resumed {
                            assert!(
                                origins
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .rev()
                                    .take(2)
                                    .any(|phase| phase
                                        == "control_turn_origin resumed=true replacement=false")
                            );
                            let _ = resumed.send(());
                            let _ = release.acquire().await;
                        }
                    }
                })
            }
        }));
    *b.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let expected = reference.identity.public();
        let admitted = admitted.clone();
        let saved_after = saved_after.clone();
        let b_requests = b_requests.clone();
        move |peer, frame| {
            let expected = expected.clone();
            let admitted = admitted.clone();
            let saved_after = saved_after.clone();
            let b_requests = b_requests.clone();
            Box::pin(async move {
                if peer != expected {
                    return;
                }
                match frame {
                    AccountHistoryFrame::DocumentsComplete { body }
                        if !admitted.load(std::sync::atomic::Ordering::Relaxed) =>
                    {
                        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        if value["domain"] == "group_control" && !value["next"].is_null() {
                            let key: crate::node::AccountDocumentKey =
                                serde_json::from_value(value["next"].clone()).unwrap();
                            if matches!(
                                key,
                                crate::node::AccountDocumentKey::GroupLegacyOperation { .. }
                            ) {
                                *saved_after.lock().unwrap() = Some(key);
                            }
                        }
                    }
                    AccountHistoryFrame::DocumentsRequest { body }
                        if admitted.load(std::sync::atomic::Ordering::Relaxed) =>
                    {
                        if let Some(request) = DocumentRequest::decode(&body)
                            .filter(|request| request.domain == AccountDocumentDomain::GroupControl)
                        {
                            let mut rows = b_requests.lock().unwrap();
                            if rows.len() < 32 {
                                rows.push(effective_after(&request));
                            }
                        }
                    }
                    _ => {}
                }
            })
        }
    }));
    reference.account_sync_notify.notify_one();
    super::cold_account_setup(first_rx)
        .await
        .expect("natural A read must precede real B publication")
        .unwrap();
    let keys = crate::node::account_documents::sync::publish_legacy_control_page_for_test(&b, 66);
    super::cold_account_setup(resume_rx)
        .await
        .expect("B protected partial page must be queued before A resumes")
        .unwrap();
    assert!(b_partial.load(std::sync::atomic::Ordering::Relaxed));
    let saved = saved_after
        .lock()
        .unwrap()
        .clone()
        .expect("a real B Complete must retain its legacy continuation");
    let result_key = crate::node::AccountDocumentKey::GroupOperation {
        channel: hex::encode(group_b.as_bytes()),
        source_device: b.user_id(),
    };
    assert!(
        result_key < saved,
        "the new result is behind the old queued suffix"
    );
    assert!(reference.pending_account_operation_sources().is_empty());
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    admitted.store(true, std::sync::atomic::Ordering::Relaxed);
    let accepted = reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group_b,
            AccountGroupOperation::Rename {
                name: "Queued foreground".into(),
            },
            |commit| commit(),
        )
        .unwrap();
    assert_eq!(accepted.executions.len(), 1);
    assert_eq!(accepted.executions[0].device_id, b.user_id());
    let receipt = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let local = reference
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            let remote = b
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            if let (Some(local), Some(remote)) = (local, remote)
                && local.phase == AccountGroupOperationPhase::Committed
                && remote.phase == AccountGroupOperationPhase::Committed
                && local.committed_events == remote.committed_events
                && reference
                    .account_group_operation_inputs()
                    .into_iter()
                    .any(|(_, _, frame)| {
                        frame.group == group_b
                            && frame.proof.public() == b.identity.public()
                            && frame.projection.results.iter().any(|row| {
                                row.reference.id == id
                                    && row.status.phase == AccountGroupOperationPhase::Committed
                                    && row.status.committed_events == local.committed_events
                            })
                    })
            {
                break local;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    release.close();
    let first_fresh = b_requests
        .lock()
        .unwrap()
        .first()
        .is_some_and(Option::is_none);
    let recovered = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if keys
                .iter()
                .all(|key| reference.account_document_meta(key).is_some())
                && external
                    .channels
                    .lock()
                    .unwrap()
                    .state(&group_b)
                    .is_some_and(|state| state.name() == "Queued foreground")
                && (!first_fresh || b_retired.load(std::sync::atomic::Ordering::Relaxed))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    *a.account_session_test_hook.lock().unwrap() = None;
    *b.account_session_test_hook.lock().unwrap() = None;
    *reference.account_client_test_hook.lock().unwrap() = None;
    for (_, node) in &devices {
        node.runtime_work.close();
    }
    servers.shutdown().await;
    for (_, node) in &devices {
        tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .expect("all queued-source owners drain");
    }
    assert!(
        recovered.is_ok(),
        "releasing A must restore actual public receipt and complete all signed rows"
    );
    assert!(
        first_fresh,
        "new selected queued source resumed its OLD suffix instead of a fresh comparison: saved={saved:?}, requests={:?}",
        b_requests.lock().unwrap()
    );
    let result=receipt.expect("queued selected source must return its real private receipt within unchanged eight seconds");
    assert_eq!(result.committed_events.len(), 1);
    let event = b
        .log
        .lock()
        .unwrap()
        .get(&result.committed_events[0])
        .unwrap()
        .clone();
    assert!(event.verify_integrity() && event.verify_signature());
    assert_eq!(event.kind, EventKind::ChannelRename);
    assert_eq!(event.author.ed25519_pub(), &b.identity.public().ed25519_pub);
    assert!(
        b_requests
            .lock()
            .unwrap()
            .iter()
            .any(|after| after.as_ref() == Some(&saved)),
        "old B continuation must be replayed exactly"
    );
    let a_rows = a_requests.lock().unwrap();
    assert!(
        a_rows.len() >= 2 && a_rows[0] == a_rows[1],
        "A protected resume repays its exact checkpoint"
    );
}
