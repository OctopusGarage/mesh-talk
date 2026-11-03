//! Retired terminal File receipts retain status, never usable manifest/key copies.
use super::*;
use crate::node::account_group::operations::wire::OperationFrame;
use crate::{file::FileKind, node::MessageProtection};
use base64::Engine;

#[derive(Clone, Copy)]
enum Retirement {
    Expiry,
    Recall,
    Erase,
}

async fn retired_file_result_is_status_only(retirement: Retirement) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let mut source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let path = dir.path().join("protected.png");
    let bytes = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
    std::fs::write(&path, bytes).unwrap();
    let deadline = matches!(retirement, Retirement::Expiry).then(|| source.expiry_now() + 60_000);
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    source
        .submit_account_group_file_if(
            &account.account_id(),
            id,
            group,
            &path,
            FileKind::Media,
            deadline,
            Some(MessageProtection::Click),
            |_| {},
            |commit| commit(),
        )
        .await
        .unwrap();
    source.maintain_account_group_operation_effects();
    let original = source
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(original.phase, AccountGroupOperationPhase::Committed);
    let event_id = original.primary_event.unwrap();
    let live = source
        .operation_projection(group)
        .unwrap()
        .results
        .into_iter()
        .find(|result| result.reference.id == id)
        .unwrap();
    assert!(
        live.file_descriptor.is_some() && live.message_event.is_some(),
        "live explicit File completion must first expose a real usable result"
    );
    // Exercise the real preflight path used while submit already holds delivery.
    // Release the guard before joining even on failure, so regressions report a
    // bounded timeout rather than leaving a test thread permanently blocked.
    let operation = AccountGroupOperation::React {
        target: event_id,
        emoji: "👍".into(),
        remove: false,
    };
    let next_id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    // This fixture drives accepted effects without the participant worker.
    // Finish metadata before constructing the internal executor snapshot whose
    // preflight is then tested while the delivery lock is held.
    source.await_channel_metadata(group).await.unwrap();
    let executor = source
        .account_group_access(group)
        .unwrap()
        .sources
        .into_iter()
        .find(|executor| executor.device == source.identity.public())
        .unwrap();
    let request = operations::wire::RequestProjection {
        reference: operations::journal::OperationReference::new(
            &source, next_id, group, &operation, executor,
        ),
        body: Some(operation),
        cancel: false,
        nonce: [0; 16],
    };
    let (sent, received) = std::sync::mpsc::channel();
    let worker_source = source.clone();
    let held = source.delivery.lock().unwrap();
    let worker = std::thread::spawn(move || {
        sent.send(worker_source.preflight_group_operation_request(request))
            .unwrap()
    });
    let preflight = received.recv_timeout(std::time::Duration::from_secs(2));
    drop(held);
    worker.join().unwrap();
    assert!(
        preflight
            .expect("live File result preflight must not reacquire the already-held delivery lock")
    );
    let malformed_reference = node(&dir.path().join("malformed-reference"), &account);
    link(&malformed_reference, &source);
    for missing_descriptor in [false, true] {
        let mut partial = live.clone();
        if missing_descriptor {
            partial.file_descriptor = None;
        } else {
            partial.message_event = None;
        }
        assert!(
            malformed_reference
                .import_group_operation_result_for_test(partial, &source.own_group_proof())
                .is_err(),
            "retired status-only support must not accept a partially stripped File proof"
        );
    }
    drop(malformed_reference);
    let file =
        crate::node::files::validated_manifest(&live.file_descriptor.as_ref().unwrap().manifest)
            .unwrap()
            .file_conv();
    match retirement {
        Retirement::Expiry => {
            Arc::get_mut(&mut source).unwrap().expiry_clock_start =
                (std::time::Instant::now(), deadline.unwrap() + 1);
        }
        Retirement::Recall => {
            let recall_id = source
                .new_account_group_operation_id(&account.account_id())
                .unwrap();
            source
                .submit_account_group_operation(
                    &account.account_id(),
                    recall_id,
                    group,
                    AccountGroupOperation::Recall { target: event_id },
                )
                .unwrap();
            source.maintain_account_group_operation_effects();
            assert_eq!(
                source
                    .account_group_operation_status(&account.account_id(), recall_id)
                    .unwrap()
                    .unwrap()
                    .phase,
                AccountGroupOperationPhase::Committed
            );
            assert!(
                source
                    .recalls
                    .lock()
                    .unwrap()
                    .targets(&group)
                    .contains(&event_id)
            );
        }
        Retirement::Erase => {
            assert!(source.delete_message(group, event_id, false).unwrap() > 0);
            assert!(
                source
                    .received_files
                    .lock()
                    .unwrap()
                    .entry(event_id)
                    .unwrap()
                    .is_none()
            );
        }
    }
    let projection = source.operation_projection(group).unwrap();
    let retired = projection
        .results
        .iter()
        .find(|result| result.reference.id == id)
        .unwrap();
    assert_eq!(retired.status, original);
    assert!(
        retired.file_descriptor.is_none() && retired.message_event.is_none(),
        "expiry, recall or local erasure must not republish a usable retired File manifest/key"
    );
    let wire = OperationFrame::new(&source, group, 1, projection).encode();
    let frame = OperationFrame::decode(&wire, &account.public().ed25519_pub)
        .expect("status-only terminal receipts must retain their original signed reference");
    let result = frame
        .projection
        .results
        .into_iter()
        .find(|result| result.reference.id == id)
        .unwrap();
    let reference = node(&dir.path().join("fresh-reference"), &account);
    link(&reference, &source);
    assert!(
        !reference
            .import_group_operation_result_for_test(result, &source.own_group_proof())
            .unwrap(),
        "a body-free retired receipt must adopt terminal status without keeping a pending File copy"
    );
    assert_eq!(
        reference
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap(),
        original
    );
    assert!(
        reference
            .received_files
            .lock()
            .unwrap()
            .entry(event_id)
            .unwrap()
            .is_none()
    );
    assert!(
        reference
            .account_file_proofs
            .lock()
            .unwrap()
            .get(&event_id)
            .unwrap()
            .is_none()
    );
    assert!(reference.account_group_data_work(None).work.is_empty());
    assert!(reference.channel_members(group).is_empty());
    assert!(reference.history(group, 20).unwrap().is_empty());
    assert!(
        reference
            .log
            .lock()
            .unwrap()
            .events(&file)
            .unwrap()
            .is_empty()
    );
    let identity = reference.identity.secret_bytes();
    drop(reference);
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let restarted = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(identity.0, identity.1),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        files,
        &dir.path().join("fresh-reference/events.log"),
        &dir.path().join("fresh-reference/sent.log"),
        "pw",
    )
    .unwrap();
    assert_eq!(
        restarted
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap(),
        original
    );
    assert!(restarted.account_group_data_work(None).work.is_empty());
    assert!(
        restarted
            .received_files
            .lock()
            .unwrap()
            .entry(event_id)
            .unwrap()
            .is_none()
    );
    assert!(
        restarted
            .account_file_proofs
            .lock()
            .unwrap()
            .get(&event_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn expired_file_result_publishes_only_terminal_status_to_fresh_sibling() {
    retired_file_result_is_status_only(Retirement::Expiry).await;
}
#[tokio::test]
async fn recalled_file_result_publishes_only_terminal_status_to_fresh_sibling() {
    retired_file_result_is_status_only(Retirement::Recall).await;
}
#[tokio::test]
async fn erased_file_result_publishes_only_terminal_status_to_fresh_sibling() {
    retired_file_result_is_status_only(Retirement::Erase).await;
}

/// A terminal account receipt remains readable after its physical source leaves.
/// A fresh authenticated wake must not wait behind an older failed Noise dial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modern_leave_terminal_wake_recovers_a_real_noise_timeout_within_operation_budget() {
    Box::pin(modern_leave_timeout_recovery(false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modern_leave_wake_during_real_noise_timeout_recovers_within_operation_budget() {
    Box::pin(modern_leave_timeout_recovery(true)).await;
}

#[derive(Default)]
struct DuringAttemptWake {
    revision: Option<u64>,
    received_before_failure: bool,
    failed: bool,
}
impl DuringAttemptWake {
    fn observe(&mut self, event: &str) {
        if event.contains("outcome=NoiseTimeout") {
            self.failed = true;
        }
        if self
            .revision
            .is_some_and(|revision| event == format!("wake_received revision={revision}"))
            && !self.failed
        {
            self.received_before_failure = true;
        }
    }
}

#[test]
fn during_attempt_wake_uses_exact_reception_order_not_ack_return() {
    let mut observed = DuringAttemptWake {
        revision: Some(7),
        ..Default::default()
    };
    observed.observe("wake_received revision=7");
    observed.observe("dial_noise_end ok=false outcome=NoiseTimeout");
    assert!(observed.failed);
    assert!(
        observed.received_before_failure,
        "a later ack return cannot erase authenticated reception during the attempt"
    );
    for revision in [None, Some(6), Some(7)] {
        let mut rejected = DuringAttemptWake {
            revision: Some(7),
            ..Default::default()
        };
        if let Some(revision) = revision.filter(|revision| *revision != 7) {
            rejected.observe(&format!("wake_received revision={revision}"));
        }
        rejected.observe("dial_noise_end ok=false outcome=NoiseTimeout");
        if revision == Some(7) {
            rejected.observe("wake_received revision=7");
        }
        assert!(
            !rejected.received_before_failure,
            "missing, stale and late wake evidence must fail"
        );
    }
}

#[derive(Default)]
struct NoiseFault {
    state: Mutex<NoiseFaultState>,
    changed: tokio::sync::Notify,
}
#[derive(Default)]
struct NoiseFaultState {
    armed: bool,
    closed: bool,
    roles: std::collections::HashMap<tokio::task::Id, bool>,
    ports: std::collections::HashMap<u16, tokio::task::Id>,
    latest_ports: std::collections::HashMap<tokio::task::Id, u16>,
    claimed: Option<(tokio::task::Id, u16)>,
}
impl NoiseFault {
    fn arm(&self) {
        *self.state.lock().unwrap() = NoiseFaultState {
            armed: true,
            ..Default::default()
        };
        self.changed.notify_waiters();
    }
    fn observe(&self, task: tokio::task::Id, event: &str) {
        let mut state = self.state.lock().unwrap();
        if event.starts_with("negotiation_begin") {
            state
                .roles
                .insert(task, event == "negotiation_begin domain=Some(GroupControl)");
        }
        if let Some(port) = event
            .strip_prefix("dial_socket_ports=(Some(")
            .and_then(|ports| ports.split(')').next())
            .and_then(|port| port.parse::<u16>().ok())
        {
            state.ports.insert(port, task);
            state.latest_ports.insert(task, port);
        }
        if event == "dial_noise_begin"
            && state.armed
            && state.claimed.is_none()
            && state.roles.get(&task) == Some(&true)
            && let Some(port) = state.latest_ports.get(&task).copied()
        {
            state.claimed = Some((task, port));
        }
        drop(state);
        self.changed.notify_waiters();
    }
    fn is_fault_task(&self, task: tokio::task::Id) -> bool {
        let state = self.state.lock().unwrap();
        state.claimed.is_some_and(|(owner, port)| {
            owner == task
                && state.latest_ports.get(&task) == Some(&port)
                && state.roles.get(&task) == Some(&true)
        })
    }
    async fn classify(&self, port: u16) -> Option<bool> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                if state.closed {
                    return None;
                }
                if !state.armed {
                    return Some(false);
                }
                if let Some(task) = state.ports.get(&port) {
                    if state.roles.get(task) == Some(&false) {
                        return Some(false);
                    }
                    if let Some(claimed) = state.claimed {
                        return Some(claimed == (*task, port));
                    }
                }
            }
            changed.await;
        }
    }
    fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.changed.notify_waiters();
    }
}
struct ReleaseNoiseFault(Arc<NoiseFault>, Arc<tokio::sync::Semaphore>);
impl Drop for ReleaseNoiseFault {
    fn drop(&mut self) {
        self.0.close();
        self.1.close();
    }
}
fn noise_fixture_provider(
    node: Arc<Node>,
    listener: TcpListener,
    fault: Option<Arc<NoiseFault>>,
    release: Arc<tokio::sync::Semaphore>,
    held: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let node = node.clone();
                    let Some(permit) = node.runtime_work.admit() else { return; };
                    let fault = fault.clone(); let release = release.clone(); let held = held.clone();
                    connections.spawn(permit.track(Box::pin(async move {
                        if let Some(fault) = fault {
                            match fault.classify(stream.peer_addr().unwrap().port()).await {
                                None => return,
                                Some(true) => {
                                    held.store(true, std::sync::atomic::Ordering::Release);
                                    let _ = release.acquire().await;
                                    return;
                                }
                                Some(false) => {}
                            }
                        }
                        if let Ok(channel) = node.privacy_accept(stream).await { node.serve_connection(channel).await; }
                    })));
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    })
}
#[tokio::test]
async fn noise_fault_requires_a_fresh_control_socket_and_retires_unclassified_waits() {
    let fault = NoiseFault::default();
    fault.arm();
    let directory = tokio::spawn(async { tokio::task::id() }).await.unwrap();
    let control = tokio::spawn(async { tokio::task::id() }).await.unwrap();
    let retry = tokio::spawn(async { tokio::task::id() }).await.unwrap();
    fault.observe(directory, "negotiation_begin domain=Some(GroupInventory)");
    fault.observe(directory, "dial_socket_ports=(Some(41), Some(90))");
    fault.observe(directory, "dial_noise_begin");
    assert!(!fault.is_fault_task(directory));
    assert_eq!(fault.classify(41).await, Some(false));
    fault.observe(control, "negotiation_begin domain=Some(GroupControl)");
    fault.observe(control, "session_checkout_end reused=true");
    assert!(
        !fault.is_fault_task(control),
        "a reused channel cannot prove a Noise fault"
    );
    fault.observe(control, "dial_socket_ports=(Some(42), Some(90))");
    assert!(
        !fault.is_fault_task(control),
        "TCP classification alone is not Noise start"
    );
    fault.observe(control, "dial_noise_begin");
    assert_eq!(fault.classify(42).await, Some(true));
    assert!(fault.is_fault_task(control));
    fault.observe(control, "dial_socket_ports=(Some(45), Some(90))");
    fault.observe(control, "dial_noise_begin");
    assert!(
        !fault.is_fault_task(control),
        "a second socket in the same task cannot qualify the original failure"
    );
    assert_eq!(fault.classify(45).await, Some(false));
    fault.observe(control, "dial_socket_ports=(Some(42), Some(90))");
    fault.observe(control, "negotiation_begin domain=Some(GroupInventory)");
    assert!(
        !fault.is_fault_task(control),
        "another domain in the original task cannot qualify the Control failure"
    );
    fault.observe(retry, "negotiation_begin domain=Some(GroupControl)");
    fault.observe(retry, "dial_socket_ports=(Some(43), Some(90))");
    fault.observe(retry, "dial_noise_begin");
    assert_eq!(
        fault.classify(43).await,
        Some(false),
        "only the original exact socket is held"
    );
    let waiting = fault.classify(44);
    tokio::pin!(waiting);
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    fault.close();
    assert_eq!(
        waiting.await,
        None,
        "cleanup retires classification without a synthetic transport timeout"
    );
}

#[test]
fn fixture_gate_hands_off_its_worker_to_the_queued_owner() {
    use super::recovered_fanout::{PublicationGate, ReleasePublications};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    struct StopWatchdog(std::sync::mpsc::Sender<()>);
    impl Drop for StopWatchdog {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    for handoff in [false, true] {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let gate = Arc::new(PublicationGate::default());
        let returned = Arc::new(AtomicBool::new(false));
        let watchdog_fired = Arc::new(AtomicBool::new(false));
        let (outcome, retired) = std::thread::scope(|scope| {
            let (stop, stopped) = std::sync::mpsc::channel();
            let stop = StopWatchdog(stop);
            let watchdog = scope.spawn({
                let gate = gate.clone();
                let fired = watchdog_fired.clone();
                move || {
                    // Deadlock backstop only: its use always fails the proof.
                    if stopped.recv_timeout(Duration::from_secs(5)).is_err() {
                        fired.store(true, Ordering::Release);
                        gate.release();
                    }
                }
            });
            let release_on_unwind = ReleasePublications([gate.clone()]);
            let (owner_finished, mut owner_completion) = tokio::sync::oneshot::channel();
            let held = runtime.spawn({
                let gate = gate.clone();
                let returned = returned.clone();
                async move {
                    // Spawn on the sole worker, then block without yielding.
                    // The actual owner's runnable task is in its local queue.
                    let owner = tokio::spawn({
                        let returned = returned.clone();
                        async move {
                            let _ = owner_finished.send(!returned.load(Ordering::Acquire));
                        }
                    });
                    if handoff {
                        gate.hold_on_runtime_worker();
                    } else {
                        gate.hold();
                    }
                    returned.store(true, Ordering::Release);
                    owner.await
                }
            });
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(async {
                    gate.entered.notified().await;
                    if handoff {
                        assert!(
                            owner_completion.await.unwrap(),
                            "the owner must complete while the handed-off gate remains held"
                        );
                        assert!(!returned.load(Ordering::Acquire));
                    } else {
                        assert!(
                            matches!(
                                owner_completion.try_recv(),
                                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                            ),
                            "the old raw Condvar strands its known queued owner on the sole worker"
                        );
                        gate.release();
                        assert!(
                            !owner_completion.await.unwrap(),
                            "the raw-gate owner can run only after its worker is released"
                        );
                    }
                });
            }));
            gate.release();
            let retired = runtime.block_on(held);
            drop(release_on_unwind);
            drop(stop);
            watchdog.join().unwrap();
            (outcome, retired)
        });
        runtime.shutdown_timeout(Duration::from_secs(5));
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
        assert!(
            !watchdog_fired.load(Ordering::Acquire),
            "the deadlock backstop cannot establish readiness"
        );
        assert!(
            matches!(retired, Ok(Ok(()))),
            "both the gate task and actual queued owner must retire"
        );
    }
}

async fn modern_leave_timeout_recovery(wake_during_attempt: bool) {
    use super::recovered_fanout::{PublicationGate, ReleasePublications};
    use futures_util::FutureExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let f = crate::node::channels::account_self_control::tests::fixture(false);
    let dir = tempfile::tempdir().unwrap();
    let group = f.group;
    let account = Account::from_secret_bytes(f.account.secret_bytes());
    let source_path = dir.path().join("source");
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (channel, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    let source = Node::open_with_account(
        f.member,
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        files,
        &source_path.join("events.log"),
        &source_path.join("sent.log"),
        "pw",
    )
    .unwrap();
    let reference = node(&dir.path().join("reference"), &account);
    for event in &f.events {
        install(&source, event);
    }
    link(&source, &reference);
    link(&reference, &source);
    let source_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reference_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_address = source_listener.local_addr().unwrap();
    let reference_address = reference_listener.local_addr().unwrap();
    for (label, current, other, address, own_address) in [
        (
            "source",
            &source,
            &reference,
            reference_address,
            source_address,
        ),
        (
            "reference",
            &reference,
            &source,
            source_address,
            reference_address,
        ),
    ] {
        for category in [
            SyncCategory::History,
            SyncCategory::Profile,
            SyncCategory::Attachments,
        ] {
            current.set_device_sync_category(category, false).unwrap();
        }
        configure(current, &dir.path().join(label), own_address.port());
        current.roster.lock().unwrap().update(
            &other.signed_announce("Timeout recovery", address.port()),
            address.ip(),
            &current.user_id(),
        );
    }
    use super::selected_same_source_control_latency::{BTrace, record_b_trace};
    use crate::node::account_documents::sync::DocumentSyncCursor;
    let fault = Arc::new(NoiseFault::default());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let _release_fault = ReleaseNoiseFault(fault.clone(), release.clone());
    let held_control = Arc::new(AtomicBool::new(false));
    let dispatch = Arc::new(PublicationGate::default());
    let directory = Arc::new(PublicationGate::default());
    let _release_dispatch = ReleasePublications([dispatch.clone(), directory.clone()]);
    let pause_directory = Arc::new(AtomicBool::new(false));
    let first_dispatch = Arc::new(AtomicBool::new(true));
    let noise_timeout_seen = Arc::new(AtomicBool::new(false));
    let timeout_seen = Arc::new(AtomicBool::new(false));
    let during = Arc::new(Mutex::new(DuringAttemptWake::default()));
    let trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let clock = Instant::now();
    *reference.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let expected = source.identity.public();
        let fault = fault.clone();
        let noise_timeout_seen = noise_timeout_seen.clone();
        let timeout_seen = timeout_seen.clone();
        let during = during.clone();
        let dispatch = dispatch.clone();
        let directory = directory.clone();
        let pause_directory = pause_directory.clone();
        let trace = trace.clone();
        move |peer, event| {
            if peer != expected {
                return;
            }
            let original = tokio::task::try_id().is_some_and(|task| {
                fault.observe(task, &event);
                fault.is_fault_task(task)
            });
            if event.starts_with("wake_received")
                || (original && event.starts_with("dial_noise_end"))
            {
                during.lock().unwrap().observe(&event);
            }
            if original
                && event.starts_with("dial_noise_end")
                && event.contains("outcome=NoiseTimeout")
            {
                noise_timeout_seen.store(true, Ordering::Release);
            }
            // The terminal-wake case starts only after the failed original
            // Control page has returned and installed its retry state.
            if original
                && noise_timeout_seen.load(Ordering::Acquire)
                && event.starts_with("page_end ")
                && event
                    .split_whitespace()
                    .any(|field| field == "outcome=noise_timeout")
            {
                timeout_seen.store(true, Ordering::Release);
            }
            if event.starts_with("negotiation_")
                || event.starts_with("session_checkout")
                || event.starts_with("dial_")
                || event.starts_with("page_end")
                || event.starts_with("wake_")
                || event.starts_with("control_dispatch")
                || event.starts_with("control_timeout_wake_refresh")
            {
                record_b_trace(&trace, clock, "requester_to_source", event.clone());
            }
            // This fixture isolates Control Noise recovery. Its unrelated
            // Directory pass completed in cold setup; pause the next child
            // before negotiation/authentication clocks or authority guards.
            if pause_directory.load(Ordering::Acquire)
                && (event == "negotiation_begin domain=Some(GroupInventory)"
                    || event == "negotiation_begin domain=Some(DeviceHostname)")
            {
                directory.hold_on_runtime_worker();
            }
            if wake_during_attempt
                && event.starts_with("control_dispatch")
                && first_dispatch.swap(false, Ordering::AcqRel)
            {
                dispatch.hold_on_runtime_worker();
            }
        }
    }));
    let source_server = noise_fixture_provider(
        source.clone(),
        source_listener,
        Some(fault.clone()),
        release.clone(),
        held_control.clone(),
    );
    let reference_server = noise_fixture_provider(
        reference.clone(),
        reference_listener,
        None,
        release.clone(),
        held_control.clone(),
    );
    let mut reference_worker = None;
    let mut source_worker = None;
    let outcome = std::panic::AssertUnwindSafe(async {
        source.publish_account_group_access_for(group);
        pull_group_control(&reference, &source, source_address, group).await;
        source.pull_account_documents_page(&source.routing_peer(&reference.user_id()).unwrap(), &mut DocumentSyncCursor::control()).await.unwrap();
        // Finish both genuine cold mandatory Directory passes before admitting
        // the measured operation. Preserve cursors, policy checks and retries.
        super::cold_account_setup(async {
            for (node, other) in [(&reference, &source), (&source, &reference)] {
                let mut cursor = DocumentSyncCursor::directory();
                loop {
                    node.pull_account_documents_page(&node.routing_peer(&other.user_id()).unwrap(), &mut cursor).await.unwrap();
                    assert!(cursor.error.is_none() && !cursor.requires_upgrade, "cold Directory preparation cannot hide protocol rejection");
                    if !cursor.pending && !cursor.admission_deferred() && cursor.retry_at.is_none() { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }).await.expect("unrelated mandatory Directory preparation must precede Control fault admission");
        record_b_trace(&trace, clock, "fixture", "cold_directory_prepared".into());
        assert_eq!(reference.account_group_access(group).unwrap().role, AccountGroupRole::Member);
        assert!(reference.channel_members(group).is_empty());
        source.sessions.clear(); reference.sessions.clear();
        pause_directory.store(true, Ordering::Release);
        fault.arm();
        let id = reference.new_account_group_operation_id(&account.account_id()).unwrap();
        let started = Instant::now();
        reference.submit_account_group_operation(&account.account_id(), id, group, AccountGroupOperation::LeaveAccount).unwrap();
        record_b_trace(&trace, clock, "fixture", "original_operation_admitted".into());
        reference_worker = Some(tokio::spawn(reference.clone().run_account_history_loop()));
        if !wake_during_attempt { source_worker = Some(tokio::spawn(source.clone().run_account_history_loop())); }
        let receipt = tokio::time::timeout(Duration::from_secs(8), async {
            if wake_during_attempt {
                dispatch.entered.notified().await;
                let intent_key = crate::node::AccountDocumentKey::GroupOperation { channel: hex::encode(group.as_bytes()), source_device: reference.user_id() };
                loop {
                    let published = reference.account_document(&intent_key).unwrap()
                        .and_then(|document| OperationFrame::decode(document.payload(), &account.public().ed25519_pub))
                        .is_some_and(|frame| frame.projection.requests.iter().any(|row| row.reference.id == id));
                    if published { break; }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                record_b_trace(&trace, clock, "fixture", "original_signed_intent_published".into());
                source_worker = Some(tokio::spawn(source.clone().run_account_history_loop()));
                let key = crate::node::AccountDocumentKey::GroupOperation { channel: hex::encode(group.as_bytes()), source_device: source.user_id() };
                let control_key = crate::node::AccountDocumentKey::GroupControl { channel: hex::encode(group.as_bytes()), source_device: source.user_id() };
                loop {
                    let published = source.account_document(&key).unwrap()
                        .and_then(|document| OperationFrame::decode(document.payload(), &account.public().ed25519_pub))
                        .is_some_and(|frame| frame.projection.results.iter().any(|row| row.reference.id == id && row.status.phase == AccountGroupOperationPhase::Committed));
                    let published_left = source.account_document(&control_key).unwrap()
                        .and_then(|document| crate::node::account_group::wire::GroupControlHead::decode(document.payload(), &account.public().ed25519_pub))
                        .is_some_and(|head| head.access.source.role == AccountGroupRole::Left && head.access.observation.role == AccountGroupRole::Left);
                    if published && published_left { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                record_b_trace(&trace, clock, "fixture", "source_signed_terminal_published".into());
                source_worker.as_ref().unwrap().abort();
                while !source_worker.as_ref().unwrap().is_finished() { tokio::time::sleep(Duration::from_millis(1)).await; }
                dispatch.release();
                while !held_control.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(1)).await; }
                record_b_trace(&trace, clock, "fixture", "exact_control_noise_socket_held".into());
            } else {
                while !timeout_seen.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(10)).await; }
            }
            source.set_device_hostname(Some(if wake_during_attempt { "Published during failed control attempt" } else { "Published after failed control attempt" })).unwrap();
            let (epoch, revision) = source.account_wake_token_for_test();
            during.lock().unwrap().revision = Some(revision);
            use crate::node::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion};
            let mut wake = source.begin_account_exchange(&source.routing_peer(&reference.user_id()).unwrap(), AccountSyncVersion::V2,
                AccountHistoryFrame::Wake { account_id: account.account_id(), epoch, revision }).await.unwrap();
            let ack = wake.recv().await.unwrap();
            assert!(matches!(AccountHistoryFrame::decode_versioned(&ack), Some((AccountSyncVersion::V2, AccountHistoryFrame::WakeAck { epoch: observed_epoch, revision: observed_revision })) if observed_epoch == epoch && observed_revision == revision));
            wake.recycle(&source);
            if wake_during_attempt {
                let received_during = during.lock().unwrap().received_before_failure;
                let phases = trace.lock().unwrap().clone();
                assert!(received_during, "the exact authenticated wake reception must precede the original attempt failure; phases={phases:?}");
                while !timeout_seen.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(10)).await; }
            }
            release.close();
            loop {
                let status = reference.list_account_group_operations(&account.account_id(), AccountGroupOperationQuery { group: Some(group), pending_only: false, after: None, limit: 64 }).unwrap()
                    .into_iter().find(|row| row.id == id).unwrap();
                if status.phase == AccountGroupOperationPhase::Committed {
                    assert_eq!(status.committed_events.len(), 1);
                    if reference.account_group_access(group).unwrap().role == AccountGroupRole::Left { break; }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await;
        let source_phase = source.list_account_group_operations(&account.account_id(), AccountGroupOperationQuery { group: Some(group), pending_only: false, after: None, limit: 64 }).unwrap()
            .into_iter().find(|row| row.id == id).map(|row| row.phase);
        (receipt, source_phase, started.elapsed())
    }).catch_unwind().await;
    // Open every fixture wait before cancellation; tracked children retain the
    // nodes through actual retirement, including admitted blocking work.
    dispatch.release();
    directory.release();
    fault.close();
    release.close();
    source.runtime_work.close();
    reference.runtime_work.close();
    for task in [
        Some(source_server),
        Some(reference_server),
        reference_worker,
        source_worker,
    ]
    .into_iter()
    .flatten()
    {
        task.abort();
        let _ = task.await;
    }
    let mut drained = true;
    for node in [&source, &reference] {
        if tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .is_err()
        {
            drained = false;
        }
    }
    *reference.account_client_test_hook.lock().unwrap() = None;
    if !drained {
        let retained = dir.keep();
        panic!("Noise fixture work did not retire; retaining {retained:?}");
    }
    let (receipt, source_phase, elapsed) =
        outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    let phases = trace.lock().unwrap().clone();
    let claimed = fault.state.lock().unwrap().claimed;
    assert!(
        timeout_seen.load(Ordering::Acquire)
            && held_control.load(Ordering::Acquire)
            && claimed.is_some(),
        "a real exact Control Noise timeout must precede recovery; claimed={claimed:?}; phases={phases:?}"
    );
    assert!(
        phases
            .iter()
            .any(|(_, _, _, event)| event.starts_with("wake_received")),
        "a real newer authenticated wake must arrive"
    );
    assert_eq!(source_phase, Some(AccountGroupOperationPhase::Committed));
    assert!(
        receipt.is_ok(),
        "original Leave receipt and Left access must recover within unchanged 8s from admission; elapsed={elapsed:?}; phases={phases:?}"
    );
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Left
    );
}
