//! A real short operation prefix leaves a queued suffix that cannot witness new demand.
use super::selected_same_source_control_latency::{
    BTrace, ColdControlScope, ControlTestPages, record_b_trace, record_control_frame,
    result_snapshot, send_boundary_wake,
};
use super::*;
use crate::node::account_documents::wire::DocumentRequest;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::time::Duration;
use tokio::sync::{Notify, Semaphore, oneshot};

/// A scope-complete event is historical until its actual page and any paired
/// comparison retire. The original natural hold must start after this setup.
#[derive(Default)]
struct QueuedSetupComparison {
    scope: ColdControlScope,
    task: Option<tokio::task::Id>,
    page_finished: bool,
}

impl QueuedSetupComparison {
    fn observe(&mut self, phase: &str, task: Option<tokio::task::Id>) {
        if phase.starts_with("control_task ") {
            self.task = task;
            self.page_finished = false;
        } else if phase.starts_with("control_cancel ") {
            self.page_finished = false;
        }
        if phase != "document_scope_settled scope=Control" || (task.is_some() && task == self.task)
        {
            self.scope.observe(phase, true);
        }
        if task.is_some()
            && task == self.task
            && phase.starts_with("page_end ")
            && phase.contains("outcome=ok")
        {
            self.page_finished = true;
        }
    }

    fn ready(&self) -> bool {
        self.scope.ready() && self.page_finished
    }
}

#[tokio::test]
async fn queued_setup_retires_wake_replacement_before_arming_the_natural_hold() {
    tokio::spawn(async {
        let task = Some(tokio::task::id());
        let mut comparison = QueuedSetupComparison::default();
        comparison.observe("control_task paired=false protected=false ", task);
        comparison.observe("document_scope_settled scope=Control", task);
        assert!(
            !comparison.ready(),
            "scope completion still has an active page"
        );
        comparison.observe("control_cancel task=old reason=same_source", None);
        comparison.observe("control_task paired=true protected=true ", task);
        comparison.observe("document_scope_settled scope=Control", task);
        comparison.observe("page_end outcome=ok", None);
        assert!(!comparison.ready(), "a different task cannot finish setup");
        comparison.observe("page_end outcome=ok", task);
        assert!(
            !comparison.ready(),
            "the real Wake replacement still owes its pair"
        );
        comparison.observe("control_pair_retired", None);
        assert!(comparison.ready());
        comparison.observe("control_task paired=false protected=false ", task);
        assert!(!comparison.ready(), "a new read invalidates old settlement");
    })
    .await
    .unwrap();
}

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
struct ProtectedBPageProgress {
    server_task: Option<tokio::task::Id>,
    lifetime: std::sync::Weak<()>,
    nonce: [u8; 32],
    headers_attempted: usize,
    complete_attempted: bool,
    last_elapsed_ms: Option<u128>,
}

#[derive(Default)]
struct PreparationReadState {
    open: bool,
    overflow: bool,
    decisions: std::collections::HashMap<[u8; 32], (bool, std::sync::Weak<()>)>,
}
#[derive(Default)]
struct PreparationReadGate {
    state: Mutex<PreparationReadState>,
    changed: Notify,
}
impl PreparationReadGate {
    fn decide(&self, nonce: [u8; 32], hold: bool, lifetime: Option<std::sync::Weak<()>>) {
        let mut state = self.state.lock().unwrap();
        state
            .decisions
            .retain(|_, (_, lifetime)| lifetime.strong_count() > 0);
        if !state.open
            && let Some(lifetime) = lifetime.filter(|value| value.strong_count() > 0)
        {
            if state.decisions.len() == 64 && !state.decisions.contains_key(&nonce) {
                state.overflow = true;
            } else {
                state.decisions.insert(nonce, (hold, lifetime));
            }
        }
        drop(state);
        self.changed.notify_waiters();
    }
    async fn wait(&self, nonce: [u8; 32]) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (ready, overflow) = {
                let mut state = self.state.lock().unwrap();
                let ready =
                    state.open || state.decisions.get(&nonce).is_some_and(|(hold, _)| !hold);
                if ready {
                    state.decisions.remove(&nonce);
                }
                (ready, state.overflow)
            };
            assert!(
                !overflow,
                "preparation decision inventory exceeded its bounded owner"
            );
            if ready {
                return;
            }
            changed.await;
        }
    }
    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.open = true;
        state.decisions.clear();
        drop(state);
        self.changed.notify_waiters();
    }
}
struct ReleasePreparationRead(Arc<PreparationReadGate>);
impl Drop for ReleasePreparationRead {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct PreparationServerDecision {
    gate: Arc<PreparationReadGate>,
    nonce: Option<[u8; 32]>,
    lifetime: Option<std::sync::Weak<()>>,
    decided: bool,
}
impl PreparationServerDecision {
    fn decide(&mut self, hold: bool) {
        if let Some(nonce) = self.nonce {
            self.gate.decide(nonce, hold, self.lifetime.clone());
        }
        self.decided = true;
    }
}
impl Drop for PreparationServerDecision {
    fn drop(&mut self) {
        if !self.decided {
            self.decide(false);
        }
    }
}

#[tokio::test]
async fn preparation_server_decisions_and_drop_release_preserve_child_lifetimes() {
    for outcome in ["pass", "drop", "hold_then_release"] {
        let gate = Arc::new(PreparationReadGate::default());
        let (created, observed) = oneshot::channel();
        let child_gate = gate.clone();
        let child = tokio::spawn(async move {
            let lifetime = Arc::new(());
            created.send(Arc::downgrade(&lifetime)).unwrap();
            child_gate.wait([1; 32]).await;
            drop(lifetime);
        });
        let lifetime = observed.await.unwrap();
        assert!(
            !child.is_finished(),
            "a client cannot guess the server's decision"
        );
        let mut decision = PreparationServerDecision {
            gate: gate.clone(),
            nonce: Some([1; 32]),
            lifetime: Some(lifetime.clone()),
            decided: false,
        };
        match outcome {
            "pass" => decision.decide(false),
            "drop" => drop(decision),
            "hold_then_release" => {
                decision.decide(true);
                tokio::task::yield_now().await;
                assert!(!child.is_finished());
                assert!(lifetime.strong_count() > 0);
                drop(ReleasePreparationRead(gate.clone()));
                // The release is permanent, including previously unknown nonces.
                gate.wait([2; 32]).await;
            }
            _ => unreachable!(),
        }
        child.await.unwrap();
        assert_eq!(lifetime.strong_count(), 0);
    }
}

#[tokio::test]
async fn preparation_decision_inventory_prunes_retired_witnesses_and_rejects_live_overflow() {
    let gate = Arc::new(PreparationReadGate::default());
    for nonce in 0..65u8 {
        let child = Arc::new(());
        gate.decide([nonce; 32], false, Some(Arc::downgrade(&child)));
        drop(child);
    }
    let (count, overflow) = {
        let state = gate.state.lock().unwrap();
        (state.decisions.len(), state.overflow)
    };
    assert!(count <= 1 && !overflow);
    let dead = {
        let child = Arc::new(());
        Arc::downgrade(&child)
    };
    gate.decide([90; 32], false, Some(dead));
    let count = gate.state.lock().unwrap().decisions.len();
    assert_eq!(
        count, 0,
        "a late retired server cannot create a pending decision"
    );
    let live: Vec<_> = (0..64).map(|_| Arc::new(())).collect();
    for (nonce, child) in live.iter().enumerate() {
        gate.decide([nonce as u8; 32], true, Some(Arc::downgrade(child)));
    }
    let (count, overflow) = {
        let state = gate.state.lock().unwrap();
        (state.decisions.len(), state.overflow)
    };
    assert_eq!(count, 64);
    assert!(!overflow);
    let extra = Arc::new(());
    gate.decide([99; 32], true, Some(Arc::downgrade(&extra)));
    let (count, overflow) = {
        let state = gate.state.lock().unwrap();
        (state.decisions.len(), state.overflow)
    };
    assert_eq!(count, 64);
    assert!(
        overflow,
        "never discard a live decision to admit another witness"
    );
    let refused = tokio::spawn(async move {
        gate.wait([99; 32]).await;
    })
    .await;
    assert!(refused.unwrap_err().is_panic());
}

#[tokio::test(start_paused = true)]
async fn queued_functional_recovery_keeps_all_predicates_after_the_receipt_budget() {
    let changed = Notify::new();
    let state = std::cell::Cell::new(QueuedRecoverySnapshot {
        rows_complete: false,
        public_receipt: false,
        pair_retired: false,
    });
    let started = tokio::time::Instant::now();
    let mut recovery = Box::pin(super::functional_account_convergence(
        observe_queued_recovery(&changed, || state.get()),
    ));
    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    tokio::time::advance(Duration::from_millis(8_001)).await;
    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    for incomplete in [
        QueuedRecoverySnapshot {
            rows_complete: false,
            public_receipt: true,
            pair_retired: true,
        },
        QueuedRecoverySnapshot {
            rows_complete: true,
            public_receipt: false,
            pair_retired: true,
        },
        QueuedRecoverySnapshot {
            rows_complete: true,
            public_receipt: true,
            pair_retired: false,
        },
    ] {
        state.set(incomplete);
        changed.notify_one();
        assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    }
    tokio::time::advance(Duration::from_millis(20_999)).await;
    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    state.set(QueuedRecoverySnapshot {
        pair_retired: true,
        ..state.get()
    });
    changed.notify_one();
    assert!(recovery.await.is_ok());
    assert_eq!(started.elapsed(), Duration::from_secs(29));
}

#[tokio::test(start_paused = true)]
async fn queued_functional_recovery_fails_without_actual_pair_retirement() {
    let changed = Notify::new();
    let mut recovery = Box::pin(super::functional_account_convergence(
        observe_queued_recovery(&changed, || QueuedRecoverySnapshot {
            rows_complete: true,
            public_receipt: true,
            pair_retired: false,
        }),
    ));
    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    tokio::time::advance(Duration::from_millis(8_001)).await;
    changed.notify_one();
    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
    tokio::time::advance(Duration::from_millis(21_999)).await;
    assert!(
        recovery.await.is_err(),
        "functional convergence still requires the real pair to retire before its watchdog"
    );
}

struct ReleaseQueuedReads(Arc<Semaphore>);
impl Drop for ReleaseQueuedReads {
    fn drop(&mut self) {
        self.0.close();
    }
}
// This gate runs only after the local maintenance helpers have returned and
// released their journal, document, log and membership guards. It isolates the
// parent scheduler's wait dependency from contention on shared storage locks.
#[derive(Default)]
struct LocalMaintenanceGate {
    armed: std::sync::atomic::AtomicBool,
    entered: Notify,
    released: (Mutex<bool>, std::sync::Condvar),
}
impl LocalMaintenanceGate {
    fn hold_if_armed(&self) {
        if !self.armed.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let (released, changed) = &self.released;
        let mut released = released.lock().unwrap();
        self.entered.notify_one();
        while !*released {
            released = changed.wait(released).unwrap();
        }
    }
    fn release(&self) {
        let (released, changed) = &self.released;
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
    fn held(&self) -> bool {
        !*self.released.0.lock().unwrap()
    }
}
struct ReleaseLocalMaintenance(Arc<LocalMaintenanceGate>);
impl Drop for ReleaseLocalMaintenance {
    fn drop(&mut self) {
        self.0.release();
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
async fn newer_terminal_reuses_completed_background_before_stale_foreground_suffix() {
    Box::pin(run_queued_source(true, false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_queued_source_dispatches_while_unrelated_local_maintenance_is_held() {
    Box::pin(run_queued_source(false, true)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_scheduler_retains_blocked_local_maintenance_until_actual_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let source = node(&dir.path().join("source"), &Account::generate());
    let gate = Arc::new(LocalMaintenanceGate::default());
    let _release = ReleaseLocalMaintenance(gate.clone());
    gate.armed.store(true, std::sync::atomic::Ordering::Release);
    *source.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let gate = gate.clone();
        move |_, phase| {
            if phase.starts_with("operation_owner phase=legacy_publication_end ") {
                gate.hold_if_armed();
            }
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let worker = tokio::spawn(source.clone().run_accept_loop(listener));
    super::cold_account_setup(gate.entered.notified())
        .await
        .unwrap();
    source.runtime_work.close();
    worker.abort();
    // Closed admission can retire a supervised arm before abort is observed.
    // Either route must still retain the actual blocked storage admission.
    if let Err(error) = worker.await {
        assert!(
            error.is_cancelled(),
            "accept owner failed while retiring: {error}"
        );
    }
    let drained = source.runtime_work.drain();
    tokio::pin!(drained);
    assert!(
        futures_util::poll!(drained.as_mut()).is_pending(),
        "cancelling the scheduler cannot retire its still-blocked local storage closure"
    );
    gate.release();
    super::cold_account_setup(drained).await.unwrap();
    *source.account_client_test_hook.lock().unwrap() = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_maintenance_panic_propagates_through_actual_accept_owner() {
    let dir = tempfile::tempdir().unwrap();
    let source = node(&dir.path().join("source"), &Account::generate());
    *source.account_client_test_hook.lock().unwrap() = Some(Arc::new(|_, phase| {
        if phase.starts_with("operation_owner phase=legacy_publication_end ") {
            panic!("injected local maintenance panic");
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let worker = tokio::spawn(source.clone().run_accept_loop(listener));
    let failure = super::cold_account_setup(worker)
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        failure.is_panic(),
        "storage worker panic cannot become normal accept-owner completion"
    );
    let panic = failure.into_panic();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"injected local maintenance panic")
    );
    source.runtime_work.close();
    super::cold_account_setup(source.runtime_work.drain())
        .await
        .unwrap();
    *source.account_client_test_hook.lock().unwrap() = None;
}

async fn run_queued_source(gate_terminal: bool, gate_local_maintenance: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let a = node(&dir.path().join("a"), &account);
    let b = node(&dir.path().join("b"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let group_a = new_channel_id();
    let group_b = ConversationId::new([3; 32]);
    let padding_group = ConversationId::new([254; 32]);
    install_owner_group(&a, &external, group_a);
    install_owner_group(&b, &external, group_b);
    install_owner_group(&b, &external, padding_group);
    // Use the normal provider's operation-prefix/legacy boundary rather than
    // importing a full 64-row padding page just to obtain a real continuation.
    b.publish_account_group_operations_for(padding_group);
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
    let release = Arc::new(Semaphore::new(0));
    let _release = ReleaseQueuedReads(release.clone());
    let (first_tx, first_rx) = oneshot::channel();
    let first_tx = Arc::new(Mutex::new(Some(first_tx)));
    let (resume_tx, resume_rx) = oneshot::channel();
    let resume_tx = Arc::new(Mutex::new(Some(resume_tx)));
    let resumed_task = Arc::new(Mutex::new(None::<tokio::task::Id>));
    let b_partial = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let b_page_progress = Arc::new(Mutex::new(ProtectedBPageProgress::default()));
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saved_after = Arc::new(Mutex::new(None));
    let b_requests = Arc::new(Mutex::new(Vec::new()));
    let b_dispatches = Arc::new(Mutex::new(Vec::new()));
    let a_requests = Arc::new(Mutex::new(Vec::new()));
    let origins = Arc::new(Mutex::new(Vec::new()));
    let b_retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let recovery_changed = Arc::new(Notify::new());
    let local_maintenance = Arc::new(LocalMaintenanceGate::default());
    let _release_local_maintenance = ReleaseLocalMaintenance(local_maintenance.clone());
    let (held_dispatch_tx, held_dispatch_rx) = oneshot::channel();
    let held_dispatch_tx = Arc::new(Mutex::new(Some(held_dispatch_tx)));
    let trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let setup_trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let first_held_task = Arc::new(Mutex::new(None::<tokio::task::Id>));
    let setup_wake_observed = Arc::new(Mutex::new(None::<std::time::Instant>));
    let setup_wake_min_revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let a_setup_comparison = Arc::new(Mutex::new(QueuedSetupComparison::default()));
    let first_hold_armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let first_rejections = Arc::new(Mutex::new([0_u64; 6]));
    let setup_wake_ready = Arc::new(Semaphore::new(0));
    let _release_setup_wake = ReleaseQueuedReads(setup_wake_ready.clone());
    let preparation_read = Arc::new(PreparationReadGate::default());
    let _release_preparation_read = ReleasePreparationRead(preparation_read.clone());
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
        let setup_wake_observed = setup_wake_observed.clone();
        let setup_wake_min_revision = setup_wake_min_revision.clone();
        let a_setup_comparison = a_setup_comparison.clone();
        let peer_b = b.identity.public();
        let own = reference.identity.public();
        let local_maintenance = local_maintenance.clone();
        let held_dispatch_tx = held_dispatch_tx.clone();
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
        move |peer, phase| {
            if peer == peer_a {
                if let Some(revision) = phase
                    .strip_prefix("wake_received revision=")
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    let minimum =
                        setup_wake_min_revision.load(std::sync::atomic::Ordering::Acquire);
                    if minimum > 0 && revision >= minimum {
                        setup_wake_observed
                            .lock()
                            .unwrap()
                            .get_or_insert_with(std::time::Instant::now);
                    }
                }
                if setup_wake_observed.lock().unwrap().is_some() {
                    a_setup_comparison
                        .lock()
                        .unwrap()
                        .observe(&phase, tokio::task::try_id());
                }
            }
            let held_task = *first_held_task.lock().unwrap();
            if peer == peer_a
                && let Some(task) = held_task
                && phase.starts_with(&format!("control_cancel task={task:?} "))
            {
                record_setup_milestone(&setup_trace, clock, "first_A_cancel", phase.clone());
            }
            if peer == peer_a
                && phase.starts_with("page_end ")
                && held_task == tokio::task::try_id()
            {
                record_setup_milestone(&setup_trace, clock, "first_A_terminal", phase.clone());
            }
            if admitted.load(std::sync::atomic::Ordering::Acquire)
                && (phase.starts_with("operation_owner ")
                    || phase.starts_with("operation_publication_owner "))
            {
                record_b_trace(&owner_trace, clock, "requester_owner", phase.clone());
            }
            if gate_local_maintenance
                && peer == own
                && phase.starts_with("operation_owner phase=legacy_publication_end ")
            {
                local_maintenance.hold_if_armed();
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
                if gate_local_maintenance
                    && local_maintenance.held()
                    && let Some(sender) = held_dispatch_tx.lock().unwrap().take()
                {
                    let _ = sender.send(());
                }
                // Open the window at the exact committed-demand dispatch.
                // Later recovery pages remain observable after it completes.
                demand_dispatch
                    .lock()
                    .unwrap()
                    .get_or_insert(page.dispatched_at);
            }
            if peer == peer_b {
                b_readiness.lock().unwrap().observe(&phase, true);
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
    // Observe the initial real B comparison and pair retirement before actors
    // start. Installing readiness after the cold pulls can miss their terminal
    // events; unchanged input need not schedule another completed comparison.
    // Arm A's blocking hooks only after these original receiver-owned pulls,
    // so setup cannot wait on the Wake that deliberately follows settlement.
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
    assert!(!reference.channel_can_manage(group_b).unwrap());

    *reference.account_document_read_test_hook.lock().unwrap() = Some(Arc::new({
        let exact = a.identity.public();
        let gate = preparation_read.clone();
        let pages = pages.clone();
        move |peer, request| {
            let exact = exact.clone();
            let gate = gate.clone();
            let pages = pages.clone();
            Box::pin(async move {
                let live = pages.lock().unwrap().live_request(&request).is_some();
                if peer == exact
                    && request.domain == AccountDocumentDomain::GroupControl
                    && let Some(probe) = request.operation.as_ref()
                    && live
                {
                    gate.wait(probe.nonce).await;
                }
            })
        }
    }));
    *a.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let expected = reference.identity.public();
        let setup_wake_ready = setup_wake_ready.clone();
        let setup_wake_observed = setup_wake_observed.clone();
        let first_hold_armed = first_hold_armed.clone();
        let first_rejections = first_rejections.clone();
        let release = release.clone();
        let first_tx = first_tx.clone();
        let resume_tx = resume_tx.clone();
        let b_partial = b_partial.clone();
        let a_requests = a_requests.clone();
        let pages = pages.clone();
        let b_readiness = b_readiness.clone();
        let resumed_task = resumed_task.clone();
        let setup_trace = setup_trace.clone();
        let first_held_task = first_held_task.clone();
        let preparation_read = preparation_read.clone();
        move |peer, frame| {
            let expected = expected.clone();
            let setup_wake_ready = setup_wake_ready.clone();
            let setup_wake_observed = setup_wake_observed.clone();
            let first_hold_armed = first_hold_armed.clone();
            let first_rejections = first_rejections.clone();
            let release = release.clone();
            let first_tx = first_tx.clone();
            let resume_tx = resume_tx.clone();
            let b_partial = b_partial.clone();
            let a_requests = a_requests.clone();
            let pages = pages.clone();
            let b_readiness = b_readiness.clone();
            let resumed_task = resumed_task.clone();
            let setup_trace = setup_trace.clone();
            let first_held_task = first_held_task.clone();
            let preparation_read = preparation_read.clone();
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
                // SessionReady can reach the client before this callback
                // decides whether the genuine request establishes the hold.
                let mut read_decision = PreparationServerDecision {
                    gate: preparation_read,
                    nonce: request.operation.as_ref().map(|probe| probe.nonce),
                    lifetime: {
                        let observed = pages.lock().unwrap();
                        observed
                            .request(&request)
                            .and_then(|page| observed.lifetime(page.task))
                    },
                    decided: false,
                };
                let first_pending = first_tx.lock().unwrap().is_some();
                if first_pending {
                    let rejected = if !first_hold_armed.load(std::sync::atomic::Ordering::Acquire) {
                        Some(0)
                    } else if !b_readiness.lock().unwrap().ready() {
                        Some(1)
                    } else if !pages.lock().unwrap().request(&request).is_some_and(|page| {
                        page.dispatch()
                            .is_some_and(|dispatch| !dispatch.paired && !dispatch.protected)
                    }) {
                        Some(2)
                    } else {
                        None
                    };
                    if let Some(reason) = rejected {
                        let mut counts = first_rejections.lock().unwrap();
                        counts[reason] = counts[reason].saturating_add(1);
                        return;
                    }
                }
                // Only the initial qualified unpaired read waits for setup
                // WakeAck. Paired/protected cold reads must finish above so B
                // can become ready and the setup sender can deliver that Wake.
                if first_pending {
                    let _ = setup_wake_ready.acquire().await;
                }
                // Re-resolve after the wait: a pre-Wake child may have retired
                // while the authenticated change was being processed.
                let Some(page) = pages.lock().unwrap().live_request(&request) else {
                    let mut counts = first_rejections.lock().unwrap();
                    counts[3] = counts[3].saturating_add(1);
                    return;
                };
                if first_pending
                    && !setup_wake_observed
                        .lock()
                        .unwrap()
                        .is_some_and(|at| page.dispatched_at >= at)
                {
                    let mut counts = first_rejections.lock().unwrap();
                    counts[4] = counts[4].saturating_add(1);
                    return;
                }
                let Some(dispatch) = page.dispatch() else {
                    let mut counts = first_rejections.lock().unwrap();
                    counts[5] = counts[5].saturating_add(1);
                    return;
                };
                {
                    let mut requests = a_requests.lock().unwrap();
                    requests.push((
                        request.version,
                        effective_after(&request),
                        request.known_digest,
                        request.operation.as_ref().map(|probe| probe.known_digest),
                    ));
                }
                let first = first_tx.lock().unwrap().take();
                if let Some(first) = first {
                    assert!(effective_after(&request).is_none());
                    {
                        let mut held = first_held_task.lock().unwrap();
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
                    let lifetime = pages
                        .lock()
                        .unwrap()
                        .lifetime(page.task)
                        .expect("the exact natural child has an observed weak lifetime");
                    read_decision.decide(true);
                    let _ = first.send((page.task, lifetime));
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
                    panic!(
                        "A cannot resume before B validates its actual partial page; task={:?}",
                        page.task
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
        let b_page_progress = b_page_progress.clone();
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
            let b_page_progress = b_page_progress.clone();
            Box::pin(async move {
                if peer != expected {
                    return;
                }
                if !admitted.load(std::sync::atomic::Ordering::Acquire) {
                    // Headers carry no request nonce. Bind this one fixed-size
                    // observer to the serving task at its exact protected request;
                    // these are send attempts, not proof of received page bytes.
                    let server_task = tokio::task::try_id();
                    match &frame {
                        AccountHistoryFrame::DocumentsRequest { body } => {
                            let candidate = DocumentRequest::decode(body)
                                .filter(|request| {
                                    request.domain == AccountDocumentDomain::GroupControl
                                })
                                .and_then(|request| {
                                    let observed = pages.lock().unwrap();
                                    let page = observed.live_request(&request)?;
                                    if !page.dispatch()?.protected {
                                        return None;
                                    }
                                    Some((request.operation?.nonce, observed.lifetime(page.task)?))
                                });
                            let mut progress = b_page_progress.lock().unwrap();
                            if let Some((nonce, lifetime)) = candidate {
                                *progress = ProtectedBPageProgress {
                                    server_task,
                                    lifetime,
                                    nonce,
                                    last_elapsed_ms: Some(clock.elapsed().as_millis()),
                                    ..Default::default()
                                };
                            } else if progress.server_task == server_task {
                                progress.server_task = None;
                            }
                        }
                        AccountHistoryFrame::DocumentHeader { body } => {
                            if serde_json::from_slice::<crate::node::AccountDocumentMeta>(body)
                                .is_ok_and(|meta| {
                                    matches!(
                                        meta.key,
                                        crate::node::AccountDocumentKey::GroupLegacyOperation { .. }
                                    )
                                })
                            {
                                let mut progress = b_page_progress.lock().unwrap();
                                if server_task.is_some() && progress.server_task == server_task {
                                    progress.headers_attempted =
                                        progress.headers_attempted.saturating_add(1);
                                    progress.last_elapsed_ms = Some(clock.elapsed().as_millis());
                                }
                            }
                        }
                        AccountHistoryFrame::DocumentsComplete { body } => {
                            let nonce = serde_json::from_slice::<serde_json::Value>(body)
                                .ok()
                                .and_then(|value| {
                                    serde_json::from_value::<[u8; 32]>(
                                        value["operation"]["nonce"].clone(),
                                    )
                                    .ok()
                                });
                            let mut progress = b_page_progress.lock().unwrap();
                            if server_task.is_some()
                                && progress.server_task == server_task
                                && nonce == Some(progress.nonce)
                            {
                                progress.complete_attempted = true;
                                progress.last_elapsed_ms = Some(clock.elapsed().as_millis());
                            }
                        }
                        _ => {}
                    }
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
                            if matches!(key, crate::node::AccountDocumentKey::GroupOperation { .. })
                            {
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
    let (first_task, first_task_lifetime) = super::cold_account_setup(async {
        // A real setup Wake may replace an active natural comparison. Let
        // that exact signed comparison and its pair finish while the hold is
        // unarmed, then open the original through receiver-owned demand only.
        let key = crate::node::account_documents::sync::seed_legacy_control_page_for_test(
            &a,
            ConversationId::new([67; 32]),
        );
        let (_, revision) = a.account_wake_token_for_test();
        setup_wake_min_revision.store(revision.checked_add(1).unwrap(), std::sync::atomic::Ordering::Release);
        a.note_account_local_change();
        send_boundary_wake(&a, &reference).await;
        loop {
            let a_ready = a_setup_comparison.lock().unwrap().ready();
            let b_ready = b_readiness.lock().unwrap().ready();
            if a_ready && b_ready && reference.account_document_meta(&key) == a.account_document_meta(&key) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        setup_wake_ready.close();
        first_hold_armed.store(true, std::sync::atomic::Ordering::Release);
        reference.request_sibling_pull_for_test(&a.identity.public());
        first_rx.await
    })
        .await
        .unwrap_or_else(|error| {
            let ready = b_readiness.lock().unwrap().ready();
            let observed = pages.lock().unwrap().summary();
            panic!("natural A read must precede real B publication: {error:?}; B_unpaired_settled={ready} B_partial={}; A_setup_ready={}; armed={}; rejected_unarmed_B_unready_ineligible_not_live_pre_wake_missing_dispatch={:?}; pages={observed:?}", b_partial.load(std::sync::atomic::Ordering::Acquire), a_setup_comparison.lock().unwrap().ready(), first_hold_armed.load(std::sync::atomic::Ordering::Acquire), *first_rejections.lock().unwrap());
        })
        .unwrap();
    let first_active = pages.lock().unwrap().is_active(first_task);
    assert!(
        first_active,
        "the exact natural A hold must remain live at canonical B publication"
    );
    let keys = super::cold_account_setup(async {
        record_setup_milestone(&setup_trace, clock, "B2_publication_begin", format!("first_A_task={first_task:?}"));
        let keys = crate::node::account_documents::sync::publish_legacy_control_page_for_test(&b, 2);
        // Keep the actual child's Weak separately from the bounded history;
        // never retain a strong Arc or accept retirement as a live hold.
        let (ledger_active, origin_retained) = {
            let observed = pages.lock().unwrap();
            (observed.is_active(first_task), observed.lifetime(first_task).is_some())
        };
        let direct_child_alive = first_task_lifetime.strong_count() > 0;
        // documents_changed legitimately lets the scheduler cancel A before
        // the publisher returns. Liveness belongs before the publication trigger;
        // requiring it here would race the very cancellation under test.
        record_setup_milestone(&setup_trace, clock, "B2_publication_end", format!("first_A_task={first_task:?} ledger_active={ledger_active} direct_child_alive={direct_child_alive} origin_retained={origin_retained}"));
        // Permanently open before the protected resume and real operation.
        preparation_read.release();
        resume_rx.await.map(|()| keys)
    })
        .await
        .unwrap_or_else(|error| {
            let ready = b_readiness.lock().unwrap().ready();
            let roles = origins.lock().unwrap().clone();
            let observed = pages.lock().unwrap().summary();
            let setup = setup_trace.lock().unwrap().clone();
            let phases = trace.lock().unwrap().clone();
            let (headers_attempted, complete_attempted, last_elapsed_ms, current_task_alive) = {
                let progress = b_page_progress.lock().unwrap();
                (progress.headers_attempted, progress.complete_attempted, progress.last_elapsed_ms, progress.lifetime.strong_count() > 0)
            };
            panic!("B protected partial page must be queued before A resumes: {error:?}; B_unpaired_settled={ready} B_partial={}; B_headers_attempted={headers_attempted} B_complete_attempted={complete_attempted} B_last_elapsed_ms={last_elapsed_ms:?} B_current_task_alive={current_task_alive}; A_roles={roles:?}; pages={observed:?}; setup={setup:?}; phases={phases:?}", b_partial.load(std::sync::atomic::Ordering::Acquire));
        })
        .unwrap();
    assert!(b_partial.load(std::sync::atomic::Ordering::Relaxed));
    let saved = saved_after
        .lock()
        .unwrap()
        .clone()
        .expect("a real B Complete must retain its operation-prefix continuation");
    assert_eq!(
        saved,
        crate::node::AccountDocumentKey::GroupOperation {
            channel: hex::encode(padding_group.as_bytes()),
            source_device: b.user_id(),
        },
        "the normal short operation prefix must end before the two legacy rows"
    );
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
    if gate_local_maintenance {
        local_maintenance
            .armed
            .store(true, std::sync::atomic::Ordering::Release);
        reference.account_sync_notify.notify_one();
        super::cold_account_setup(local_maintenance.entered.notified())
            .await
            .expect(
                "requester maintenance outside shared storage guards is held before B admission",
            );
        record_b_trace(
            &trace,
            clock,
            "fixture",
            "requester_local_maintenance_held".into(),
        );
    }
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
    let receipt_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let held_dispatch = if gate_local_maintenance {
        let observed = tokio::time::timeout_at(receipt_deadline, held_dispatch_rx).await;
        local_maintenance.release();
        record_b_trace(
            &trace,
            clock,
            "fixture",
            format!(
                "requester_local_maintenance_released dispatch_while_held={}",
                matches!(&observed, Ok(Ok(())))
            ),
        );
        Some(observed)
    } else {
        None
    };
    let receipt = tokio::time::timeout_at(receipt_deadline, async {
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
        "queued_owner_at_receipt_deadline gate_terminal={gate_terminal} receipt_ok={} setup={:?} executor={:?} requester={:?}",
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
    // The private receipt above retains its measured eight-second bound.
    // After releasing A, full corpus replay and retirement of both independent
    // comparisons are functional convergence under ordinary cross-peer fairness.
    let recovered =
        super::functional_account_convergence(observe_queued_recovery(&recovery_changed, || {
            let snapshot = recovery_snapshot();
            last_recovery_observation = Some((clock.elapsed(), snapshot));
            snapshot
        }))
        .await;
    let recovery_at_completion = (clock.elapsed(), recovery_snapshot());
    *a.account_session_test_hook.lock().unwrap() = None;
    *b.account_session_test_hook.lock().unwrap() = None;
    *b.account_client_test_hook.lock().unwrap() = None;
    *reference.account_document_read_test_hook.lock().unwrap() = None;
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
    if let Some(observed) = held_dispatch {
        assert!(
            matches!(observed, Ok(Ok(()))),
            "the exact selected B request must dispatch while unrelated local maintenance outside shared storage guards remains held; phases={:?}",
            trace.lock().unwrap()
        );
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
    let result=receipt.unwrap_or_else(|error| {
        let phases = trace.lock().unwrap().clone();
        panic!("queued selected source must return its real private receipt within unchanged eight seconds: {error:?}; phases={phases:?}");
    });
    assert_eq!(result.committed_events.len(), 1);
    let event = b
        .log
        .lock()
        .unwrap()
        .get(&result.committed_events[0])
        .unwrap()
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
