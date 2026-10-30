//! A queued pre-operation suffix does not witness a newly admitted request.
use super::selected_same_source_control_latency::{
    BTrace, ControlTestPages, record_b_trace, record_control_frame, result_snapshot,
};
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
    Box::pin(run_queued_source(false, false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn newer_terminal_reuses_completed_background_before_stale_foreground_suffix() {
    Box::pin(run_queued_source(true, false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_source_ignores_retired_natural_request_before_capturing_live_resume() {
    Box::pin(run_queued_source(true, true)).await;
}

async fn run_queued_source(gate_terminal: bool, delay_retired_request: bool) {
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
    let resumed_task = Arc::new(Mutex::new(None::<tokio::task::Id>));
    let b_partial = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delayed_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let retired_request_rejected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (rejected_tx, rejected_rx) = oneshot::channel();
    let rejected_tx = Arc::new(Mutex::new(Some(rejected_tx)));
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saved_after = Arc::new(Mutex::new(None));
    let b_requests = Arc::new(Mutex::new(Vec::new()));
    let b_dispatches = Arc::new(Mutex::new(Vec::new()));
    let a_requests = Arc::new(Mutex::new(Vec::new()));
    let origins = Arc::new(Mutex::new(Vec::new()));
    let b_retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let clock = std::time::Instant::now();
    let operation = Arc::new(Mutex::new(None));
    let pages = Arc::new(Mutex::new(ControlTestPages::default()));
    let demand_dispatch = Arc::new(Mutex::new(None));
    let b_unpaired_settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let first_foreground = Arc::new(Mutex::new(None::<tokio::task::Id>));
    let foreground_after = Arc::new(Mutex::new(None::<crate::node::AccountDocumentKey>));
    let release_intent = Arc::new(Semaphore::new(0));
    let _release_intent = ReleaseQueuedReads(release_intent.clone());
    let intent_held = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let intent_entered = Arc::new(Semaphore::new(0));
    let _release_entered = ReleaseQueuedReads(intent_entered.clone());
    let terminal_published = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let terminal_wake = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wake_changed = Arc::new(Semaphore::new(0));
    let _release_wake = ReleaseQueuedReads(wake_changed.clone());
    *b.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let source = Arc::downgrade(&b);
        let operation = operation.clone();
        let trace = trace.clone();
        let peer = reference.identity.public();
        let terminal_published = terminal_published.clone();
        move |observed, phase| {
            if phase.starts_with("operation_local_published") {
                if let Some(source) = source.upgrade() {
                    let snapshot = result_snapshot(&source, group_b, *operation.lock().unwrap());
                    if snapshot.contains("matching_terminal=true") {
                        terminal_published.store(true, std::sync::atomic::Ordering::Release);
                    }
                    record_b_trace(&trace, clock, "B_publication", snapshot);
                }
            } else if observed == peer
                && ((phase.starts_with("control_")
                    && !phase.starts_with("control_queue")
                    && !phase.starts_with("control_refresh")
                    && !phase.starts_with("control_request_body="))
                    || phase.starts_with("wake_")
                    || phase.starts_with("negotiation_"))
            {
                record_b_trace(&trace, clock, "B_to_requester", phase);
            }
        }
    }));
    *reference.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let trace = trace.clone();
        let peer = b.identity.public();
        let writer = b.user_id();
        let requester_writer = reference.user_id();
        let operation = operation.clone();
        let release_intent = release_intent.clone();
        let intent_held = intent_held.clone();
        let intent_entered = intent_entered.clone();
        move |observed, frame| {
            let trace = trace.clone();
            let peer = peer.clone();
            let writer = writer.clone();
            let requester_writer = requester_writer.clone();
            let operation = operation.clone();
            let release_intent = release_intent.clone();
            let intent_held = intent_held.clone();
            let intent_entered = intent_entered.clone();
            Box::pin(async move {
                // Arm before durable admission and hold every copy at its origin:
                // A can relay this signed head to B, and concurrent reads must
                // not bypass the first held header. Keep the topology unchanged.
                let intent_armed = operation.lock().unwrap().is_some();
                if gate_terminal && intent_armed
                    && let AccountHistoryFrame::DocumentHeader { body } = &frame
                    && serde_json::from_slice::<crate::node::AccountDocumentMeta>(body).ok().is_some_and(|meta| matches!(&meta.key, crate::node::AccountDocumentKey::GroupOperation { channel, source_device } if channel == &hex::encode(group_b.as_bytes()) && source_device == &requester_writer))
                {
                    if !release_intent.is_closed()
                        && !intent_held.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        intent_entered.close();
                        record_b_trace(&trace, clock, "fixture", "source_intent_header_held_until_foreground_complete".into());
                    }
                    let _ = release_intent.acquire().await;
                }
                if observed == peer {
                    record_control_frame(
                        &trace,
                        clock,
                        "requester_receive",
                        &frame,
                        group_b,
                        &writer,
                    );
                }
            })
        }
    }));
    *reference.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let peer_a = a.identity.public();
        let peer_b = b.identity.public();
        let origins = origins.clone();
        let b_partial = b_partial.clone();
        let b_retired = b_retired.clone();
        let trace = trace.clone();
        let admitted = admitted.clone();
        let first_foreground = first_foreground.clone();
        let release_intent = release_intent.clone();
        let terminal_published = terminal_published.clone();
        let intent_held = intent_held.clone();
        let terminal_wake = terminal_wake.clone();
        let wake_changed = wake_changed.clone();
        let pages = pages.clone();
        let operation = operation.clone();
        let b_unpaired_settled = b_unpaired_settled.clone();
        let demand_dispatch = demand_dispatch.clone();
        move |peer, phase| {
            if peer == peer_a || peer == peer_b {
                pages.lock().unwrap().observe(&phase);
            }
            if peer == peer_b
                && phase.starts_with("control_request_body=")
                && let Some(page) = pages.lock().unwrap().task(tokio::task::id())
                && operation
                    .lock()
                    .unwrap()
                    .is_some_and(|id| page.contains_demand(id))
            {
                // Open the window at the exact committed-demand dispatch.
                // Later recovery pages remain observable after it completes.
                demand_dispatch
                    .lock()
                    .unwrap()
                    .get_or_insert(page.dispatched_at);
            }
            if peer == peer_b
                && phase.starts_with("page_end")
                && phase.contains("outcome=ok")
                && phase.contains("pending=false after=false")
                && pages
                    .lock()
                    .unwrap()
                    .task(tokio::task::id())
                    .is_some_and(|page| page.origin.starts_with("control_task paired=false "))
            {
                b_unpaired_settled.store(true, std::sync::atomic::Ordering::Release);
            }
            if gate_terminal && peer == peer_b {
                if phase.starts_with("control_task ")
                    && operation
                        .lock()
                        .unwrap()
                        .is_some_and(|id| phase.contains(&format!("{id:?}")))
                {
                    let mut first = first_foreground.lock().unwrap();
                    if first.is_none() {
                        *first = tokio::task::try_id();
                    }
                }
                if phase.starts_with("page_end")
                    && phase.contains("pending=true after=true")
                    && tokio::task::try_id() == *first_foreground.lock().unwrap()
                {
                    record_b_trace(
                        &trace,
                        clock,
                        "fixture",
                        "first_foreground_partial_validated_release_source_intent".into(),
                    );
                    assert!(
                        intent_held.load(std::sync::atomic::Ordering::Acquire)
                            && !terminal_published.load(std::sync::atomic::Ordering::Acquire),
                        "the executor terminal must be newer than the first foreground page; phases={:?}",
                        trace.lock().unwrap()
                    );
                    release_intent.close();
                }
                if phase == "control_pair_followup_observed"
                    && terminal_published.load(std::sync::atomic::Ordering::Acquire)
                {
                    terminal_wake.store(true, std::sync::atomic::Ordering::Release);
                    wake_changed.add_permits(1);
                }
            }
            if (peer == peer_a || peer == peer_b)
                && ((phase.starts_with("control_")
                    && !phase.starts_with("control_queue")
                    && !phase.starts_with("control_refresh")
                    && !phase.starts_with("control_request_body="))
                    || phase.starts_with("wake_")
                    || phase.starts_with("negotiation_")
                    || phase.starts_with("page_end"))
            {
                record_b_trace(
                    &trace,
                    clock,
                    if peer == peer_a {
                        "requester_to_A"
                    } else {
                        "requester_to_B"
                    },
                    phase.clone(),
                );
            }
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
            if peer == peer_b
                && phase == "control_pair_retired"
                && admitted.load(std::sync::atomic::Ordering::Acquire)
            {
                b_retired.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }));
    *a.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let expected = reference.identity.public();
        let release = release.clone();
        let first_tx = first_tx.clone();
        let resume_tx = resume_tx.clone();
        let b_partial = b_partial.clone();
        let a_requests = a_requests.clone();
        let pages = pages.clone();
        let b_unpaired_settled = b_unpaired_settled.clone();
        let delayed_once = delayed_once.clone();
        let publisher = Arc::downgrade(&b);
        let resumed_task = resumed_task.clone();
        let retired_request_rejected = retired_request_rejected.clone();
        let rejected_tx = rejected_tx.clone();
        move |peer, frame| {
            let expected = expected.clone();
            let release = release.clone();
            let first_tx = first_tx.clone();
            let resume_tx = resume_tx.clone();
            let b_partial = b_partial.clone();
            let a_requests = a_requests.clone();
            let pages = pages.clone();
            let b_unpaired_settled = b_unpaired_settled.clone();
            let delayed_once = delayed_once.clone();
            let publisher = publisher.clone();
            let resumed_task = resumed_task.clone();
            let retired_request_rejected = retired_request_rejected.clone();
            let rejected_tx = rejected_tx.clone();
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
                    && (!b_unpaired_settled.load(std::sync::atomic::Ordering::Acquire)
                        || !pages.lock().unwrap().request(&request).is_some_and(|page| {
                            page.dispatch()
                                .is_some_and(|dispatch| !dispatch.paired && !dispatch.protected)
                        }))
                {
                    return;
                }
                let mut delayed_this_request = false;
                if first_pending
                    && delay_retired_request
                    && !delayed_once.swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    let page = pages.lock().unwrap().request(&request).unwrap();
                    // A real authenticated publication cancels this exact old
                    // client while its acknowledged server callback is delayed.
                    // The head is one of the same 66 later published below.
                    let publisher = publisher.upgrade().unwrap();
                    crate::node::account_documents::sync::publish_legacy_control_page_for_test(
                        &publisher, 1,
                    );
                    while pages.lock().unwrap().is_active(page.task) {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    delayed_this_request = true;
                }
                let Some(page) = pages.lock().unwrap().live_request(&request) else {
                    if delayed_this_request {
                        retired_request_rejected.store(true, std::sync::atomic::Ordering::Release);
                        if let Some(rejected) = rejected_tx.lock().unwrap().take() {
                            let _ = rejected.send(());
                        }
                    }
                    return;
                };
                assert!(
                    !delayed_this_request,
                    "a retired child must not establish the natural-read hold"
                );
                let Some(dispatch) = page.dispatch() else {
                    return;
                };
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
                } else if b_partial.load(std::sync::atomic::Ordering::Relaxed)
                    && dispatch.resumed
                    && !dispatch.replacement
                    && dispatch.protected
                {
                    let resumed = resume_tx.lock().unwrap().take();
                    if let Some(resumed) = resumed {
                        *resumed_task.lock().unwrap() = Some(page.task);
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
        let b_dispatches = b_dispatches.clone();
        let trace = trace.clone();
        let source = Arc::downgrade(&b);
        let writer = b.user_id();
        let operation = operation.clone();
        let terminal_wake = terminal_wake.clone();
        let wake_changed = wake_changed.clone();
        let foreground_after = foreground_after.clone();
        let pages = pages.clone();
        let demand_dispatch = demand_dispatch.clone();
        let intent_entered = intent_entered.clone();
        move |peer, frame| {
            let expected = expected.clone();
            let admitted = admitted.clone();
            let saved_after = saved_after.clone();
            let b_requests = b_requests.clone();
            let b_dispatches = b_dispatches.clone();
            let trace = trace.clone();
            let source = source.clone();
            let writer = writer.clone();
            let operation = operation.clone();
            let terminal_wake = terminal_wake.clone();
            let wake_changed = wake_changed.clone();
            let foreground_after = foreground_after.clone();
            let pages = pages.clone();
            let demand_dispatch = demand_dispatch.clone();
            let intent_entered = intent_entered.clone();
            Box::pin(async move {
                if peer != expected {
                    return;
                }
                record_control_frame(&trace, clock, "B_server", &frame, group_b, &writer);
                if let AccountHistoryFrame::DocumentsRequest { body } = &frame
                    && DocumentRequest::decode(body).is_some_and(|request| {
                        request.domain == AccountDocumentDomain::GroupControl
                    })
                    && let Some(source) = source.upgrade()
                {
                    record_b_trace(
                        &trace,
                        clock,
                        "B_snapshot_start",
                        result_snapshot(&source, group_b, *operation.lock().unwrap()),
                    );
                }
                if gate_terminal && let AccountHistoryFrame::DocumentsComplete { body } = &frame {
                    let complete: serde_json::Value = serde_json::from_slice(body).unwrap();
                    let page =
                        serde_json::from_value::<[u8; 32]>(complete["operation"]["nonce"].clone())
                            .ok()
                            .and_then(|nonce| pages.lock().unwrap().nonce(nonce))
                            .filter(|page| {
                                demand_dispatch
                                    .lock()
                                    .unwrap()
                                    .is_some_and(|at| page.dispatched_at >= at)
                            });
                    if complete["domain"] == "group_control"
                        && let Some(page) = page
                    {
                        let last = Some(effective_after(&page.request));
                        if last == Some(None)
                            && !complete["next"].is_null()
                            && foreground_after.lock().unwrap().is_none()
                        {
                            *foreground_after.lock().unwrap() =
                                Some(serde_json::from_value(complete["next"].clone()).unwrap());
                            // Establish the intended interleaving with real
                            // frames, without depending on which route wins.
                            let _ = intent_entered.acquire().await;
                        }
                        if last.is_some_and(|after| after.is_some())
                            && complete["next"].is_null()
                            && !terminal_wake.load(std::sync::atomic::Ordering::Acquire)
                        {
                            let _ = wake_changed.acquire().await;
                        }
                    }
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
                    AccountHistoryFrame::DocumentsRequest { body } => {
                        if let Some(request) = DocumentRequest::decode(&body)
                            .filter(|request| request.domain == AccountDocumentDomain::GroupControl)
                            && let Some(page) = pages.lock().unwrap().request(&request)
                            && demand_dispatch
                                .lock()
                                .unwrap()
                                .is_some_and(|at| page.dispatched_at >= at)
                        {
                            let mut rows = b_requests.lock().unwrap();
                            if rows.len() < 32 {
                                b_dispatches.lock().unwrap().push((
                                    page.task,
                                    page.origin,
                                    request.operation.as_ref().unwrap().nonce,
                                ));
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
    super::cold_account_setup(async {
        first_rx.await?;
        if delay_retired_request { rejected_rx.await?; }
        Ok::<(), oneshot::error::RecvError>(())
    })
        .await
        .unwrap_or_else(|error| panic!("natural A read must precede real B publication: {error:?}; B_unpaired_settled={} B_partial={}; pages={:?}", b_unpaired_settled.load(std::sync::atomic::Ordering::Acquire), b_partial.load(std::sync::atomic::Ordering::Acquire), pages.lock().unwrap().summary()))
        .unwrap();
    if delay_retired_request {
        assert!(
            retired_request_rejected.load(std::sync::atomic::Ordering::Acquire),
            "the delayed historical request is rejected before canonical publication"
        );
    }
    let keys = crate::node::account_documents::sync::publish_legacy_control_page_for_test(&b, 66);
    super::cold_account_setup(resume_rx)
        .await
        .unwrap_or_else(|error| panic!("B protected partial page must be queued before A resumes: {error:?}; B_unpaired_settled={} B_partial={}; A_roles={:?}; pages={:?}", b_unpaired_settled.load(std::sync::atomic::Ordering::Acquire), b_partial.load(std::sync::atomic::Ordering::Acquire), origins.lock().unwrap(), pages.lock().unwrap().summary()))
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
    let intent_key = crate::node::AccountDocumentKey::GroupOperation {
        channel: hex::encode(group_b.as_bytes()),
        source_device: reference.user_id(),
    };
    assert!(
        [&reference, &a, &b]
            .into_iter()
            .all(|node| node.account_document_meta(&intent_key).is_none()),
        "the header gate must hold the new request, not an existing empty head"
    );
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    *operation.lock().unwrap() = Some(id);
    record_b_trace(&trace, clock, "fixture", "B_admission_begin".into());
    b_retired.store(false, std::sync::atomic::Ordering::Relaxed);
    let accepted = reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group_b,
            AccountGroupOperation::Rename {
                name: "Queued foreground".into(),
            },
            |commit| {
                let task = resumed_task
                    .lock()
                    .unwrap()
                    .expect("the exact protected resume is held");
                assert!(
                    pages.lock().unwrap().is_active(task),
                    "the exact held resume must be live at admission"
                );
                commit()
            },
        )
        .unwrap();
    admitted.store(true, std::sync::atomic::Ordering::Release);
    record_b_trace(&trace, clock, "fixture", "B_admitted".into());
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
    record_b_trace(
        &trace,
        clock,
        "fixture",
        if receipt.is_ok() {
            "B_private_receipt"
        } else {
            "B_deadline_expired"
        }
        .into(),
    );
    release_intent.close();
    intent_entered.close();
    wake_changed.close();
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
    *b.account_client_test_hook.lock().unwrap() = None;
    *reference.account_session_test_hook.lock().unwrap() = None;
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
        "releasing A must restore actual public receipt and complete all signed rows; phases={:?}",
        trace.lock().unwrap()
    );
    assert!(
        first_fresh,
        "new selected queued source resumed its OLD suffix instead of a fresh comparison: saved={saved:?}, requests={:?}; dispatches={:?}",
        b_requests.lock().unwrap(),
        b_dispatches.lock().unwrap()
    );
    if gate_terminal {
        assert!(
            intent_held.load(std::sync::atomic::Ordering::Acquire),
            "the real source intent header was held before executor admission; phases={:?}",
            trace.lock().unwrap()
        );
        assert!(terminal_wake.load(std::sync::atomic::Ordering::Acquire));
        let requests = b_requests.lock().unwrap();
        assert!(
            requests.get(2).is_some_and(Option::is_none),
            "new terminal must reuse completed background slot before old foreground suffix: requests={requests:?}; dispatches={:?}; phases={:?}",
            b_dispatches.lock().unwrap(),
            trace.lock().unwrap()
        );
        let saved_foreground = foreground_after
            .lock()
            .unwrap()
            .clone()
            .expect("first fresh foreground has a real continuation");
        assert!(
            requests
                .iter()
                .skip(3)
                .any(|after| after.as_ref() == Some(&saved_foreground)),
            "old foreground continuation must still be replayed exactly: saved={saved_foreground:?} requests={requests:?}; phases={:?}",
            trace.lock().unwrap()
        );
    }
    let result=receipt.unwrap_or_else(|error|panic!("queued selected source must return its real private receipt within unchanged eight seconds: {error:?}; phases={:?}",trace.lock().unwrap()));
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
