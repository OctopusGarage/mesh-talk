//! A new explicit receipt must not inherit a pre-operation background read.
use super::*;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

struct ReleaseReadOnDrop(Arc<Semaphore>);
impl Drop for ReleaseReadOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_source_receipt_overtakes_its_own_preoperation_background_control_read() {
    run_same_source_receipt(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_source_lower_key_receipt_preserves_both_partial_control_comparisons() {
    run_same_source_receipt(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_selected_executor_overtakes_a_completed_operations_paired_background_page() {
    run_same_source_receipt(true, true).await;
}

async fn run_same_source_receipt(partial: bool, other_selected: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let other = other_selected.then(|| node(&dir.path().join("other"), &account));
    let other_group = new_channel_id();
    let group = new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Before same-source hold".into(),
            members: vec![owner.identity.public(), external.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
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
            name: "Before same-source hold".into(),
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
        install(target, &invitation);
    }
    link(&owner, &reference);
    link(&reference, &owner);
    if let Some(other) = &other {
        for peer in [&owner, &reference] {
            link(other, peer);
            link(peer, other);
        }
        let first = Event::new(
            &other.identity,
            other_group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Other executor".into(),
                members: vec![other.identity.public(), external.identity.public()],
                epoch: 0,
                owner: other.user_id(),
            }
            .encode(),
        );
        let invited = Event::new(
            &other.identity,
            other_group,
            2,
            vec![first.id],
            2,
            2,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Other executor".into(),
                members: vec![
                    other.identity.public(),
                    external.identity.public(),
                    pending.public(),
                ],
                epoch: 1,
                owner: other.user_id(),
            }
            .encode_invitation(),
        );
        for target in [other, &external] {
            install(target, &first);
            install(target, &invited);
        }
    }
    let mut devices = vec![
        ("owner", owner.clone()),
        ("reference", reference.clone()),
        ("external", external.clone()),
    ];
    if let Some(other) = &other {
        devices.push(("other", other.clone()));
    }
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
                    &right.signed_announce("Same-source fixture", address.port()),
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
    owner.publish_account_group_access(&mut None);
    super::message_action_reference::pull_reference_action_control(
        &reference,
        &owner,
        endpoints[0],
        group,
    )
    .await;
    if let Some(other) = &other {
        other.publish_account_group_access(&mut None);
        super::message_action_reference::pull_reference_action_control(
            &reference,
            other,
            endpoints[3],
            other_group,
        )
        .await;
    }
    super::cold_account_setup(async {
        loop {
            let mut required = vec![(&owner, &reference), (&reference, &owner)];
            if let Some(other) = &other {
                required.extend([(other, &reference), (&reference, other)]);
            }
            let ready = required.into_iter().all(|(node, peer)| {
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
    .expect("both real certified operation capabilities must precede admission");
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(reference.channel_members(group).is_empty());
    assert!(reference.pending_account_operation_sources().is_empty());

    let classes = Arc::new(Mutex::new(Vec::new()));
    let aborts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *reference.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let classes = classes.clone();
        let aborts = aborts.clone();
        let retired = retired.clone();
        let exact_owner = owner.identity.public();
        move |peer, phase| {
            if peer != exact_owner {
                return;
            }
            if phase.starts_with("control_turn_origin") || phase.starts_with("control_dispatch") {
                let mut rows = classes.lock().unwrap();
                if rows.len() == 16 {
                    rows.remove(0);
                }
                rows.push(phase);
            } else if phase == "control_pair_retired" {
                retired.store(true, std::sync::atomic::Ordering::Relaxed);
            } else if phase.starts_with("control_owned_abort reason=") {
                aborts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }));
    let release = Arc::new(Semaphore::new(0));
    let _release_on_drop = ReleaseReadOnDrop(release.clone());
    let (arrived, held) = oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let (resumed_signal, resumed_wait) = oneshot::channel();
    let resumed_signal = Arc::new(Mutex::new(Some(resumed_signal)));
    let resumed_release = Arc::new(Semaphore::new(0));
    let _resumed_release_on_drop = ReleaseReadOnDrop(resumed_release.clone());
    let active_operation = Arc::new(Mutex::new(None));
    let checkpoints = Arc::new(Mutex::new(Vec::new()));
    let partial_keys = Arc::new(Mutex::new(Vec::new()));
    let seeded = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *owner.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let release = release.clone();
        let checkpoints = checkpoints.clone();
        let classes = classes.clone();
        let exact_reference = reference.identity.public();
        let owner_id = owner.user_id();
        let source = Arc::downgrade(&owner);
        let requester = Arc::downgrade(&reference);
        let active_operation = active_operation.clone();
        let resumed_signal = resumed_signal.clone();
        let resumed_release = resumed_release.clone();
        let partial_keys = partial_keys.clone();
        let seeded = seeded.clone();
        move |peer, frame| {
            let owner_id = owner_id.clone();
            let source = source.clone();
            let requester = requester.clone();
            let active_operation = active_operation.clone();
            let resumed_signal = resumed_signal.clone();
            let resumed_release = resumed_release.clone();
            let partial_keys = partial_keys.clone();
            let seeded = seeded.clone();
            let release = release.clone();
            let arrived = arrived.clone();
            let checkpoints = checkpoints.clone();
            let classes = classes.clone();
            let exact_reference = exact_reference.clone();
            Box::pin(async move {
                if peer != exact_reference {
                    return;
                }
                let AccountHistoryFrame::DocumentsRequest { body } = frame else {
                    return;
                };
                let request =
                    crate::node::account_documents::wire::DocumentRequest::decode(&body).unwrap();
                if request.domain != AccountDocumentDomain::GroupControl {
                    return;
                }
                // A request already dispatched before the observer was installed
                // is allowed to finish; hold only a positively observed natural turn.
                if !classes
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|phase| phase.starts_with("control_dispatch protected=false"))
                {
                    return;
                }
                let effective_after = request.operation.as_ref().and_then(|probe| probe.after.as_ref()).map(|after| {
                    if after.legacy_leave {
                        crate::node::account_documents::AccountDocumentKey::GroupLegacyOperation { channel: after.channel.clone(), source_device: after.source_device.clone() }
                    } else {
                        crate::node::account_documents::AccountDocumentKey::GroupOperation { channel: after.channel.clone(), source_device: after.source_device.clone() }
                    }
                }).or_else(|| request.after.clone());
                let admitted = *active_operation.lock().unwrap();
                if other_selected
                    && effective_after.is_none()
                    && let Some(id) = admitted
                {
                    // Isolate post-terminal fairness: the foreground READ
                    // snapshots after a real signed terminal publication.
                    // This observes a local watch, without another RPC.
                    let source = source.upgrade().unwrap();
                    let mut changed = source.subscribe_account_documents();
                    loop {
                        let key =
                            crate::node::account_documents::AccountDocumentKey::GroupOperation {
                                channel: hex::encode(group.as_bytes()),
                                source_device: source.user_id(),
                            };
                        let published = source.account_document(&key).unwrap().and_then(|document| crate::node::account_group::operations::wire::OperationFrame::decode(document.payload(), &source.account.public().ed25519_pub));
                        if published.is_some_and(|frame| {
                            frame.proof.public() == source.identity.public()
                                && frame.group == group
                                && frame.projection.results.iter().any(|result| {
                                    result.reference.id == id
                                        && result.status.phase
                                            == AccountGroupOperationPhase::Committed
                                })
                        }) {
                            break;
                        }
                        if changed.changed().await.is_err() {
                            return;
                        }
                    }
                }
                if partial
                    && effective_after.is_none()
                    && classes
                        .lock()
                        .unwrap()
                        .last()
                        .is_some_and(|phase| phase.starts_with("control_dispatch protected=false"))
                    && !seeded.swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    // Insert before this natural request's immutable snapshot;
                    // earlier protected cold-start comparisons cannot consume
                    // the intended partial checkpoint before we observe it.
                    let source = source.upgrade().unwrap();
                    for value in 1..=66u8 {
                        partial_keys.lock().unwrap().push(
                            crate::node::account_documents::sync::seed_legacy_control_page_for_test(
                                &source,
                                crate::eventlog::ConversationId::new([value; 32]),
                            ),
                        );
                    }
                }
                if partial && !matches!(&effective_after, Some(crate::node::account_documents::AccountDocumentKey::GroupLegacyOperation { .. })) {
                    return;
                }
                checkpoints.lock().unwrap().push((
                    request.version,
                    effective_after.clone(),
                    request.known_digest,
                    request.operation.as_ref().map(|probe| probe.known_digest),
                ));
                let signal = arrived.lock().unwrap().take();
                if let Some(signal) = signal {
                    // This is a completed SessionReady and an authenticated
                    // read, not an accepted operation or mutation transaction.
                    if partial {
                        let operation_key =
                            crate::node::account_documents::AccountDocumentKey::GroupOperation {
                                channel: hex::encode(group.as_bytes()),
                                source_device: owner_id.clone(),
                            };
                        assert!(
                            effective_after
                                .as_ref()
                                .is_some_and(|after| operation_key < *after),
                            "the new terminal result must sort before the preserved partial checkpoint"
                        );
                    } else {
                        assert!(
                            request.after.is_none(),
                            "the original case captures a fresh background comparison"
                        );
                    }
                    let _ = signal.send(());
                    let _ = release.acquire().await;
                } else if other_selected
                    && admitted.is_some_and(|id| {
                        requester.upgrade().is_some_and(|requester| {
                            requester.account_group_operation_inputs().into_iter().any(
                                |(_, _, frame)| {
                                    frame.group == group
                                        && frame.projection.results.iter().any(|result| {
                                            result.reference.id == id
                                                && result.status.phase
                                                    == AccountGroupOperationPhase::Committed
                                        })
                                },
                            )
                        })
                    })
                {
                    let signal = resumed_signal.lock().unwrap().take();
                    if let Some(signal) = signal {
                        assert!(
                            classes.lock().unwrap().last().is_some_and(
                                |phase| phase.starts_with("control_dispatch protected=true")
                            ),
                            "the paired background page must be protected from publication hints"
                        );
                        let _ = signal.send(());
                        let _ = resumed_release.acquire().await;
                    }
                }
            })
        }
    }));
    reference.account_sync_notify.notify_one();
    super::cold_account_setup(held)
        .await
        .expect("the natural same-source background read must be held before admission")
        .unwrap();
    assert!(reference.pending_account_operation_sources().is_empty());
    let preoperation_classes = classes.lock().unwrap().clone();
    assert!(
        preoperation_classes
            .iter()
            .any(|phase| phase == "control_turn_origin resumed=false replacement=false")
    );
    assert!(
        preoperation_classes
            .iter()
            .any(|phase| phase.starts_with("control_dispatch protected=false"))
    );
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    *active_operation.lock().unwrap() = Some(id);
    reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "After same-source hold".into(),
            },
            |commit| commit(),
        )
        .unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(8), async {
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
                && requester.committed_events == source.committed_events
                && reference
                    .account_group_operation_inputs()
                    .into_iter()
                    .any(|(_, _, frame)| {
                        frame.group == group
                            && frame.proof.public() == owner.identity.public()
                            && frame.projection.results.iter().any(|result| {
                                result.reference.id == id
                                    && result.reference.origin.public()
                                        == reference.identity.public()
                                    && result.reference.executor.device == owner.identity.public()
                                    && result.status.phase == AccountGroupOperationPhase::Committed
                                    && result.status.committed_events == requester.committed_events
                            })
                    })
            {
                break (requester, source);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let before_release_requester = reference
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let before_release_source = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let mut second_id = None;
    let other_receipt = if let Some(other) = &other {
        if receipt.is_ok() {
            tokio::time::timeout(Duration::from_secs(8), resumed_wait)
                .await
                .expect("completed operation must yield a paired background page")
                .unwrap();
            assert!(
                !reference.pending_account_operation_sources().contains(&(
                    owner.identity.public().ed25519_pub,
                    owner.identity.public().x25519_pub
                )),
                "A is terminal before B is admitted"
            );
            let second = reference
                .new_account_group_operation_id(&account.account_id())
                .unwrap();
            second_id = Some(second);
            reference
                .submit_account_group_operation_if(
                    &account.account_id(),
                    second,
                    other_group,
                    AccountGroupOperation::Rename {
                        name: "Other executor overtook background".into(),
                    },
                    |commit| commit(),
                )
                .unwrap();
            Some(
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        let local = reference
                            .account_group_operation_status(&account.account_id(), second)
                            .unwrap();
                        let remote = other
                            .account_group_operation_status(&account.account_id(), second)
                            .unwrap();
                        if let (Some(local), Some(remote)) = (local, remote)
                            && local.phase == AccountGroupOperationPhase::Committed
                            && remote.phase == AccountGroupOperationPhase::Committed
                            && local.committed_events == remote.committed_events
                            && reference.account_group_operation_inputs().into_iter().any(
                                |(_, _, frame)| {
                                    frame.group == other_group
                                        && frame.proof.public() == other.identity.public()
                                        && frame.projection.results.iter().any(|result| {
                                            result.reference.id == second
                                                && result.status.phase
                                                    == AccountGroupOperationPhase::Committed
                                                && result.status.committed_events
                                                    == local.committed_events
                                        })
                                },
                            )
                        {
                            break local.committed_events;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await,
            )
        } else {
            None
        }
    } else {
        None
    };
    let aborts_before_release = aborts.load(std::sync::atomic::Ordering::Relaxed);
    let effects = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .filter(|event| event.kind == EventKind::ChannelRename)
        .cloned()
        .collect::<Vec<_>>();
    release.close();
    resumed_release.close();
    let private_failed = receipt.is_err();
    let recovered = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let receipt = reference
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            let public = external
                .channels
                .lock()
                .unwrap()
                .state(&group)
                .is_some_and(|state| state.name() == "After same-source hold");
            let partial_complete = partial_keys
                .lock()
                .unwrap()
                .iter()
                .all(|key| reference.account_document_meta(key).is_some());
            if checkpoints.lock().unwrap().len() >= 2
                && partial_complete
                && (!partial
                    || private_failed
                    || retired.load(std::sync::atomic::Ordering::Relaxed))
                && public
                && receipt
                    .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let second_phases = second_id.map(|second| {
        (
            reference
                .account_group_operation_status(&account.account_id(), second)
                .unwrap()
                .map(|status| status.phase),
            other
                .as_ref()
                .unwrap()
                .account_group_operation_status(&account.account_id(), second)
                .unwrap()
                .map(|status| status.phase),
        )
    });
    let recovery_snapshot = (
        receipt.is_ok(),
        other_receipt.as_ref().map(Result::is_ok),
        second_phases,
        retired.load(std::sync::atomic::Ordering::Relaxed),
        checkpoints.lock().unwrap().len(),
        partial_keys
            .lock()
            .unwrap()
            .iter()
            .filter(|key| reference.account_document_meta(key).is_some())
            .count(),
        aborts_before_release,
    );
    *owner.account_session_test_hook.lock().unwrap() = None;
    *reference.account_client_test_hook.lock().unwrap() = None;
    let preserved = {
        let rows = checkpoints.lock().unwrap();
        rows.len() >= 2 && rows[0] == rows[1]
    };
    for (_, node) in &devices {
        node.runtime_work.close();
    }
    servers.shutdown().await;
    for (_, node) in &devices {
        tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .expect("all original fixture owners must drain");
    }
    assert!(
        recovered.is_ok(),
        "releasing the old metadata read must restore ordinary progress and public receipt: {recovery_snapshot:?}"
    );
    assert!(
        before_release_source
            .as_ref()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed),
        "the real source must commit while its prior read is held"
    );
    let (requester, source) = receipt.unwrap_or_else(|_| panic!("same-source pre-operation background read blocked its new private receipt: requester={:?} source={:?}", before_release_requester.map(|status| status.phase), before_release_source.map(|status| status.phase)));
    assert_eq!(
        aborts_before_release,
        if other_selected { 2 } else { 1 },
        "the old background read yields exactly once despite repeated status polling"
    );
    assert!(
        preserved,
        "the authenticated comparison checkpoint must not be reset or skipped"
    );
    if other_selected {
        let events = other_receipt.expect("the second real operation was admitted after A committed")
            .expect("a new selected executor must overtake A's held paired background within its unchanged eight seconds");
        assert_eq!(events.len(), 1);
        let other = other.as_ref().unwrap();
        let event = other.log.lock().unwrap().get(&events[0]).unwrap().clone();
        assert!(event.verify_integrity() && event.verify_signature());
        assert_eq!(
            event.author.ed25519_pub(),
            &other.identity.public().ed25519_pub
        );
    }
    assert_eq!(effects.len(), 1);
    assert!(effects[0].verify_integrity() && effects[0].verify_signature());
    assert_eq!(
        effects[0].author.ed25519_pub(),
        &owner.identity.public().ed25519_pub
    );
    assert_eq!(requester.committed_events, vec![effects[0].id]);
    assert_eq!(source.committed_events, requester.committed_events);
}
