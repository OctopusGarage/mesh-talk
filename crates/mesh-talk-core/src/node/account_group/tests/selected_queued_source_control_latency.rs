//! A queued pre-operation suffix does not witness a newly admitted request.
use super::selected_same_source_control_latency::{
    BTrace, ColdControlScope, ControlTestPages, record_b_trace, record_control_frame,
    result_snapshot,
};
use super::*;
use crate::node::account_documents::wire::DocumentRequest;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::time::Duration;
use tokio::sync::{Notify, Semaphore, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QueuedRecoverySnapshot {
    rows_complete: bool,
    public_receipt: bool,
    pair_retired: bool,
}

impl QueuedRecoverySnapshot {
    fn complete(self) -> bool {
        self.rows_complete && self.public_receipt && self.pair_retired
    }
}

async fn observe_queued_recovery(
    changed: &Notify,
    mut snapshot: impl FnMut() -> QueuedRecoverySnapshot,
) {
    loop {
        // The final pair can retire between periodic checks just before the
        // deadline. Register before reading its state so that transition cannot
        // leave a complete recovery sleeping past the unchanged watchdog.
        let notification = changed.notified();
        tokio::pin!(notification);
        notification.as_mut().enable();
        if snapshot().complete() {
            return;
        }
        tokio::select! {
            () = notification => {}
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

#[tokio::test(start_paused = true)]
async fn queued_recovery_observes_retirement_before_its_deadline_between_periodic_checks() {
    let changed = Notify::new();
    let retired = std::sync::atomic::AtomicBool::new(false);
    let started = tokio::time::Instant::now();
    let mut recovery = Box::pin(tokio::time::timeout(
        Duration::from_secs(8),
        observe_queued_recovery(&changed, || QueuedRecoverySnapshot {
            rows_complete: true,
            public_receipt: true,
            pair_retired: retired.load(std::sync::atomic::Ordering::Acquire),
        }),
    ));
    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    // A scheduled observer can run at any offset from the owning deadline.
    // Its last periodic check here sleeps beyond that deadline, while the
    // actual final transition still arrives inside the unchanged eight seconds.
    tokio::time::advance(Duration::from_millis(7_990)).await;
    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    changed.notify_one();
    assert!(
        futures_util::poll!(recovery.as_mut()).is_pending(),
        "a notification cannot replace the complete recovery predicate"
    );
    tokio::time::advance(Duration::from_millis(9)).await;
    retired.store(true, std::sync::atomic::Ordering::Release);
    changed.notify_one();
    assert!(started.elapsed() < Duration::from_secs(8));
    let observed = recovery.await;
    assert!(
        observed.is_ok(),
        "completed recovery inside its existing deadline must not wait for a later periodic check"
    );
}

#[test]
fn queued_source_requires_current_settlement_after_retired_request_publication() {
    let mut readiness = ColdControlScope::default();
    readiness.observe("control_task paired=false protected=false", true);
    readiness.observe("document_scope_settled scope=Control", true);
    assert!(readiness.ready());
    readiness.invalidate();
    assert!(
        !readiness.ready(),
        "publication invalidates readiness before its wake arrives"
    );
    // The retired-request publication starts another comparison. The original
    // empty settlement cannot authorize capturing A while B still owes work.
    readiness.observe("control_task paired=true protected=true", false);
    assert!(!readiness.ready(), "the old empty B settlement is stale");
    readiness.observe("document_scope_settled scope=Control", false);
    assert!(
        !readiness.ready(),
        "B1 must actually be present on the requester"
    );
    readiness.observe("document_scope_settled scope=Control", true);
    assert!(
        !readiness.ready(),
        "a complete prefix cannot pay the parked comparison"
    );
    readiness.observe("control_pair_retired", true);
    assert!(readiness.ready());
    readiness.observe("control_task paired=false protected=false", true);
    assert!(
        !readiness.ready(),
        "every new comparison invalidates prior settlement"
    );
    readiness.observe("control_cancel task=Id(1) reason=same_source", true);
    readiness.observe("document_scope_settled scope=Control", true);
    assert!(
        !readiness.ready(),
        "a late child completion cannot pay new pair debt"
    );
    readiness.observe("control_pair_retired", true);
    assert!(readiness.ready());
}

// First relevant setup transitions survive unrelated traffic and recovery.
fn record_setup_milestone(
    trace: &BTrace,
    clock: std::time::Instant,
    milestone: &'static str,
    details: String,
) {
    let mut rows = trace.lock().unwrap();
    if rows.len() < 8 && !rows.iter().any(|row| row.1 == milestone) {
        rows.push_back((
            clock.elapsed().as_millis(),
            milestone,
            tokio::task::try_id(),
            details,
        ));
    }
}

#[derive(Default)]
struct SetupCancellationState {
    task: Option<tokio::task::Id>,
    open: bool,
    published: bool,
    entered: bool,
}
#[derive(Default)]
struct SetupCancellationGate {
    state: Mutex<SetupCancellationState>,
    changed: std::sync::Condvar,
    entered: Notify,
}
impl SetupCancellationGate {
    fn arm(&self, task: tokio::task::Id) {
        self.state.lock().unwrap().task = Some(task);
    }
    fn before_cancel(&self, task: tokio::task::Id) -> Option<bool> {
        let mut state = self.state.lock().unwrap();
        if state.task != Some(task) {
            return None;
        }
        state.entered = true;
        self.entered.notify_one();
        // Condvar releases its mutex. No node, ledger or trace guard may be
        // retained by the caller: the other runtime worker publishes B's rows.
        while !state.open {
            state = self.changed.wait(state).unwrap();
        }
        Some(state.published)
    }
    fn publish_complete(&self) {
        let mut state = self.state.lock().unwrap();
        state.published = true;
        state.open = true;
        self.changed.notify_all();
    }
    fn release(&self) {
        self.state.lock().unwrap().open = true;
        self.changed.notify_all();
    }
}
struct ReleaseSetupCancellation(Arc<SetupCancellationGate>);
impl Drop for ReleaseSetupCancellation {
    fn drop(&mut self) {
        self.0.release();
    }
}

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
    Box::pin(run_queued_source(false, false, false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn newer_terminal_reuses_completed_background_before_stale_foreground_suffix() {
    Box::pin(run_queued_source(true, false, false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_source_ignores_retired_natural_request_before_capturing_live_resume() {
    Box::pin(run_queued_source(true, true, false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_source_retains_live_natural_hold_through_forced_early_cancellation() {
    Box::pin(run_queued_source(false, false, true)).await;
}

async fn run_queued_source(
    gate_terminal: bool,
    delay_retired_request: bool,
    force_early_cancel: bool,
) {
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
    super::cold_account_control_setup(
        super::observe_account_fixture_condition(|| {
            [
                (&reference, &a),
                (&a, &reference),
                (&reference, &b),
                (&b, &reference),
            ]
            .into_iter()
            .all(|(node, peer)| {
                super::has_account_group_operation_support(
                    node,
                    peer,
                    Some(AccountGroupOperationKind::Rename),
                )
            })
        }),
        async {
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
        },
    )
    .await;
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
    let recovery_changed = Arc::new(Notify::new());
    let trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let setup_trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let first_held_task = Arc::new(Mutex::new(None::<tokio::task::Id>));
    let setup_cancellation = Arc::new(SetupCancellationGate::default());
    let _release_setup_cancellation = ReleaseSetupCancellation(setup_cancellation.clone());
    // Independent bounded owner rings keep frequent requester maintenance from
    // evicting the executor's last stalled phase. Snapshot before recovery.
    let executor_owner_trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let requester_owner_trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let clock = std::time::Instant::now();
    let operation = Arc::new(Mutex::new(None));
    let pages = Arc::new(Mutex::new(ControlTestPages::default()));
    let demand_dispatch = Arc::new(Mutex::new(None));
    let b_readiness = Arc::new(Mutex::new(ColdControlScope::default()));
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
        let owner_trace = executor_owner_trace.clone();
        let admitted = admitted.clone();
        move |observed, phase| {
            if admitted.load(std::sync::atomic::Ordering::Acquire)
                && (phase.starts_with("operation_owner ")
                    || phase.starts_with("operation_publication_owner "))
            {
                record_b_trace(&owner_trace, clock, "B_owner", phase.clone());
            }
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
        let requester = Arc::downgrade(&reference);
        let retired_publication_key = crate::node::AccountDocumentKey::GroupLegacyOperation {
            channel: hex::encode([1u8; 32]),
            source_device: b.user_id(),
        };
        let delayed_once = delayed_once.clone();
        let origins = origins.clone();
        let b_partial = b_partial.clone();
        let b_retired = b_retired.clone();
        let recovery_changed = recovery_changed.clone();
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
        let b_readiness = b_readiness.clone();
        let demand_dispatch = demand_dispatch.clone();
        let owner_trace = requester_owner_trace.clone();
        let setup_trace = setup_trace.clone();
        let first_held_task = first_held_task.clone();
        let setup_cancellation = setup_cancellation.clone();
        move |peer, phase| {
            // Copy first, so the wait retains no diagnostic mutex guard.
            let held_task = *first_held_task.lock().unwrap();
            if peer == peer_a
                && let Some(task) = held_task
                && phase.starts_with(&format!("control_cancel task={task:?} "))
            {
                record_setup_milestone(&setup_trace, clock, "first_A_cancel", phase.clone());
                if let Some(published) = setup_cancellation.before_cancel(task) {
                    record_setup_milestone(
                        &setup_trace,
                        clock,
                        "first_A_cancel_released",
                        format!("task={task:?} canonical_published={published} {phase}"),
                    );
                }
            }
            if admitted.load(std::sync::atomic::Ordering::Acquire)
                && (phase.starts_with("operation_owner ")
                    || phase.starts_with("operation_publication_owner "))
            {
                record_b_trace(&owner_trace, clock, "requester_owner", phase.clone());
            }
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
            if peer == peer_b {
                let corpus_present = !delay_retired_request
                    || !delayed_once.load(std::sync::atomic::Ordering::Acquire)
                    || requester.upgrade().is_some_and(|node| {
                        node.account_document_meta(&retired_publication_key)
                            .is_some()
                    });
                b_readiness.lock().unwrap().observe(&phase, corpus_present);
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
                if let Some(page) = pages.lock().unwrap().task(tokio::task::id()) {
                    record_setup_milestone(
                        &setup_trace,
                        clock,
                        "first_B_partial",
                        format!(
                            "task={:?} nonce={} request_checkpoint={:?} origin={} validated={phase}",
                            page.task,
                            hex::encode(page.request.operation.as_ref().unwrap().nonce),
                            effective_after(&page.request),
                            page.origin
                        ),
                    );
                }
                b_partial.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if peer == peer_b
                && phase == "control_pair_retired"
                && admitted.load(std::sync::atomic::Ordering::Acquire)
            {
                b_retired.store(true, std::sync::atomic::Ordering::Relaxed);
                recovery_changed.notify_one();
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
        let b_readiness = b_readiness.clone();
        let delayed_once = delayed_once.clone();
        let publisher = Arc::downgrade(&b);
        let resumed_task = resumed_task.clone();
        let retired_request_rejected = retired_request_rejected.clone();
        let rejected_tx = rejected_tx.clone();
        let setup_trace = setup_trace.clone();
        let first_held_task = first_held_task.clone();
        let setup_cancellation = setup_cancellation.clone();
        move |peer, frame| {
            let expected = expected.clone();
            let release = release.clone();
            let first_tx = first_tx.clone();
            let resume_tx = resume_tx.clone();
            let b_partial = b_partial.clone();
            let a_requests = a_requests.clone();
            let pages = pages.clone();
            let b_readiness = b_readiness.clone();
            let delayed_once = delayed_once.clone();
            let publisher = publisher.clone();
            let resumed_task = resumed_task.clone();
            let retired_request_rejected = retired_request_rejected.clone();
            let rejected_tx = rejected_tx.clone();
            let setup_trace = setup_trace.clone();
            let first_held_task = first_held_task.clone();
            let setup_cancellation = setup_cancellation.clone();
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
                    && (!b_readiness.lock().unwrap().ready()
                        || (delay_retired_request
                            && delayed_once.load(std::sync::atomic::Ordering::Acquire)
                            && !retired_request_rejected
                                .load(std::sync::atomic::Ordering::Acquire))
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
                    // The earlier empty B scope is no longer current, even if
                    // this publication's authenticated wake has not arrived yet.
                    b_readiness.lock().unwrap().invalidate();
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
                    {
                        let mut held = first_held_task.lock().unwrap();
                        setup_cancellation.arm(page.task);
                        *held = Some(page.task);
                    }
                    record_setup_milestone(
                        &setup_trace,
                        clock,
                        "first_A_held",
                        format!(
                            "task={:?} nonce={} origin={}",
                            page.task,
                            hex::encode(request.operation.as_ref().unwrap().nonce),
                            page.origin
                        ),
                    );
                    let _ = first.send(page.task);
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
                } else if dispatch.resumed && !dispatch.replacement && dispatch.protected {
                    // The preceding capture condition rejected this eligible
                    // resume because no B partial had yet been observed.
                    record_setup_milestone(
                        &setup_trace,
                        clock,
                        "early_A_resume_ignored",
                        format!(
                            "task={:?} nonce={} request_checkpoint={:?} origin={}",
                            page.task,
                            hex::encode(request.operation.as_ref().unwrap().nonce),
                            effective_after(&request),
                            page.origin
                        ),
                    );
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
    let first_task = super::cold_account_setup(async {
        let task = first_rx.await?;
        if delay_retired_request { rejected_rx.await?; }
        Ok::<_, oneshot::error::RecvError>(task)
    })
        .await
        .unwrap_or_else(|error| panic!("natural A read must precede real B publication: {error:?}; B_unpaired_settled={} B_partial={}; pages={:?}", b_readiness.lock().unwrap().ready(), b_partial.load(std::sync::atomic::Ordering::Acquire), pages.lock().unwrap().summary()))
        .unwrap();
    if delay_retired_request {
        assert!(
            retired_request_rejected.load(std::sync::atomic::Ordering::Acquire),
            "the delayed historical request is rejected before canonical publication"
        );
    }
    assert!(
        pages.lock().unwrap().is_active(first_task),
        "the exact natural A hold must remain live at canonical B publication"
    );
    let keys = super::cold_account_setup(async {
        if force_early_cancel {
            // One of the same 66 canonical signed rows, not an empty Wake or
            // an extra row. Force cancellation before the rest are published.
            crate::node::account_documents::sync::publish_legacy_control_page_for_test(&b, 1);
            record_setup_milestone(&setup_trace, clock, "forced_B1_publication", format!("canonical_rows=1 wake_token={:?}", b.account_wake_token_for_test()));
            // The publication's automatic authenticated Wake causes the
            // cancellation. Do not open another connection while the gate
            // can block the reference's combined accept/scheduler owner.
            setup_cancellation.entered.notified().await;
            assert!(setup_cancellation.state.lock().unwrap().entered);
            assert!(setup_trace.lock().unwrap().iter().any(|row| row.1 == "first_A_cancel" && row.3.ends_with("reason=wake")), "the forced control must reach the exact unrelated-Wake cancellation");
            assert!(pages.lock().unwrap().is_active(first_task));
        }
        record_setup_milestone(&setup_trace, clock, "B66_publication_begin", format!("first_A_task={first_task:?}"));
        let keys = crate::node::account_documents::sync::publish_legacy_control_page_for_test(&b, 66);
        // The exact natural child remains live through real source publication.
        assert!(pages.lock().unwrap().is_active(first_task));
        record_setup_milestone(&setup_trace, clock, "B66_publication_end", format!("first_A_task={first_task:?}"));
        setup_cancellation.publish_complete();
        resume_rx.await.map(|()| keys)
    })
        .await
        .unwrap_or_else(|error| panic!("B protected partial page must be queued before A resumes: {error:?}; B_unpaired_settled={} B_partial={}; A_roles={:?}; pages={:?}; setup={:?}; phases={:?}", b_readiness.lock().unwrap().ready(), b_partial.load(std::sync::atomic::Ordering::Acquire), origins.lock().unwrap(), pages.lock().unwrap().summary(), setup_trace.lock().unwrap(), trace.lock().unwrap()))
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
        let mut executor_committed_observed = false;
        let mut requester_committed_observed = false;
        loop {
            let local = reference
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            let remote = b
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            if !executor_committed_observed
                && remote
                    .as_ref()
                    .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed)
            {
                executor_committed_observed = true;
                record_b_trace(
                    &executor_owner_trace,
                    clock,
                    "fixture",
                    "B_executor_committed_first_observed".into(),
                );
            }
            if !requester_committed_observed
                && local
                    .as_ref()
                    .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed)
            {
                requester_committed_observed = true;
                record_b_trace(
                    &requester_owner_trace,
                    clock,
                    "fixture",
                    "B_requester_committed_first_observed".into(),
                );
            }
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
    // Recovery can make a late terminal look timely. Preserve each owner's
    // bounded body-free phase history at the original receipt boundary.
    eprintln!(
        "queued_owner_at_receipt_deadline gate_terminal={gate_terminal} delay_retired_request={delay_retired_request} receipt_ok={} setup={:?} executor={:?} requester={:?}",
        receipt.is_ok(),
        setup_trace.lock().unwrap().clone(),
        executor_owner_trace.lock().unwrap().clone(),
        requester_owner_trace.lock().unwrap().clone(),
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
    let recovery_snapshot = || QueuedRecoverySnapshot {
        rows_complete: keys
            .iter()
            .all(|key| reference.account_document_meta(key).is_some()),
        public_receipt: external
            .channels
            .lock()
            .unwrap()
            .state(&group_b)
            .is_some_and(|state| state.name() == "Queued foreground"),
        pair_retired: !first_fresh || b_retired.load(std::sync::atomic::Ordering::Relaxed),
    };
    let recovery_started = clock.elapsed();
    let mut last_recovery_observation = None;
    let recovered = tokio::time::timeout(
        Duration::from_secs(8),
        observe_queued_recovery(&recovery_changed, || {
            let snapshot = recovery_snapshot();
            last_recovery_observation = Some((clock.elapsed(), snapshot));
            snapshot
        }),
    )
    .await;
    let recovery_at_completion = (clock.elapsed(), recovery_snapshot());
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
        "releasing A must restore actual public receipt and complete all signed rows; first_fresh={first_fresh} recovery_started={recovery_started:?} last_observation={last_recovery_observation:?} completion={recovery_at_completion:?}; phases={:?}",
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
