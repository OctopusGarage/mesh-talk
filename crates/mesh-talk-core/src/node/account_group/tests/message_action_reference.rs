//! Explicit account actions work through Noise while optional public history is off.
use super::*;
use crate::node::AccountGroupTextInput;

#[path = "reaction_account_reduction.rs"]
mod reaction_account_reduction;

type ActionTrace = Arc<
    std::sync::Mutex<(
        std::collections::VecDeque<(u128, &'static str, String)>,
        usize,
    )>,
>;

struct ReleaseActionControlOnDrop(Arc<tokio::sync::Semaphore>);

impl Drop for ReleaseActionControlOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn record_action_trace(
    trace: &ActionTrace,
    started: std::time::Instant,
    label: &'static str,
    event: String,
) {
    let mut trace = trace.lock().unwrap();
    if trace.0.len() == 1024 {
        trace.0.pop_front();
        trace.1 += 1;
    }
    trace
        .0
        .push_back((started.elapsed().as_millis(), label, event));
}

async fn wait_action_fixture(ready: impl FnMut() -> bool, message: &'static str) {
    wait_action_fixture_diagnosed(ready, || message).await;
}

async fn wait_action_fixture_diagnosed(
    mut ready: impl FnMut() -> bool,
    diagnosis: impl Fn() -> &'static str,
) {
    // Functional provenance/convergence watchdog; dedicated latency fixtures retain 8s.
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !ready() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{}", diagnosis()));
}

#[derive(Debug, Default)]
struct RoleBootstrapDiagnostics {
    // Binding, access capability, operation capability, absent source, metadata,
    // and decoded exact-source role failures. All fields are body-free.
    rejected: [usize; 6],
    captured_source_revision: Option<u64>,
    source_floor: Option<crate::node::AccountDocumentMeta>,
    observed_revisions: std::collections::VecDeque<(Option<u64>, Option<u64>)>,
}

impl RoleBootstrapDiagnostics {
    fn pin_first_source(&mut self, meta: Option<&crate::node::AccountDocumentMeta>) {
        if self.source_floor.is_none()
            && let Some(meta) = meta.filter(|meta| !meta.tombstone)
        {
            self.captured_source_revision = Some(meta.revision);
            self.source_floor = Some(meta.clone());
        }
    }
}

fn role_metadata_reaches_snapshot(
    actual: &crate::node::AccountDocumentMeta,
    floor: &crate::node::AccountDocumentMeta,
) -> bool {
    !actual.tombstone
        && actual.key == floor.key
        && actual.account_public == floor.account_public
        && actual.writer_device == floor.writer_device
        && (actual == floor || actual.revision > floor.revision)
}

fn authenticated_source_role_ready(
    target: &Node,
    source: &Node,
    group: crate::eventlog::ConversationId,
    diagnosis: &mut RoleBootstrapDiagnostics,
) -> bool {
    let public = source.identity.public();
    let key = crate::node::AccountDocumentKey::GroupControl {
        channel: hex::encode(group.as_bytes()),
        source_device: source.user_id(),
    };
    let expected = source
        .account_document_meta(&key)
        .filter(|meta| !meta.tombstone);
    let actual = target.account_document_meta(&key);
    diagnosis.pin_first_source(expected.as_ref());
    let revisions = (
        expected.as_ref().map(|meta| meta.revision),
        actual.as_ref().map(|meta| meta.revision),
    );
    if diagnosis.observed_revisions.back() != Some(&revisions) {
        if diagnosis.observed_revisions.len() == 16 {
            diagnosis.observed_revisions.pop_front();
        }
        diagnosis.observed_revisions.push_back(revisions);
    }
    let rejected = if !target.certified_sibling(&public) {
        Some(0)
    } else if !target
        .account_group_versions
        .lock()
        .unwrap()
        .get(&source.user_id())
        .is_some_and(|support| support.public == public)
    {
        Some(1)
    } else if !target
        .group_operation_versions
        .lock()
        .unwrap()
        .get(&source.user_id())
        .is_some_and(|support| support.public == public)
    {
        Some(2)
    } else if diagnosis.source_floor.is_none() {
        Some(3)
    } else if !actual
        .as_ref()
        .zip(diagnosis.source_floor.as_ref())
        .is_some_and(|(actual, floor)| role_metadata_reaches_snapshot(actual, floor))
    {
        Some(4)
    } else if !target
        .account_document(&key)
        .unwrap()
        .is_some_and(|document| {
            crate::node::account_group::wire::GroupControlHead::decode(
                document.payload(),
                &target.account.public().ed25519_pub,
            )
            .is_some_and(|head| {
                head.group == group
                    && head.proof.public() == public
                    && target.account_group_binding_current(&head.proof)
            })
        })
    {
        Some(5)
    } else {
        None
    };
    if let Some(reason) = rejected {
        diagnosis.rejected[reason] += 1;
        false
    } else {
        true
    }
}

// Keep the exact fixture retry classification in one owner. Cold protocol
// preparation and warm role proof share cursor/backoff semantics, not budgets.
pub(super) async fn accept_reference_control_page(
    target: &Node,
    source: &Node,
    peer: &crate::discovery::roster::PeerRecord,
    cursor: &crate::node::account_documents::sync::DocumentSyncCursor,
    result: Result<(), crate::node::NodeError>,
    interruptions: &mut usize,
) -> bool {
    match result {
        Ok(()) => {
            assert!(
                cursor.error.is_none() && !cursor.requires_upgrade,
                "control bootstrap must not hide a protocol error or capability rejection"
            );
            true
        }
        Err(error) => {
            let capacity_busy = crate::node::account_sync::local_sync_capacity_busy(&error);
            let page_interrupted = matches!(&error,
                crate::node::NodeError::Session(
                    crate::node::session::SessionError::Transport(
                        crate::transport::TransportError::Io(io)
                    )
                ) if io.kind() == std::io::ErrorKind::UnexpectedEof
            ) && cursor.pending
                && !cursor.up_to_date
                && cursor.error.is_none()
                && !cursor.requires_upgrade
                && target.certified_sibling(&peer.public)
                && peer.public == source.identity.public();
            assert!(
                capacity_busy || page_interrupted,
                "only local admission backpressure or authenticated pending GroupControl EOF is retryable: {error}"
            );
            if page_interrupted {
                *interruptions += 1;
                assert!(
                    *interruptions <= 3,
                    "role page repeatedly invalidated during bootstrap"
                );
            }
            // Honor both public retry_at and the owner's private short
            // GroupControl cooldown without discarding cursor progress.
            let delay = cursor
                .retry_at
                .map(|at| at.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_default()
                .max(std::time::Duration::from_millis(100));
            tokio::time::sleep(delay).await;
            false
        }
    }
}

fn reference_control_peer(
    target: &Node,
    source: &Node,
    address: std::net::SocketAddr,
) -> crate::discovery::roster::PeerRecord {
    target.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &source.identity,
            &source.account,
            "Reference action control",
            address.port(),
        ),
        address.ip(),
        &target.user_id(),
    );
    let peer = target.routing_peer(&source.user_id()).unwrap();
    assert_eq!(peer.public, source.identity.public());
    assert!(target.certified_sibling(&peer.public));
    peer
}

fn reference_control_protocol_ready(
    target: &Node,
    source: &Node,
    kind: Option<AccountGroupOperationKind>,
) -> bool {
    let public = source.identity.public();
    target.certified_sibling(&public)
        && target
            .account_group_versions
            .lock()
            .unwrap()
            .get(&source.user_id())
            .is_some_and(|support| support.public == public)
        && super::has_account_group_operation_support(target, source, kind)
}

struct ColdReferenceTrace<'a> {
    target: &'a Node,
    previous: Option<Arc<crate::node::account_session::AccountClientTestHook>>,
    installed: Arc<crate::node::account_session::AccountClientTestHook>,
    events: Arc<Mutex<std::collections::VecDeque<String>>>,
}
impl<'a> ColdReferenceTrace<'a> {
    fn install(target: &'a Node, source: &Node) -> Self {
        let events = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let mut owner = target.account_client_test_hook.lock().unwrap();
        let previous = owner.clone();
        let installed: Arc<crate::node::account_session::AccountClientTestHook> = Arc::new({
            let exact = source.identity.public();
            let previous = previous.clone();
            let events = events.clone();
            move |peer, event| {
                if peer == exact
                    && (event.starts_with("negotiation_")
                        || event.starts_with("dial_")
                        || event.starts_with("session_")
                        || event.starts_with("page_"))
                {
                    let mut events = events.lock().unwrap();
                    if events.len() == 32 {
                        events.pop_front();
                    }
                    events.push_back(event.chars().take(256).collect());
                }
                // An incumbent callback can block: forward only after all local
                // trace/installation guards have been released.
                if let Some(previous) = &previous {
                    previous(peer, event);
                }
            }
        });
        *owner = Some(installed.clone());
        drop(owner);
        Self {
            target,
            previous,
            installed,
            events,
        }
    }
}
impl Drop for ColdReferenceTrace<'_> {
    fn drop(&mut self) {
        let mut owner = self.target.account_client_test_hook.lock().unwrap();
        if owner
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.installed))
        {
            *owner = self.previous.take();
        }
        drop(owner);
        let events = self.events.lock().unwrap().clone();
        eprintln!("cold reference Control phases: {events:?}");
    }
}

// Inbound-only fixtures have no autonomous producer to observe. Drive only the
// required real Control exchange inside the caller's existing cold setup budget.
// A genuinely proven exact source is a no-op, including no additional RPC.
pub(super) async fn prepare_inbound_reference_control_protocol(
    target: &Arc<Node>,
    source: &Node,
    address: std::net::SocketAddr,
    kind: Option<AccountGroupOperationKind>,
) {
    let peer = reference_control_peer(target, source, address);
    if reference_control_protocol_ready(target, source, kind) {
        return;
    }
    let _trace = ColdReferenceTrace::install(target, source);
    eprintln!(
        "cold reference Control begin: source={} kind={kind:?}",
        source.user_id()
    );
    let mut cursor = crate::node::account_documents::sync::DocumentSyncCursor::control();
    let mut interruptions = 0;
    loop {
        let result = target.pull_account_documents_page(&peer, &mut cursor).await;
        let complete = accept_reference_control_page(
            target,
            source,
            &peer,
            &cursor,
            result,
            &mut interruptions,
        )
        .await;
        if complete && reference_control_protocol_ready(target, source, kind) {
            break;
        }
        if complete {
            // Retain the same cursor and existing short domain cooldown, as in
            // the warm proof; never turn a no-I/O pending page into a busy loop.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    eprintln!(
        "cold reference Control complete: source={} kind={kind:?}",
        source.user_id()
    );
}

/// Role-page publication can invalidate the authenticated page mid-transfer.
/// Retain the cursor and retry only that precise transport interruption; other
/// errors remain fixture failures, and operation execution keeps its own budget.
pub(super) async fn pull_reference_action_control(
    target: &Arc<Node>,
    source: &Node,
    address: std::net::SocketAddr,
    group: crate::eventlog::ConversationId,
) {
    let peer = reference_control_peer(target, source, address);
    let mut cursor = crate::node::account_documents::sync::DocumentSyncCursor::control();
    let mut interruptions = 0;
    let mut updates = target.subscribe_account_documents();
    let key = crate::node::AccountDocumentKey::GroupControl {
        channel: hex::encode(group.as_bytes()),
        source_device: source.user_id(),
    };
    let mut diagnosis = RoleBootstrapDiagnostics::default();
    diagnosis.pin_first_source(source.account_document_meta(&key).as_ref());
    if authenticated_source_role_ready(target, source, group, &mut diagnosis) {
        return;
    }
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let result = {
                let pull = target.pull_account_documents_page(&peer, &mut cursor);
                tokio::pin!(pull);
                loop {
                    tokio::select! {
                        biased;
                        result = &mut pull => break result,
                        changed = updates.changed() => {
                            assert!(changed.is_ok(), "the local document observer must remain live");
                            if authenticated_source_role_ready(target,source,group, &mut diagnosis) {return;}
                        }
                    }
                }
            };
            if accept_reference_control_page(target, source, &peer, &cursor, result, &mut interruptions).await {
                if authenticated_source_role_ready(target,source,group, &mut diagnosis) { break; }
                // A completed response may still leave a private short domain
                // cooldown; retain the cursor until the exact source row arrives.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("the exact authenticated source GroupControl row must arrive within the bootstrap budget: captured_source_revision={:?}, rejected={:?}, observed_revisions={:?}",
            diagnosis.captured_source_revision, diagnosis.rejected, diagnosis.observed_revisions);
    });
}

// Keep simulated remote devices on their own reactor/scheduling thread while
// B retains its current-thread capability override + synchronous admission seam.
pub(super) struct ActionPeerRuntime {
    launch: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    handle: tokio::runtime::Handle,
}
impl ActionPeerRuntime {
    pub(super) async fn start(node: Arc<Node>, listener: TcpListener) -> Self {
        let listener = listener.into_std().unwrap();
        let (shutdown, mut stopped) = tokio::sync::oneshot::channel();
        let (launch, launched) = tokio::sync::oneshot::channel();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("account-action-peer".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = TcpListener::from_std(listener).unwrap();
                    ready.send(tokio::runtime::Handle::current()).unwrap();
                    let launched = tokio::select! {
                        _ = &mut stopped => false,
                        result = launched => result.is_ok(),
                    };
                    let task = if launched {
                        let task = tokio::spawn(node.clone().run_accept_loop(listener));
                        let _ = stopped.await;
                        Some(task)
                    } else {
                        None
                    };
                    node.runtime_work.close();
                    if let Some(task) = task {
                        task.abort();
                        if let Err(error) = task.await {
                            assert!(
                                error.is_cancelled(),
                                "owned peer server failed before shutdown"
                            );
                        }
                    }
                    node.sessions.clear();
                    node.runtime_work.drain().await;
                });
            })
            .unwrap();
        Self {
            launch: Some(launch),
            shutdown: Some(shutdown),
            thread: Some(thread),
            handle: ready_rx.await.unwrap(),
        }
    }
    pub(super) fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }
    pub(super) fn launch(&mut self) {
        self.launch.take().unwrap().send(()).unwrap();
    }
    pub(super) async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let thread = self.thread.take().unwrap();
        let joined = tokio::task::spawn_blocking(move || thread.join())
            .await
            .unwrap();
        assert!(
            joined.is_ok(),
            "owned peer runtime must finish its shutdown and writer drain"
        );
    }
}
impl Drop for ActionPeerRuntime {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let joined = thread.join();
            if !std::thread::panicking() {
                assert!(joined.is_ok(), "owned peer runtime failed during cleanup");
            }
        }
    }
}

#[tokio::test]
async fn independent_action_peer_runtime_joins_accepted_connections_and_admitted_writers() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let reference = node(&directory.path().join("reference"), &account);
    link(&source, &reference);
    link(&reference, &source);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&source, &directory.path().join("source"), address.port());
    configure(&reference, &directory.path().join("reference"), 1);
    source.roster.lock().unwrap().update(
        &reference.signed_announce("fixture reference", 1),
        address.ip(),
        &source.user_id(),
    );
    reference.roster.lock().unwrap().update(
        &source.signed_announce("fixture source", address.port()),
        address.ip(),
        &reference.user_id(),
    );
    let mut owner = ActionPeerRuntime::start(source.clone(), listener).await;
    owner.launch();
    let peer_thread = owner
        .handle
        .spawn(async { std::thread::current().id() })
        .await
        .unwrap();
    assert_ne!(peer_thread, std::thread::current().id());
    let channel = reference
        .privacy_sync_dial(address, &source.identity.public())
        .await
        .unwrap();
    assert_eq!(channel.peer_identity(), &source.identity.public());
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let written = completed.clone();
    let writer_node = source.clone();
    let writer = owner.handle.spawn(async move {
        writer_node
            .account_blocking(move |_| {
                entered.send(()).unwrap();
                release_rx.recv().unwrap();
                written.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    let mut stopping = tokio::spawn(owner.stop());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut stopping)
            .await
            .is_err(),
        "stop must retain the peer runtime while an admitted blocking writer remains active"
    );
    assert!(source.runtime_work.is_closed());
    release.send(()).unwrap();
    stopping.await.unwrap();
    assert!(completed.load(std::sync::atomic::Ordering::Acquire));
    match writer.await {
        Ok(result) => result.unwrap(),
        Err(error) => assert!(
            error.is_cancelled(),
            "retirement may cancel the waiter, never conceal a writer panic"
        ),
    }
    source.runtime_work.drain().await;
    drop(channel);
}

async fn reference_message_action(recall: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("offline-owner"), &Account::generate());
    let source = node(&dir.path().join("source"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let sibling = node(&dir.path().join("sibling"), &account);
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
            name: "Reference message actions".into(),
            members: vec![
                owner.identity.public(),
                source.identity.public(),
                sibling.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
    for target in [&source, &sibling, &external] {
        install(target, &genesis);
    }
    link(&source, &reference);
    link(&reference, &source);
    let nodes = [&source, &reference, &external, &sibling];
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for (name, target) in [
        ("source", &source),
        ("reference", &reference),
        ("external", &external),
        ("sibling", &sibling),
    ] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push(listener);
    }
    // Authenticate A/B/C first. D has real physical membership but is not yet
    // introduced as a same-account executor, so the original send selects A.
    for left in nodes.iter().take(3) {
        for (right, address) in nodes.iter().take(3).zip(&addresses) {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Reference action fixture", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    let trace: ActionTrace = Arc::new(std::sync::Mutex::new((
        std::collections::VecDeque::new(),
        0,
    )));
    let trace_started = std::time::Instant::now();
    let peer_labels: std::collections::HashMap<_, _> = [
        (source.user_id(), "source"),
        (reference.user_id(), "reference"),
        (external.user_id(), "external"),
        (sibling.user_id(), "sibling"),
    ]
    .into_iter()
    .collect();
    for (label, node) in [
        ("source", &source),
        ("reference", &reference),
        ("sibling", &sibling),
    ] {
        let client_trace = trace.clone();
        let labels = peer_labels.clone();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |peer, event| {
            let peer = labels.get(&peer.user_id()).copied().unwrap_or("unknown");
            record_action_trace(
                &client_trace,
                trace_started,
                label,
                format!("client peer={peer} {event}"),
            );
        }));
        let trace = trace.clone();
        let labels = peer_labels.clone();
        *node.account_session_test_hook.lock().unwrap() = Some(Arc::new(move |peer, frame| {
            let trace = trace.clone();
            let peer = labels.get(&peer.user_id()).copied().unwrap_or("unknown");
            Box::pin(async move {
                use crate::node::account_sync_wire::AccountHistoryFrame;
                let event = match frame {
                    AccountHistoryFrame::DocumentsRequest { body } => {
                        crate::node::account_documents::wire::DocumentRequest::decode(&body).map(
                            |request| {
                                format!(
                                    "request from={peer} domain={:?} after={} probe={}",
                                    request.domain,
                                    request.after.is_some(),
                                    request.operation.is_some()
                                )
                            },
                        )
                    }
                    AccountHistoryFrame::DocumentHeader { body } => {
                        serde_json::from_slice::<crate::node::AccountDocumentMeta>(&body)
                            .ok()
                            .map(|meta| {
                                format!(
                                    "header to={peer} domain={:?} revision={} hash={}",
                                    meta.key.domain(),
                                    meta.revision,
                                    &hex::encode(meta.content_hash)[..12]
                                )
                            })
                    }
                    AccountHistoryFrame::DocumentsComplete { body } => {
                        serde_json::from_slice::<serde_json::Value>(&body)
                            .ok()
                            .map(|complete| {
                                format!(
                                    "complete to={peer} domain={} next={} operation={}",
                                    complete["domain"],
                                    !complete["next"].is_null(),
                                    !complete["operation"].is_null()
                                )
                            })
                    }
                    _ => None,
                };
                if let Some(event) = event {
                    record_action_trace(&trace, trace_started, label, event);
                }
            })
        }));
    }
    let mut tasks = tokio::task::JoinSet::new();
    for (label, node) in [
        ("source", source.clone()),
        ("reference", reference.clone()),
        ("sibling", sibling.clone()),
    ] {
        let trace = trace.clone();
        let mut updates = node.subscribe_account_documents();
        let writers = [source.user_id(), reference.user_id(), sibling.user_id()];
        tasks.spawn(async move {
            let mut observed = std::collections::BTreeMap::new();
            loop {
                for writer in &writers {
                    let key = crate::node::AccountDocumentKey::GroupOperation {
                        channel: hex::encode(group.as_bytes()),
                        source_device: writer.clone(),
                    };
                    if let Some(meta) = node.account_document_meta(&key) {
                        let current = (meta.revision, meta.content_hash);
                        if observed.get(writer) != Some(&current) {
                            observed.insert(writer.clone(), current);
                            record_action_trace(
                                &trace,
                                trace_started,
                                label,
                                format!(
                                    "durable operation writer={} revision={} hash={}",
                                    &writer[..8],
                                    meta.revision,
                                    &hex::encode(meta.content_hash)[..12]
                                ),
                            );
                        }
                    }
                }
                if updates.changed().await.is_err() {
                    break;
                }
            }
        });
    }
    let mut peer_runtimes = Vec::new();
    let mut external_runtime = None;
    let mut reference_launch = None;
    for (target, listener) in nodes.into_iter().zip(listeners) {
        if target.user_id() != reference.user_id() {
            let runtime = ActionPeerRuntime::start(target.clone(), listener).await;
            if target.user_id() == external.user_id() {
                external_runtime = Some(runtime.handle.clone());
            }
            peer_runtimes.push(runtime);
        } else {
            let (launch, launched) = tokio::sync::oneshot::channel();
            let target = target.clone();
            tasks.spawn(async move {
                launched.await.unwrap();
                Box::pin(target.run_accept_loop(listener)).await;
            });
            reference_launch = Some(launch);
        }
    }
    for runtime in &mut peer_runtimes {
        runtime.launch();
    }
    if let Some(launch) = reference_launch {
        launch.send(()).unwrap();
    }
    let external_runtime = external_runtime.expect("the external peer owns its runtime");
    source.publish_account_group_access(&mut None);
    super::cold_account_control_setup(
        super::observe_account_fixture_condition(|| {
            super::has_account_group_operation_support(
                &reference,
                &source,
                Some(AccountGroupOperationKind::Text),
            ) && super::has_account_group_operation_support(&source, &reference, None)
        }),
        pull_reference_action_control(&reference, &source, addresses[0], group),
    )
    .await;
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert!(reference.channel_members(group).is_empty());

    let send_id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let sent = reference
        .submit_account_group_text_if(
            &account.account_id(),
            send_id,
            group,
            AccountGroupTextInput {
                text: b"original account action target".to_vec(),
                reply_to: None,
                expires_at_ms: None,
                protection: None,
            },
            |commit| commit(),
        )
        .unwrap();
    assert_eq!(sent.executions[0].device_id, source.user_id());
    wait_action_fixture(
        || {
            reference
                .account_group_operation_status(&account.account_id(), send_id)
                .unwrap()
                .is_some_and(|status| {
                    status.phase == AccountGroupOperationPhase::Committed
                        && status.primary_event.is_some_and(|id| {
                            external.history(group, 20).iter().any(|row| row.id == id)
                        })
                })
        },
        "the original A-authored send and private receipt must arrive over Noise",
    )
    .await;
    let target = reference
        .account_group_operation_status(&account.account_id(), send_id)
        .unwrap()
        .unwrap()
        .primary_event
        .unwrap();
    assert!(
        reference
            .history(group, 20)
            .iter()
            .any(|row| row.id == target && row.from_me)
    );
    assert!(
        reference.log.lock().unwrap().get(&target).is_none(),
        "History OFF must leave B with the original private send receipt, not public message history"
    );
    let original = external.log.lock().unwrap().get(&target).cloned().unwrap();
    assert_eq!(
        original.author.ed25519_pub(),
        &source.identity.public().ed25519_pub
    );
    assert!(original.verify_integrity() && original.verify_signature());

    // The later sibling is cold even though the original author is ready.
    // Hold its real authenticated response beyond the role-page budget; this
    // delay belongs to capability setup, before any Recall is admitted.
    let sibling_control_release = recall.then(|| Arc::new(tokio::sync::Semaphore::new(0)));
    let _release_on_drop = sibling_control_release
        .as_ref()
        .map(|release| ReleaseActionControlOnDrop(release.clone()));
    if let Some(release) = &sibling_control_release {
        let previous = sibling.account_session_test_hook.lock().unwrap().clone();
        let (arrived, held) = tokio::sync::oneshot::channel();
        let arrived = Arc::new(Mutex::new(Some(arrived)));
        let expected_reference = reference.identity.public();
        *sibling.account_session_test_hook.lock().unwrap() = Some(Arc::new({
            let release = release.clone();
            move |peer, frame| {
                let previous = previous.clone();
                let release = release.clone();
                let arrived = arrived.clone();
                let expected_reference = expected_reference.clone();
                Box::pin(async move {
                    if let Some(previous) = previous {
                        previous(peer.clone(), frame.clone()).await;
                    }
                    if peer != expected_reference {
                        return;
                    }
                    let crate::node::account_sync_wire::AccountHistoryFrame::DocumentsRequest {
                        body,
                    } = frame
                    else {
                        return;
                    };
                    let request =
                        crate::node::account_documents::wire::DocumentRequest::decode(&body)
                            .expect("authenticated sibling control request must decode");
                    if request.domain == AccountDocumentDomain::GroupControl {
                        if let Some(arrived) = arrived.lock().unwrap().take() {
                            let _ = arrived.send(());
                        }
                        let _ = release.acquire().await;
                    }
                })
            }
        }));
        let release = release.clone();
        tasks.spawn(async move {
            super::cold_account_setup(held)
                .await
                .expect("the later sibling must enter its real cold capability hold")
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            release.close();
        });
    }

    // Introduce an independently capable same-account physical executor only
    // after A's original provenance is fixed. These are real device certificates
    // and authenticated operation support, with no synthetic membership grants.
    for left in [&source, &reference, &sibling] {
        for right in [&source, &reference, &sibling] {
            if left.user_id() != right.user_id() {
                link(left, right);
            }
        }
    }
    for left in nodes {
        for (right, address) in nodes.into_iter().zip(&addresses) {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Capable sibling", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    sibling.publish_account_group_access(&mut None);
    let kind = if recall {
        AccountGroupOperationKind::Recall
    } else {
        AccountGroupOperationKind::Reaction
    };
    super::cold_account_control_setup(
        super::observe_account_fixture_condition(|| {
            super::has_account_group_operation_support(&reference, &sibling, Some(kind))
                && super::has_account_group_operation_support(&sibling, &reference, None)
        }),
        pull_reference_action_control(&reference, &sibling, addresses[3], group),
    )
    .await;
    if let Some(release) = &sibling_control_release {
        assert!(
            release.is_closed(),
            "the later sibling must finish cold setup before its role qualifies"
        );
    }
    wait_action_fixture(
        || {
            [source.as_ref(), sibling.as_ref()]
                .into_iter()
                .all(|candidate| {
                    reference
                        .group_operation_versions
                        .lock()
                        .unwrap()
                        .get(&candidate.user_id())
                        .is_some_and(|support| {
                            support.public == candidate.identity.public()
                                && support.kinds & kind.capability_bit() != 0
                        })
                })
                && reference
                    .account_group_access(group)
                    .unwrap()
                    .sources
                    .iter()
                    .any(|candidate| candidate.device == sibling.identity.public())
        },
        "both physical account executors must authenticate action support",
    )
    .await;

    let turns = if recall { 1 } else { 2 };
    let mut effect_ids = Vec::new();
    for turn in 0..turns {
        let operation = if recall {
            AccountGroupOperation::Recall { target }
        } else {
            AccountGroupOperation::React {
                target,
                emoji: "👍".into(),
                remove: turn == 1,
            }
        };
        let id = reference
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        // Establish A's add first, then make A unavailable for later React
        // actions. D's remove must address the same logical account
        // reaction while preserving each original physical event author.
        // Recall still pins A even when D is more ready.
        let held_device = if recall || turn > 0 {
            source.user_id()
        } else {
            sibling.user_id()
        };
        let original_kinds = {
            let mut support = reference.group_operation_versions.lock().unwrap();
            let selected = support.get_mut(&held_device).unwrap();
            let original = selected.kinds;
            selected.kinds &= !kind.capability_bit();
            original
        };
        let admitted = reference
            .submit_account_group_operation(&account.account_id(), id, group, operation.clone())
            .unwrap();
        reference
            .group_operation_versions
            .lock()
            .unwrap()
            .get_mut(&held_device)
            .unwrap()
            .kinds = original_kinds;
        assert_eq!(
            admitted.executions[0].device_id,
            if recall || turn == 0 {
                source.user_id()
            } else {
                sibling.user_id()
            },
            "React must switch to capable D; Recall must retain original author A"
        );
        record_action_trace(
            &trace,
            trace_started,
            "reference",
            format!("admitted turn={turn}"),
        );
        let mut observed_phases = [false; 3];
        wait_action_fixture_diagnosed(
            || {
                for (index, label, ready) in [
                    (0, "source", (if recall || turn == 0 { &source } else { &sibling })
                        .account_group_operation_status(&account.account_id(), id).unwrap()
                        .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed)),
                    (1, "reference", reference.account_group_operation_status(&account.account_id(), id).unwrap()
                        .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed)),
                    (2, "external", (if recall || turn == 0 { &source } else { &sibling })
                        .account_group_operation_status(&account.account_id(), id).unwrap()
                        .and_then(|status| status.primary_event).is_some_and(|event| external.log.lock().unwrap().get(&event).is_some())),
                ] {
                    if ready && !observed_phases[index] {
                        observed_phases[index] = true;
                        record_action_trace(&trace, trace_started, label, format!("committed_or_effect turn={turn}"));
                    }
                }
                reference
                    .account_group_operation_status(&account.account_id(), id)
                    .unwrap()
                    .is_some_and(|status| {
                        status.phase == AccountGroupOperationPhase::Committed
                            && status.primary_event.is_some()
                            && (if recall || turn == 0 { &source } else { &sibling })
                                .account_group_operation_status(&account.account_id(), id).unwrap()
                                .is_some_and(|original| original.phase == AccountGroupOperationPhase::Committed
                                    && original.primary_event == status.primary_event
                                    && original.committed_events == status.committed_events)
                    })
            },
            || {
                eprintln!("action propagation trace (before-write frame hooks; durable events are committed): {:?}", trace.lock().unwrap());
                let selected = if recall || turn == 0 { &source } else { &sibling };
                if selected.account_group_operation_status(&account.account_id(), id).unwrap()
                    .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed) {
                    "private action receipt: committed source result has not reached the requester"
                } else {
                    "private action receipt: selected source has not committed the original intent"
                }
            },
        )
        .await;
        eprintln!(
            "action client sample turn={turn}: {:?}",
            trace
                .lock()
                .unwrap()
                .0
                .iter()
                .filter(|(_, _, event)| event.contains("client peer=")
                    && (event.contains("page_end") || event.contains("negotiation_end")))
                .rev()
                .take(12)
                .collect::<Vec<_>>()
        );
        eprintln!(
            "action propagation timing turn={turn}: {:?}",
            trace
                .lock()
                .unwrap()
                .0
                .iter()
                .filter(|(_, _, event)| event.starts_with("admitted")
                    || event.starts_with("committed_or_effect"))
                .collect::<Vec<_>>()
        );
        let status = reference
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        let effect_id = status.primary_event.unwrap();
        let selected = if recall || turn == 0 {
            &source
        } else {
            &sibling
        };
        let private_effect = reference
            .account_group_operation_inputs()
            .into_iter()
            .flat_map(|(_, _, frame)| frame.projection.results)
            .find(|result| result.reference.id == id)
            .and_then(|result| result.message_event)
            .expect("private Committed must retain its exact signed original effect");
        assert_eq!(private_effect.id, effect_id);
        assert!(private_effect.verify_integrity() && private_effect.verify_signature());
        assert_eq!(
            private_effect.author.ed25519_pub(),
            &selected.identity.public().ed25519_pub
        );
        if recall {
            assert!(
                reference
                    .recalls
                    .lock()
                    .unwrap()
                    .targets(&group)
                    .contains(&target),
                "private Recall must be durable before any explicit public sync"
            );
            assert!(
                reference
                    .history(group, 20)
                    .iter()
                    .any(|row| row.id == target && row.recalled)
            );
        } else {
            assert!(
                reference
                    .my_dm_reactions
                    .lock()
                    .unwrap()
                    .channel_entries(group)
                    .iter()
                    .any(
                        |(author, saved, _, _)| *author == selected.identity.public()
                            && *saved == effect_id
                    ),
                "private reaction provenance must be durable before any explicit public sync"
            );
        }
        // Private commitment is independent of optional history and background
        // public fanout. Exercise authenticated public receipt as its own seam;
        // never install the source event or plaintext directly in the receiver.
        let observing = external.clone();
        let selected_public = selected.identity.public();
        let public_receipt = async move {
            let peer = observing.routing_peer(&selected_public.user_id()).unwrap();
            assert_eq!(peer.public, selected_public);
            let mut connection = observing
                .privacy_sync_dial(peer.addr, &peer.public)
                .await
                .expect("the accepted external member must authenticate the original executor");
            let store = observing.pull_sync_store(connection.peer_identity());
            super::super::super::session::request_round(&mut connection, &store, group)
                .await
                .expect("the normal authorized public sync must deliver the original event");
            drop(store);
            observing.recycle_sync_channel(connection);
            observing.emit_new_messages(group);
        };
        external_runtime.spawn(public_receipt).await.unwrap();
        effect_ids.push(effect_id);
        assert_eq!(status.id, id);
        assert_eq!(status.committed_events, vec![effect_id]);
        let effect = external
            .log
            .lock()
            .unwrap()
            .get(&effect_id)
            .cloned()
            .unwrap();
        assert!(effect.verify_integrity() && effect.verify_signature());
        assert_eq!(
            effect.kind,
            if recall {
                EventKind::Delete
            } else {
                EventKind::React
            }
        );
        let sealed = crate::node::channel::SealedPayload::decode(&effect.ciphertext).unwrap();
        let executor = if effect.author.ed25519_pub() == &source.identity.public().ed25519_pub {
            &source
        } else {
            assert_eq!(
                effect.author.ed25519_pub(),
                &sibling.identity.public().ed25519_pub
            );
            &sibling
        };
        let plaintext = sealed
            .open(&external.identity, &executor.identity.public().x25519_pub)
            .unwrap();
        if recall {
            assert_eq!(effect.author, original.author);
            assert_eq!(
                crate::node::recall::DeletePayload::decode(&plaintext)
                    .unwrap()
                    .target,
                target
            );
            assert!(
                external
                    .history(group, 20)
                    .iter()
                    .any(|row| row.id == target && row.recalled)
            );
            assert!(
                reference
                    .recalls
                    .lock()
                    .unwrap()
                    .targets(&group)
                    .contains(&target),
                "the committed explicit Recall must durably project privately on B with optional History OFF"
            );
            assert!(
                reference
                    .history(group, 20)
                    .iter()
                    .any(|row| row.id == target && row.recalled)
            );
        } else {
            let payload = crate::node::reaction::ReactionPayload::decode(&plaintext).unwrap();
            assert_eq!(payload.target, target);
            assert_eq!(payload.emoji, "👍");
            assert_eq!(payload.remove, turn == 1);
            let external_views = external.reactions(group);
            let private_views = reference.reactions(group);
            if turn == 0 {
                assert!(
                    external_views
                        .iter()
                        .any(|row| row.target == target.to_hex()
                            && row.emoji == "👍"
                            && row.who.len() == 1),
                    "the original A add must count as one logical reaction"
                );
                assert!(
                    private_views.iter().any(|row| row.target == target.to_hex()
                        && row.emoji == "👍"
                        && row.who.len() == 1),
                    "the requester must reduce certified source authors by their authenticated account"
                );
            } else {
                assert!(
                    external_views
                        .iter()
                        .all(|row| row.target != target.to_hex() || row.emoji != "👍")
                );
                assert!(
                    private_views
                        .iter()
                        .all(|row| row.target != target.to_hex() || row.emoji != "👍")
                );
            }
            let rows = reference
                .my_dm_reactions
                .lock()
                .unwrap()
                .channel_entries(group);
            assert!(
                rows.iter().any(|(author, saved_id, _, saved)| *author
                    == executor.identity.public()
                    && *saved_id == effect_id
                    && *saved == payload),
                "terminal adoption must retain the exact original effect in the durable channel projection"
            );
            if turn > 0 {
                assert_eq!(
                    rows.len(),
                    2,
                    "account reduction must retain A and D physical provenance"
                );
                assert!(rows.iter().any(|(author, saved_id, _, saved)| *author
                    == source.identity.public()
                    && *saved_id == effect_ids[0]
                    && !saved.remove));
                assert!(rows.iter().any(|(author, saved_id, _, saved)| *author
                    == sibling.identity.public()
                    && *saved_id == effect_id
                    && saved.remove == (turn == 1)));
            }
        }
        assert_eq!(
            reference
                .submit_account_group_operation(&account.account_id(), id, group, operation)
                .unwrap(),
            status
        );
        assert!(reference.channel_members(group).is_empty());
        assert!(
            reference.log.lock().unwrap().get(&effect_id).is_none(),
            "private action results must not install public history or physical membership on B"
        );
    }
    for holder in [&source, &sibling, &external] {
        let log = holder.log.lock().unwrap();
        for effect_id in &effect_ids {
            let count = log
                .events(&group)
                .into_iter()
                .filter(|event| event.id == *effect_id)
                .count();
            if holder.user_id() == external.user_id()
                || holder.user_id() == source.user_id() && recall
            {
                assert_eq!(
                    count, 1,
                    "same original operation retries must not duplicate public effects"
                );
            } else {
                assert!(count <= 1);
            }
        }
    }
    reference.runtime_work.close();
    tasks.shutdown().await;
    reference.sessions.clear();
    reference.runtime_work.drain().await;
    for runtime in peer_runtimes {
        runtime.stop().await;
    }
}

// Admission temporarily overrides cached support without awaiting. A single
// runtime thread keeps live capability responses from replacing that override
// between cache mutation and synchronous executor selection.
#[tokio::test]
async fn reference_member_reacts_over_noise_with_private_projection_and_history_off() {
    reference_message_action(false).await;
}

#[tokio::test]
async fn reference_member_recall_pins_original_author_over_ready_sibling_with_history_off() {
    reference_message_action(true).await;
}

#[tokio::test]
async fn cold_inbound_control_proves_capabilities_before_the_warm_role_clock() {
    use futures_util::FutureExt;
    use std::{future::Future, panic::AssertUnwindSafe, task::Poll};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    link(&source, &target);
    link(&target, &source);
    let group = source
        .create_channel("Cold inbound role", vec![])
        .await
        .unwrap();
    source.publish_account_group_access_for(group);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&source, &dir.path().join("source"), address.port());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let _release = ReleaseActionControlOnDrop(release.clone());
    let server_ready = Arc::new(tokio::sync::Notify::new());
    let client_ready = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    *source.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let exact = target.identity.public();
        let ready = server_ready.clone();
        let release = release.clone();
        let requests = requests.clone();
        move |peer, frame| {
            let exact = exact.clone();
            let ready = ready.clone();
            let release = release.clone();
            let requests = requests.clone();
            Box::pin(async move {
                if peer == exact
                    && matches!(frame, crate::node::account_sync_wire::AccountHistoryFrame::DocumentsRequest { ref body }
                        if crate::node::account_documents::wire::DocumentRequest::decode(body)
                            .is_some_and(|request| request.domain == AccountDocumentDomain::GroupControl))
                {
                    requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    ready.notify_one();
                    let _ = release.acquire().await;
                }
            })
        }
    }));
    *target.account_document_read_test_hook.lock().unwrap() = Some(Arc::new({
        let exact = source.identity.public();
        let ready = client_ready.clone();
        move |peer, request| {
            let exact = exact.clone();
            let ready = ready.clone();
            Box::pin(async move {
                if peer == exact && request.domain == AccountDocumentDomain::GroupControl {
                    ready.notify_one();
                }
            })
        }
    }));
    let server = inbound_server(source.clone(), listener);
    let attempt =
        AssertUnwindSafe(pull_group_control(&target, &source, address, group)).catch_unwind();
    tokio::pin!(attempt);
    let early = super::cold_account_setup(async {
        tokio::select! {
            result = &mut attempt => Some(result),
            _ = async { server_ready.notified().await; client_ready.notified().await; } => None,
        }
    })
    .await
    .expect("both real authenticated Ready owners must reach cold preparation");
    let (pending_after_stimulus, outcome) = if let Some(result) = early {
        (false, result)
    } else {
        tokio::time::pause();
        // Explicitly cross the existing warm 3s budget while the genuine first
        // response is withheld, still below the unchanged production read clock.
        tokio::time::advance(std::time::Duration::from_secs(4)).await;
        let observed = std::future::poll_fn(|cx| Poll::Ready(attempt.as_mut().poll(cx))).await;
        tokio::time::resume();
        release.close();
        match observed {
            Poll::Pending => (true, attempt.await),
            Poll::Ready(result) => (false, result),
        }
    };
    release.close();
    let before = requests.load(std::sync::atomic::Ordering::SeqCst);
    let repeated = if outcome.is_ok() {
        Some(
            AssertUnwindSafe(pull_group_control(&target, &source, address, group))
                .catch_unwind()
                .await,
        )
    } else {
        None
    };
    let after = requests.load(std::sync::atomic::Ordering::SeqCst);
    *target.account_document_read_test_hook.lock().unwrap() = None;
    *source.account_session_test_hook.lock().unwrap() = None;
    source.runtime_work.close();
    target.runtime_work.close();
    server.abort();
    let _ = server.await;
    super::cold_account_setup(async {
        source.runtime_work.drain().await;
        target.runtime_work.drain().await;
    })
    .await
    .expect("the inbound role fixture must retire its actual work");
    assert!(
        repeated.is_some_and(|outcome| outcome.is_ok()),
        "the genuinely warm source must remain ready"
    );
    assert_eq!(
        before, after,
        "proven exact capabilities and role need no additional RPC"
    );
    assert!(
        pending_after_stimulus,
        "the first authenticated response belongs to cold setup, not the warm role clock"
    );
    assert!(
        outcome.is_ok(),
        "real cold capabilities and the exact signed role must complete after release"
    );
    assert_eq!(
        target.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn role_bootstrap_returns_when_the_authenticated_source_row_is_durable_before_a_held_tail() {
    use crate::node::{AccountDocumentKey, account_documents::sync::DocumentSyncCursor};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    link(&source, &target);
    link(&target, &source);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&source, &dir.path().join("source"), address.port());
    target.roster.lock().unwrap().update(
        &source.signed_announce("Role bootstrap", address.port()),
        address.ip(),
        &target.user_id(),
    );
    // This fixture isolates one immutable authenticated page, not autonomous
    // account publication while the page is deliberately held.
    let server = inbound_server(source.clone(), listener);
    let peer = target.routing_peer(&source.user_id()).unwrap();
    // A prior complete, authenticated page proves this exact source's protocol.
    // A first-ever incomplete capability response must never qualify instead.
    target
        .pull_account_documents_page(&peer, &mut DocumentSyncCursor::control())
        .await
        .unwrap();
    assert!(
        target
            .account_group_versions
            .lock()
            .unwrap()
            .get(&source.user_id())
            .is_some_and(|support| support.public == source.identity.public())
    );
    let first = source
        .create_channel("First valid role", vec![])
        .await
        .unwrap();
    let second = source
        .create_channel("Unrelated page tail", vec![])
        .await
        .unwrap();
    // The requested role is emitted first; only an unrelated tail is held.
    let group = if first.as_bytes() < second.as_bytes() {
        first
    } else {
        second
    };
    source.publish_account_group_access(&mut None);
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (arrived, held) = tokio::sync::oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let headers = Arc::new(AtomicUsize::new(0));
    let expected = target.identity.public();
    *source.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let release = release.clone();
        move |peer, frame| {
            let release = release.clone();
            let arrived = arrived.clone();
            let headers = headers.clone();
            let expected = expected.clone();
            Box::pin(async move {
                if peer != expected {
                    return;
                }
                let crate::node::account_sync_wire::AccountHistoryFrame::DocumentHeader { body } =
                    frame
                else {
                    return;
                };
                let meta: crate::node::AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                if matches!(meta.key, AccountDocumentKey::GroupControl { .. })
                    && headers.fetch_add(1, Ordering::SeqCst) == 1
                {
                    if let Some(signal) = arrived.lock().unwrap().take() {
                        let _ = signal.send(());
                    }
                    let _ = release.acquire().await;
                }
            })
        }
    }));
    let pulling = {
        let source = source.clone();
        let target = target.clone();
        tokio::spawn(async move {
            pull_reference_action_control(&target, &source, address, group).await;
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), held)
        .await
        .unwrap()
        .unwrap();
    let wanted = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if target
                .account_documents(AccountDocumentDomain::GroupControl, None, 64)
                .unwrap()
                .items
                .iter()
                .any(|meta| {
                    meta.key
                        == AccountDocumentKey::GroupControl {
                            channel: hex::encode(group.as_bytes()),
                            source_device: source.user_id(),
                        }
                        && source.account_document_meta(&meta.key).as_ref() == Some(meta)
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let completed_while_tail_held = pulling.await;
    release.close();
    source.runtime_work.close();
    server.abort();
    let _ = server.await;
    source.runtime_work.drain().await;
    assert!(
        wanted.is_ok(),
        "the exact signed source role must already be durable"
    );
    assert!(
        completed_while_tail_held.is_ok(),
        "an unrelated page tail must not hide an already durable authenticated source role"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn role_bootstrap_refreshes_the_requested_group_after_a_real_owner_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    link(&source, &target);
    link(&target, &source);
    let desired = legacy_group(&source, &external);
    let unrelated = legacy_group(&source, &external);
    for genesis in [&desired, &unrelated] {
        install(&source, genesis);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&source, &dir.path().join("source"), address.port());
    source.publish_account_group_access(&mut None);
    let server = inbound_server(source.clone(), listener);
    let group = desired.conversation_id;
    pull_group_control(&target, &source, address, group).await;
    assert_eq!(
        target.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    let other_key = crate::node::AccountDocumentKey::GroupControl {
        channel: hex::encode(unrelated.conversation_id.as_bytes()),
        source_device: source.user_id(),
    };
    assert_eq!(
        target.account_document_meta(&other_key),
        source.account_document_meta(&other_key)
    );
    source
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    source.process_channel(group);
    source.publish_account_group_access_for(group);
    assert_eq!(
        source.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    // This other group remains fresh. It must not make the stale desired group
    // qualify, nor can the previously authenticated Owner row qualify itself.
    assert_eq!(
        target.account_document_meta(&other_key),
        source.account_document_meta(&other_key)
    );
    pull_reference_action_control(&target, &source, address, group).await;
    let role = target.account_group_access(group).unwrap().role;
    source.runtime_work.close();
    server.abort();
    let _ = server.await;
    source.runtime_work.drain().await;
    assert_eq!(
        role,
        AccountGroupRole::Member,
        "bootstrap must import the current signed head for the requested group"
    );
}

#[tokio::test]
async fn role_bootstrap_keeps_its_entry_snapshot_when_source_publication_advances_over_noise() {
    use crate::node::AccountDocumentKey;
    use crate::node::account_documents::sync::DocumentSyncCursor;
    use crate::node::account_sync_wire::AccountHistoryFrame;
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let first = node(&dir.path().join("first"), &account);
    let second = node(&dir.path().join("second"), &account);
    // The wanted source row precedes the unrelated signed member row in the
    // existing key order, so the held tail cannot hide a missing wanted body.
    let (source, member) = if first.user_id() < second.user_id() {
        (first, second)
    } else {
        (second, first)
    };
    let target = node(&dir.path().join("target"), &account);
    for left in [&source, &member, &target] {
        for right in [&source, &member, &target] {
            if left.user_id() != right.user_id() {
                link(left, right);
            }
        }
    }
    let mut servers = Vec::new();
    let mut addresses = Vec::new();
    for (name, node) in [("source-privacy", &source), ("member-privacy", &member)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        std::fs::create_dir_all(dir.path().join(name)).unwrap();
        configure(node, &dir.path().join(name), address.port());
        addresses.push(address);
        servers.push(inbound_server(node.clone(), listener));
    }
    for (left, right, address) in [
        (&target, &source, addresses[0]),
        (&source, &member, addresses[1]),
    ] {
        left.roster.lock().unwrap().update(
            &right.signed_announce("Snapshot bootstrap", address.port()),
            address.ip(),
            &left.user_id(),
        );
    }
    let peer = target.routing_peer(&source.user_id()).unwrap();
    target
        .pull_account_documents_page(&peer, &mut DocumentSyncCursor::control())
        .await
        .unwrap();
    let genesis = legacy_group(&source, &member);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    install(&member, &genesis);
    source.publish_account_group_access_for(group);
    member.publish_account_group_access_for(group);
    let key = AccountDocumentKey::GroupControl {
        channel: hex::encode(group.as_bytes()),
        source_device: source.user_id(),
    };
    let entry = source.account_document_meta(&key).unwrap();
    let member_peer = source.routing_peer(&member.user_id()).unwrap();
    // Newly learned, real source knowledge will legitimately grow the owner's
    // signed head; it is not claimed to be an ACK-only publication.
    source
        .pull_account_documents_page(&member_peer, &mut DocumentSyncCursor::control())
        .await
        .unwrap();
    assert_eq!(source.account_document_meta(&key).as_ref(), Some(&entry));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (arrived, held) = tokio::sync::oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let advanced = Arc::new(AtomicBool::new(false));
    *source.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let source = source.clone();
        let release = release.clone();
        let target_public = target.identity.public();
        let advanced = advanced.clone();
        move |peer, frame| {
            let source = source.clone();
            let release = release.clone();
            let arrived = arrived.clone();
            let advanced = advanced.clone();
            let target_public = target_public.clone();
            Box::pin(async move {
                if peer != target_public {
                    return;
                }
                let AccountHistoryFrame::DocumentHeader { body } = frame else {
                    return;
                };
                let meta: crate::node::AccountDocumentMeta = serde_json::from_slice(&body).unwrap();
                let AccountDocumentKey::GroupControl { source_device, .. } = meta.key else {
                    return;
                };
                if source_device == source.user_id() && !advanced.swap(true, Ordering::SeqCst) {
                    // Metadata and its matching signed body were selected before
                    // this hook; advance the live publisher after helper entry.
                    source.publish_account_group_access_for(group);
                } else if source_device != source.user_id() {
                    let signal = arrived.lock().unwrap().take();
                    if let Some(signal) = signal {
                        let _ = signal.send(());
                        let _ = release.acquire().await;
                    }
                }
            })
        }
    }));
    let pulling = {
        let source = source.clone();
        let target = target.clone();
        let address = addresses[0];
        tokio::spawn(async move {
            pull_reference_action_control(&target, &source, address, group).await;
        })
    };
    let tail_held = tokio::time::timeout(std::time::Duration::from_secs(3), held).await;
    let completed = pulling.await;
    let current = source.account_document_meta(&key).unwrap();
    let imported = target.account_document_meta(&key);
    release.close();
    *source.account_session_test_hook.lock().unwrap() = None;
    for server in servers {
        server.abort();
        let _ = server.await;
    }
    for node in [&source, &member, &target] {
        node.runtime_work.close();
        node.runtime_work.drain().await;
    }
    assert!(
        tail_held.is_ok(),
        "the newer publication must remain behind a real authenticated held tail"
    );
    assert!(advanced.load(Ordering::SeqCst));
    assert!(
        current.revision > entry.revision,
        "the real publisher must advance after helper entry"
    );
    assert_eq!(
        imported.as_ref(),
        Some(&entry),
        "the exact entry snapshot is already durable"
    );
    assert!(
        completed.is_ok(),
        "a future publication cannot move an already satisfied source snapshot barrier"
    );
}

#[test]
fn role_snapshot_floor_pins_first_live_head_and_rejects_conflicting_metadata() {
    use crate::node::AccountDocumentKey;
    let dir = tempfile::tempdir().unwrap();
    let source = node(&dir.path().join("source"), &Account::generate());
    let member = node(&dir.path().join("member"), &Account::generate());
    let genesis = legacy_group(&source, &member);
    install(&source, &genesis);
    source.publish_account_group_access_for(genesis.conversation_id);
    let writer = source.user_id();
    let key = AccountDocumentKey::GroupControl {
        channel: hex::encode(genesis.conversation_id.as_bytes()),
        source_device: writer.clone(),
    };
    let initial = source.account_document_meta(&key).unwrap();
    // This pure ordering guard never admits records. Real signed-body and
    // current-binding validation remains exercised by the Noise fixtures.
    let meta = |revision, payload: &[u8], tombstone| {
        let mut value = initial.clone();
        value.revision = revision;
        value.content_hash = crate::node::account_documents::content_hash(payload);
        value.content_len = payload.len() as u32;
        value.tombstone = tombstone;
        value
    };
    let newer = meta(2, b"later signed role", false);
    let mut diagnosis = RoleBootstrapDiagnostics::default();
    diagnosis.pin_first_source(None);
    diagnosis.pin_first_source(Some(&meta(1, b"", true)));
    assert!(diagnosis.source_floor.is_none());
    diagnosis.pin_first_source(Some(&initial));
    diagnosis.pin_first_source(Some(&newer));
    assert_eq!(diagnosis.source_floor.as_ref(), Some(&initial));
    assert!(role_metadata_reaches_snapshot(&initial, &initial));
    assert!(role_metadata_reaches_snapshot(&newer, &initial));
    assert!(!role_metadata_reaches_snapshot(
        &meta(0, b"old", false),
        &initial
    ));
    assert!(!role_metadata_reaches_snapshot(
        &meta(1, b"same revision fork", false),
        &initial
    ));
    assert!(!role_metadata_reaches_snapshot(
        &meta(2, b"", true),
        &initial
    ));
    let mut wrong_group = newer.clone();
    wrong_group.key = AccountDocumentKey::GroupControl {
        channel: hex::encode(new_channel_id().as_bytes()),
        source_device: writer,
    };
    assert!(!role_metadata_reaches_snapshot(&wrong_group, &initial));
}
