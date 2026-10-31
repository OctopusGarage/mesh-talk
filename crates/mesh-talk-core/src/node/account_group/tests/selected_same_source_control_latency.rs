//! A new explicit receipt must not inherit a pre-operation background read.
use super::*;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

pub(super) type BTrace =
    Arc<Mutex<std::collections::VecDeque<(u128, &'static str, Option<tokio::task::Id>, String)>>>;

type CriticalControlEvent = (u128, &'static str, Option<tokio::task::Id>, String);
#[derive(Clone, Debug, Default)]
struct CriticalControlTrace {
    first: Vec<CriticalControlEvent>,
    last: std::collections::VecDeque<CriticalControlEvent>,
    total: usize,
    tasks: std::collections::VecDeque<tokio::task::Id>,
    queues: Vec<(&'static str, String)>,
}
impl CriticalControlTrace {
    fn begin_budget(&mut self, clock: std::time::Instant) {
        self.first.clear();
        self.last.clear();
        self.queues.clear();
        self.total = 0;
        self.record(clock, "fixture", "B_submit_begin".into());
    }
    fn record(&mut self, clock: std::time::Instant, side: &'static str, event: String) {
        let row = (
            clock.elapsed().as_millis(),
            side,
            tokio::task::try_id(),
            event,
        );
        self.total += 1;
        // Keep the complete bounded operation interval, including the middle
        // request/Complete nonce pairs needed to distinguish stale cursors from
        // scheduler delay. The former 128 + 64 sample lost those transitions.
        if self.first.len() < 1024 {
            self.first.push(row);
        } else {
            if self.last.len() == 64 {
                self.last.pop_front();
            }
            self.last.push_back(row);
        }
    }
    fn client(
        &mut self,
        clock: std::time::Instant,
        side: &'static str,
        event: &str,
        enabled: bool,
    ) {
        if event.starts_with("control_task ") {
            if self.tasks.len() == 64 {
                self.tasks.pop_front();
            }
            self.tasks.push_back(tokio::task::id());
        }
        if !enabled {
            return;
        }
        if let Some(body) = event.strip_prefix("control_request_body=") {
            let request = crate::node::account_documents::wire::DocumentRequest::decode(
                &hex::decode(body).unwrap(),
            )
            .unwrap();
            self.record(clock, side, Self::request(&request));
            return;
        }
        if event.starts_with("control_queue ") {
            if let Some((_, previous)) = self.queues.iter_mut().find(|(peer, _)| *peer == side) {
                if previous == event {
                    return;
                }
                *previous = event.into();
            } else {
                assert!(self.queues.len() < 8, "fixture identities are bounded");
                self.queues.push((side, event.into()));
            }
        }
        let control_task = tokio::task::try_id().is_some_and(|id| self.tasks.contains(&id));
        if event.starts_with("control_")
            || event.starts_with("wake_")
            || event.starts_with("operation_local_published")
            || control_task
                && (event.starts_with("page_")
                    || event.starts_with("negotiation_")
                    || event.starts_with("dial_"))
        {
            self.record(clock, side, event.chars().take(512).collect());
        }
    }
    fn request(request: &crate::node::account_documents::wire::DocumentRequest) -> String {
        format!(
            "Control_request version={} nonce={} after={:?} probe_after={:?} roles={} operations={}",
            request.version,
            request
                .operation
                .as_ref()
                .map_or_else(|| "none".into(), |probe| hex::encode(probe.nonce)),
            request.after,
            request
                .operation
                .as_ref()
                .and_then(|probe| probe.after.as_ref()),
            hex::encode(request.known_digest),
            request
                .operation
                .as_ref()
                .map_or_else(|| "none".into(), |probe| hex::encode(probe.known_digest))
        )
    }
    fn frame(
        &mut self,
        clock: std::time::Instant,
        side: &'static str,
        frame: &AccountHistoryFrame,
    ) {
        match frame {
            AccountHistoryFrame::DocumentsRequest { body } => {
                if let Some(request) =
                    crate::node::account_documents::wire::DocumentRequest::decode(body)
                        .filter(|request| request.domain == AccountDocumentDomain::GroupControl)
                {
                    self.record(clock, side, Self::request(&request));
                }
            }
            AccountHistoryFrame::DocumentsComplete { body } => {
                let complete: serde_json::Value = serde_json::from_slice(body).unwrap();
                if complete["domain"] == "group_control" {
                    let nonce =
                        serde_json::from_value::<[u8; 32]>(complete["operation"]["nonce"].clone())
                            .ok()
                            .map(hex::encode);
                    self.record(
                        clock,
                        side,
                        format!(
                            "Control_complete nonce={nonce:?} next={} enabled={}",
                            complete["next"], complete["enabled"]
                        ),
                    );
                }
            }
            _ => {}
        }
    }
    fn snapshot(
        &self,
    ) -> (
        usize,
        Vec<CriticalControlEvent>,
        std::collections::VecDeque<CriticalControlEvent>,
    ) {
        (self.total, self.first.clone(), self.last.clone())
    }
}

#[derive(Clone, Debug)]
pub(in crate::node) struct ControlTestPage {
    pub task: tokio::task::Id,
    pub dispatched_at: std::time::Instant,
    pub origin: String,
    pub request: crate::node::account_documents::wire::DocumentRequest,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::node) struct ControlTestDispatch {
    pub paired: bool,
    pub protected: bool,
    pub resumed: bool,
    pub replacement: bool,
    pub hint: bool,
}
impl ControlTestDispatch {
    pub fn natural_origin(self) -> bool {
        !self.resumed && !self.replacement
    }
    fn unpaired_background(self) -> bool {
        !self.paired && !self.protected && self.natural_origin()
    }
    fn turn_origin(self) -> String {
        format!(
            "control_turn_origin resumed={} replacement={}",
            self.resumed, self.replacement
        )
    }
}

#[derive(Default)]
struct ColdControlScope {
    settled: bool,
    paired: bool,
}
impl ColdControlScope {
    fn observe(&mut self, event: &str, corpus_present: bool) {
        if event.starts_with("control_task ") {
            self.settled = false;
            // The parent can cancel before the child's first trace. Only an
            // explicit retirement clears the announced comparison debt.
            self.paired |= event.starts_with("control_task paired=true ");
        } else if event.starts_with("control_cancel ") && event.ends_with("reason=same_source") {
            // The child can finish before abort takes effect. Its late scope
            // completion cannot pay the pair the parent is about to create.
            self.settled = false;
            self.paired = true;
        } else if event == "document_scope_settled scope=Control" {
            self.settled = corpus_present;
        } else if event == "control_pair_retired" {
            self.paired = false;
        }
    }
    fn ready(&self) -> bool {
        self.settled && !self.paired
    }
}

#[test]
fn initial_background_hold_rejects_a_paid_but_live_control_pair() {
    let natural = ControlTestDispatch {
        paired: false,
        protected: false,
        resumed: false,
        replacement: false,
        hint: false,
    };
    assert!(natural.unpaired_background());
    let paid_pair = ControlTestDispatch {
        paired: true,
        ..natural
    };
    assert!(paid_pair.natural_origin() && !paid_pair.protected);
    assert!(
        !paid_pair.unpaired_background(),
        "an existing pair cannot establish the fresh same-source cancellation boundary"
    );

    let mut scope = ColdControlScope::default();
    scope.observe("control_task paired=true protected=true", false);
    scope.observe("document_scope_settled scope=Control", true);
    assert!(
        !scope.ready(),
        "a completed prefix cannot hide an unpaid original comparison"
    );
    scope.observe("control_pair_retired", false);
    assert!(scope.ready());
    scope.observe("control_task paired=false protected=false", false);
    assert!(
        !scope.ready(),
        "an earlier completed scope is not current readiness"
    );
    scope.observe("document_scope_settled scope=Control", false);
    assert!(!scope.ready(), "the exact corpus must already be present");
    scope.observe("control_cancel task=Id(1) reason=same_source", false);
    scope.observe("document_scope_settled scope=Control", true);
    assert!(
        !scope.ready(),
        "a late child completion cannot settle an announced same-source cancellation"
    );
    scope.observe("control_task paired=false protected=false", false);
    scope.observe("document_scope_settled scope=Control", true);
    assert!(
        !scope.ready(),
        "a late first child poll cannot erase its parent's announced comparison debt"
    );
    scope.observe("control_task paired=true protected=true", false);
    scope.observe("document_scope_settled scope=Control", true);
    assert!(!scope.ready());
    scope.observe("control_pair_retired", false);
    assert!(scope.ready());
}
impl ControlTestPage {
    pub fn dispatch(&self) -> Option<ControlTestDispatch> {
        let flag = |name: &str| {
            let prefix = format!("{name}=");
            let mut values = self
                .origin
                .split_ascii_whitespace()
                .filter_map(|token| token.strip_prefix(&prefix));
            let value = match values.next()? {
                "true" => true,
                "false" => false,
                _ => return None,
            };
            values.next().is_none().then_some(value)
        };
        Some(ControlTestDispatch {
            paired: flag("paired")?,
            protected: flag("protected")?,
            resumed: flag("resumed")?,
            replacement: flag("replacement")?,
            hint: flag("hint")?,
        })
    }
    pub fn contains_demand(&self, id: AccountGroupOperationId) -> bool {
        self.origin.contains(&format!("{id:?}"))
    }
}
#[derive(Clone, Debug)]
struct ControlTestOrigin {
    task: tokio::task::Id,
    event: String,
    dispatched_at: std::time::Instant,
    lifetime: Option<std::sync::Weak<()>>,
}
#[derive(Default, Debug)]
pub(in crate::node) struct ControlTestPages {
    origins: std::collections::VecDeque<ControlTestOrigin>,
    pages: std::collections::VecDeque<ControlTestPage>,
    active: std::collections::HashSet<tokio::task::Id>,
}
impl ControlTestPages {
    pub(in crate::node) fn lifetime(&self, task: tokio::task::Id) -> Option<std::sync::Weak<()>> {
        self.origins
            .iter()
            .rev()
            .find(|origin| origin.task == task)
            .and_then(|origin| origin.lifetime.clone())
    }

    pub fn observe(&mut self, event: &str) {
        if event.starts_with("control_task ") {
            if self.origins.len() == 64
                && let Some(origin) = self.origins.pop_front()
            {
                self.active.remove(&origin.task);
            }
            self.active.insert(tokio::task::id());
            self.origins.push_back(ControlTestOrigin {
                task: tokio::task::id(),
                event: event.into(),
                dispatched_at: std::time::Instant::now(),
                lifetime: super::super::super::account_session::observed_control_task_lifetime(),
            });
        }
        if event.starts_with("page_end ") {
            self.active.remove(&tokio::task::id());
        }
        if event.starts_with("control_cancel task=") {
            let ended: Vec<_> = self
                .active
                .iter()
                .copied()
                .filter(|task| event.starts_with(&format!("control_cancel task={task:?} ")))
                .collect();
            for task in ended {
                self.active.remove(&task);
            }
        }
        if let Some(body) = event.strip_prefix("control_request_body=") {
            let request = crate::node::account_documents::wire::DocumentRequest::decode(
                &hex::decode(body).unwrap(),
            )
            .unwrap();
            let task = tokio::task::id();
            let Some(origin) = self
                .origins
                .iter()
                .rev()
                .find(|origin| origin.task == task)
                .cloned()
            else {
                // Hooks can attach after a task starts, and this bounded ledger
                // can retire an older origin. Neither is dispatch evidence.
                return;
            };
            if self.pages.len() == 64 {
                self.pages.pop_front();
            }
            self.pages.push_back(ControlTestPage {
                task,
                dispatched_at: origin.dispatched_at,
                origin: origin.event,
                request,
            });
        }
    }
    pub fn is_active(&self, task: tokio::task::Id) -> bool {
        self.active.contains(&task)
            && self
                .origins
                .iter()
                .rev()
                .find(|origin| origin.task == task)
                .and_then(|origin| origin.lifetime.as_ref())
                .is_some_and(|lifetime| lifetime.upgrade().is_some())
    }
    pub fn live_request(
        &self,
        request: &crate::node::account_documents::wire::DocumentRequest,
    ) -> Option<ControlTestPage> {
        let page = self.request(request)?;
        self.is_active(page.task).then_some(page)
    }
    pub fn request(
        &self,
        request: &crate::node::account_documents::wire::DocumentRequest,
    ) -> Option<ControlTestPage> {
        self.nonce(request.operation.as_ref()?.nonce)
    }
    pub fn nonce(&self, nonce: [u8; 32]) -> Option<ControlTestPage> {
        self.pages
            .iter()
            .rev()
            .find(|page| {
                page.request
                    .operation
                    .as_ref()
                    .is_some_and(|probe| probe.nonce == nonce)
            })
            .cloned()
    }
    pub fn task(&self, task: tokio::task::Id) -> Option<ControlTestPage> {
        self.pages
            .iter()
            .rev()
            .find(|page| page.task == task)
            .cloned()
    }
    pub fn summary(&self) -> Vec<String> {
        self.pages
            .iter()
            .rev()
            .take(16)
            .map(|page| {
                format!(
                    "task={:?} {} nonce={} continuation={}",
                    page.task,
                    page.origin.split(" requests=").next().unwrap(),
                    page.request
                        .operation
                        .as_ref()
                        .map_or_else(|| "none".into(), |probe| hex::encode(&probe.nonce[..4])),
                    page.request.after.is_some()
                        || page
                            .request
                            .operation
                            .as_ref()
                            .is_some_and(|probe| probe.after.is_some()),
                )
            })
            .collect()
    }
}

#[tokio::test]
async fn control_page_observer_rejects_retired_children_and_preserves_exact_dispatch() {
    use crate::node::account_documents::wire::{DocumentRequest, OperationProbe};
    let directory = tempfile::tempdir().unwrap();
    let source = node(directory.path(), &Account::generate());
    for outcome in ["return", "abort", "panic"] {
        let pages = Arc::new(Mutex::new(ControlTestPages::default()));
        *source.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let pages = pages.clone();
            move |_, event| pages.lock().unwrap().observe(&event)
        }));
        let request = DocumentRequest {
            version: 7,
            account_id: source.account_id(),
            domain: AccountDocumentDomain::GroupControl,
            after: None,
            known_digest: [0; 32],
            operation: Some(OperationProbe {
                version: 1,
                nonce: [1; 32],
                after: None,
                known_digest: [0; 32],
                asset: None,
                legacy_leave: true,
                personal: true,
            }),
        };
        assert!(
            DocumentRequest::decode(&serde_json::to_vec(&request).unwrap()).is_some(),
            "the observer fixture must use an owner-valid version-7 request"
        );
        let (arrived, observed) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let child_source = source.clone();
        let child_request = request.clone();
        let child = tokio::spawn(async move {
            let lifetime = Arc::new(());
            child_source.trace_control_task(&child_source.identity.public(),
                "control_task paired=true protected=true requests={} resumed=true replacement=false hint=false".into(), Arc::downgrade(&lifetime));
            child_source.trace_account_client(&child_source.identity.public(), || {
                format!(
                    "control_request_body={}",
                    hex::encode(serde_json::to_vec(&child_request).unwrap())
                )
            });
            arrived.send(()).unwrap();
            released.await.unwrap();
            if outcome == "panic" {
                panic!("deliberate child panic");
            }
            child_source.trace_account_client(&child_source.identity.public(), || {
                "page_end outcome=ok".into()
            });
            drop(lifetime);
        });
        observed.await.unwrap();
        let original = pages.lock().unwrap().live_request(&request).unwrap();
        assert_eq!(original.task, child.id());
        let dispatch = original.dispatch().unwrap();
        assert!(dispatch.protected && dispatch.resumed && !dispatch.replacement && !dispatch.hint);
        let newer_source = source.clone();
        let mut newer_request = request.clone();
        newer_request.operation.as_mut().unwrap().nonce = [2; 32];
        tokio::spawn(async move {
            let lifetime = Arc::new(());
            newer_source.trace_control_task(&newer_source.identity.public(),
                "control_task paired=false protected=false requests={} resumed=false replacement=false hint=true".into(), Arc::downgrade(&lifetime));
            newer_source.trace_account_client(&newer_source.identity.public(), || format!("control_request_body={}", hex::encode(serde_json::to_vec(&newer_request).unwrap())));
        }).await.unwrap();
        let exact = pages.lock().unwrap().live_request(&request).unwrap();
        assert_eq!(exact.task, original.task);
        assert_eq!(exact.dispatch(), Some(dispatch));
        if outcome == "abort" {
            child.abort();
        } else {
            release.send(()).unwrap();
        }
        let result = child.await;
        match outcome {
            "return" => result.unwrap(),
            "abort" => assert!(result.unwrap_err().is_cancelled()),
            "panic" => assert!(result.unwrap_err().is_panic()),
            _ => unreachable!(),
        }
        assert!(
            pages.lock().unwrap().request(&request).is_some(),
            "historical continuation evidence is retained"
        );
        assert!(
            pages.lock().unwrap().live_request(&request).is_none(),
            "retired exact nonce cannot establish a held read"
        );
    }
    *source.account_client_test_hook.lock().unwrap() = None;
}

#[test]
fn control_page_dispatch_flags_are_complete_and_orthogonal() {
    let request = crate::node::account_documents::wire::DocumentRequest {
        version: 5,
        account_id: Account::generate().account_id(),
        domain: AccountDocumentDomain::GroupControl,
        after: None,
        known_digest: [0; 32],
        operation: None,
    };
    // Missing or duplicated fields cannot impersonate a natural false/false origin.
    for origin in [
        "control_task paired=true protected=true requests={} hint=true",
        "control_task paired=true protected=true requests={} resumed=false replacement=false hint=true resumed=false",
    ] {
        let parsed = |origin: &str| {
            // A real task ID is unnecessary for flag parsing; use a short scoped runtime.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let origin = origin.to_owned();
            let request = request.clone();
            runtime.block_on(async {
                tokio::spawn(async move {
                    ControlTestPage {
                        task: tokio::task::id(),
                        dispatched_at: std::time::Instant::now(),
                        origin,
                        request,
                    }
                    .dispatch()
                })
                .await
                .unwrap()
            })
        };
        assert!(parsed(origin).is_none());
        let dispatch = parsed("control_task paired=true protected=true requests={} resumed=false replacement=false hint=true").unwrap();
        assert!(
            dispatch.natural_origin() && dispatch.protected && dispatch.hint,
            "natural origin does not mean unprotected"
        );
    }
}

#[tokio::test]
async fn control_page_observer_requires_an_observed_dispatch_without_poisoning_late_hooks() {
    let pages = Arc::new(Mutex::new(ControlTestPages::default()));
    let observed = pages.clone();
    let task = tokio::spawn(async move {
        let request = crate::node::account_documents::wire::DocumentRequest {
            version: 5,
            account_id: Account::generate().account_id(),
            domain: crate::node::AccountDocumentDomain::GroupControl,
            after: None,
            known_digest: [0; 32],
            operation: None,
        };
        let body = format!(
            "control_request_body={}",
            hex::encode(serde_json::to_vec(&request).unwrap())
        );
        observed.lock().unwrap().observe(&body);
        assert!(observed.lock().unwrap().pages.is_empty());

        let origin = "control_task paired=false protected=false requests=[]";
        observed.lock().unwrap().observe(origin);
        observed.lock().unwrap().observe(&body);
        let page = observed.lock().unwrap().task(tokio::task::id()).unwrap();
        assert_eq!(page.origin, origin);
        assert!(
            !observed.lock().unwrap().is_active(page.task),
            "a dispatch without a child lifetime fails closed"
        );
        assert!(
            observed.lock().unwrap().live_request(&request).is_none(),
            "a request without an operation nonce fails closed"
        );
        assert_eq!(
            serde_json::to_value(&page.request).unwrap(),
            serde_json::to_value(&request).unwrap()
        );
    });
    task.await.unwrap();
    assert_eq!(pages.lock().unwrap().pages.len(), 1);
}

pub(super) fn record_b_trace(
    trace: &BTrace,
    started: std::time::Instant,
    side: &'static str,
    phase: String,
) {
    // Exact bodies belong only to the bounded nonce ledger; keep the shared
    // human-readable ring available for page/payment/cancellation transitions.
    if phase.starts_with("control_request_body=") {
        return;
    }
    let mut rows = trace.lock().unwrap();
    if rows.len() == 384 {
        rows.pop_front();
    }
    rows.push_back((
        started.elapsed().as_millis(),
        side,
        tokio::task::try_id(),
        phase,
    ));
}
pub(super) fn result_snapshot(
    node: &Node,
    group: ConversationId,
    id: Option<AccountGroupOperationId>,
) -> String {
    let key = crate::node::AccountDocumentKey::GroupOperation {
        channel: hex::encode(group.as_bytes()),
        source_device: node.user_id(),
    };
    let meta = node.account_document_meta(&key);
    let committed = node
        .account_document(&key)
        .ok()
        .flatten()
        .and_then(|document| {
            crate::node::account_group::operations::wire::OperationFrame::decode(
                document.payload(),
                &node.account.public().ed25519_pub,
            )
        })
        .is_some_and(|frame| {
            frame.group == group
                && frame.proof.public() == node.identity.public()
                && frame.projection.results.iter().any(|row| {
                    Some(row.reference.id) == id
                        && row.status.phase == AccountGroupOperationPhase::Committed
                })
        });
    format!(
        "operation_revision={:?} matching_terminal={committed}",
        meta.map(|meta| meta.revision)
    )
}
pub(super) fn record_control_frame(
    trace: &BTrace,
    clock: std::time::Instant,
    side: &'static str,
    frame: &AccountHistoryFrame,
    group: ConversationId,
    writer: &str,
) {
    let record = match frame {
        AccountHistoryFrame::DocumentsRequest { body } => {
            crate::node::account_documents::wire::DocumentRequest::decode(body)
                .filter(|request| request.domain == AccountDocumentDomain::GroupControl)
                .map(|request| {
                    format!(
                        "Control_request after={:?} probe_after={:?} known_zero={}",
                        request.after,
                        request
                            .operation
                            .as_ref()
                            .and_then(|probe| probe.after.as_ref()),
                        request.known_digest == [0; 32]
                    )
                })
        }
        AccountHistoryFrame::DocumentHeader { body } => serde_json::from_slice::<
            crate::node::AccountDocumentMeta,
        >(body)
        .ok()
        .and_then(|meta| {
            let crate::node::AccountDocumentKey::GroupOperation {
                channel,
                source_device,
            } = &meta.key
            else {
                return None;
            };
            (*channel == hex::encode(group.as_bytes()) && source_device == writer).then(|| {
                format!(
                    "matching_operation_header revision={} hash={}",
                    meta.revision,
                    hex::encode(&meta.content_hash[..4])
                )
            })
        }),
        AccountHistoryFrame::DocumentsComplete { body } => {
            serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .filter(|complete| complete["domain"] == "group_control")
                .map(|complete| {
                    format!(
                        "Control_complete next={} enabled={}",
                        complete["next"], complete["enabled"]
                    )
                })
        }
        _ => None,
    };
    if let Some(record) = record {
        record_b_trace(trace, clock, side, record);
    }
}
fn b_operation_snapshot(
    node: &Node,
    peer: &Node,
    group: crate::eventlog::ConversationId,
    id: AccountGroupOperationId,
) -> serde_json::Value {
    use crate::node::account_documents::AccountDocumentKey;
    // Lifecycle list reads never advance the pending-query nonce or Notify.
    let status = node
        .list_account_group_operations(
            &node.account_id(),
            AccountGroupOperationQuery {
                group: Some(group),
                pending_only: false,
                after: None,
                limit: 64,
            },
        )
        .unwrap()
        .into_iter()
        .find(|status| status.id == id);
    let metadata = [node.user_id(), peer.user_id()].into_iter().map(|writer| {
        let key = AccountDocumentKey::GroupOperation { channel: hex::encode(group.as_bytes()), source_device: writer.clone() };
        let meta = node.account_document_meta(&key);
        let frame = node.account_document(&key).unwrap().and_then(|document| crate::node::account_group::operations::wire::OperationFrame::decode(document.payload(), &node.account.public().ed25519_pub));
        serde_json::json!({"writer_local": writer==node.user_id(), "revision":meta.as_ref().map(|meta|meta.revision),
            "hash":meta.as_ref().map(|meta|hex::encode(&meta.content_hash[..4])),
            "request":frame.as_ref().is_some_and(|frame|frame.projection.requests.iter().any(|row|row.reference.id==id)),
            "result":frame.as_ref().and_then(|frame|frame.projection.results.iter().find(|row|row.reference.id==id)).map(|row|row.status.phase.clone())})
    }).collect::<Vec<_>>();
    let ready = node
        .group_operation_versions
        .lock()
        .unwrap()
        .get(&peer.user_id())
        .is_some_and(|support| {
            support.public == peer.identity.public()
                && support.kinds & AccountGroupOperationKind::Rename.capability_bit() != 0
        });
    serde_json::json!({"phase": status.as_ref().map(|status|status.phase.clone()), "execution_devices":status.as_ref().map(|status|status.executions.iter().map(|row|row.device_id.clone()).collect::<Vec<_>>()), "exact_capability":ready,
        "selected_peer":node.pending_account_operation_sources().contains(&(peer.identity.public().ed25519_pub,peer.identity.public().x25519_pub)),"capacity":format!("{:?}",node.sessions.test_capacity_snapshot()),"metadata":metadata})
}

struct ReleaseReadOnDrop(Arc<Semaphore>);
impl Drop for ReleaseReadOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_source_receipt_overtakes_its_own_preoperation_background_control_read() {
    run_same_source_receipt(SameSourceScenario::Fresh).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_source_lower_key_receipt_preserves_both_partial_control_comparisons() {
    run_same_source_receipt(SameSourceScenario::Partial).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_selected_executor_overtakes_a_completed_operations_paired_background_page() {
    run_same_source_receipt(SameSourceScenario::OtherSelected).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incoming_operation_wake_runs_after_paid_pair_pages_before_unrelated_continuation() {
    run_same_source_receipt(SameSourceScenario::PaidBoundary).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_source_cold_preparation_replaces_a_retired_partial_candidate() {
    run_same_source_receipt(SameSourceScenario::InterruptedPreparation).await;
}

#[derive(Clone, Copy)]
enum SameSourceScenario {
    Fresh,
    Partial,
    OtherSelected,
    PaidBoundary,
    InterruptedPreparation,
}

type PostHandoffPage = (u128, tokio::task::Id, Option<[u8; 32]>, bool, String);

type PaidCheckpoint = (
    u8,
    Option<crate::node::AccountDocumentKey>,
    [u8; 32],
    Option<[u8; 32]>,
);
#[derive(Clone, Debug, Default)]
struct PaidBoundaryEvidence {
    stages: Vec<(u128, String)>,
    active_page: Option<tokio::task::Id>,
    natural_page: Option<tokio::task::Id>,
    prefix_page: Option<tokio::task::Id>,
    resume_page: Option<tokio::task::Id>,
    original: Option<PaidCheckpoint>,
    resumed: Option<PaidCheckpoint>,
    prefix_paid: bool,
    resume_paid: bool,
    source_aborts: usize,
    pair_retirements: usize,
    requester_wake: bool,
    ready_requester_hint: Option<String>,
    requester_control: Option<(u128, String)>,
    requester_page: Option<tokio::task::Id>,
    requester_nonce: Option<[u8; 32]>,
    post_handoff_pages: Vec<PostHandoffPage>,
    continuation_trapped: bool,
    trapped_protected: Option<bool>,
    trapped_origin: Option<String>,
    trapped_after_requester: bool,
    trapped_page: Option<tokio::task::Id>,
    trapped_nonce: Option<[u8; 32]>,
    invalid: Option<String>,
}
impl PaidBoundaryEvidence {
    fn record(&mut self, clock: std::time::Instant, event: impl Into<String>) {
        // Critical transitions only. Routine queue/page chatter stays in the
        // separately bounded shared ring and cannot evict payment evidence.
        if self.stages.len() < 24 {
            self.stages
                .push((clock.elapsed().as_millis(), event.into()));
        }
    }
}

struct PaidPairBoundary {
    original: Arc<Semaphore>,
    prefix: Arc<Semaphore>,
    resume: Arc<Semaphore>,
    continuation: Arc<Semaphore>,
    evidence: Arc<Mutex<PaidBoundaryEvidence>>,
    pages: Arc<Mutex<ControlTestPages>>,
}
impl Drop for PaidPairBoundary {
    fn drop(&mut self) {
        self.original.close();
        self.prefix.close();
        self.resume.close();
        self.continuation.close();
    }
}

async fn send_boundary_wake(source: &Arc<Node>, target: &Arc<Node>) -> ([u8; 16], u64) {
    use crate::node::account_sync_wire::AccountSyncVersion;
    let peer = source.routing_peer(&target.user_id()).unwrap();
    let (epoch, revision) = source.account_wake_token_for_test();
    let mut channel = source
        .begin_account_exchange(
            &peer,
            AccountSyncVersion::V2,
            AccountHistoryFrame::Wake {
                account_id: source.account_id(),
                epoch,
                revision,
            },
        )
        .await
        .unwrap();
    let bytes = channel.recv().await.unwrap();
    assert!(matches!(AccountHistoryFrame::decode_versioned(&bytes),
        Some((AccountSyncVersion::V2, AccountHistoryFrame::WakeAck { epoch: received, revision: received_revision }))
        if received == epoch && received_revision == revision));
    channel.recycle(source);
    (epoch, revision)
}

/// Create an actual pair through a held authenticated natural tail and a newer
/// source Wake. Pay its fresh prefix, then hold its exact original resume.
async fn hold_original_pair_resume(
    source: &Arc<Node>,
    executor: &Arc<Node>,
    requester: &Arc<Node>,
    partial_keys: &Arc<Mutex<Vec<crate::node::AccountDocumentKey>>>,
    trace: &BTrace,
    clock: std::time::Instant,
) -> PaidPairBoundary {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    super::cold_account_setup(async {
        while !executor.device_sync_status().iter().any(|status| {
            status.device_id == source.user_id()
                && status.document_phase == crate::node::SyncPhase::UpToDate
        }) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the executor's unrelated scope must settle before the deterministic fault");
    let pages = Arc::new(Mutex::new(ControlTestPages::default()));
    let gates = PaidPairBoundary {
        original: Arc::new(Semaphore::new(0)),
        prefix: Arc::new(Semaphore::new(0)),
        resume: Arc::new(Semaphore::new(0)),
        continuation: Arc::new(Semaphore::new(0)),
        evidence: Arc::new(Mutex::new(PaidBoundaryEvidence::default())),
        pages: pages.clone(),
    };
    let arrived = Arc::new(Semaphore::new(0));
    let prefix_arrived = Arc::new(Semaphore::new(0));
    let resume_arrived = Arc::new(Semaphore::new(0));
    let phase = Arc::new(AtomicUsize::new(0));
    let aborted = Arc::new(AtomicBool::new(false));
    let seed_pending = Arc::new(AtomicBool::new(true));
    let previous_client = executor.account_client_test_hook.lock().unwrap().clone();
    *executor.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let exact = source.identity.public();
        let requester = requester.identity.public();
        let evidence = gates.evidence.clone();
        let aborted = aborted.clone();
        let pages = pages.clone();
        let trace = trace.clone();
        let source = source.clone();
        let partial_keys = partial_keys.clone();
        let seed_pending = seed_pending.clone();
        move |peer, event| {
            if let Some(prior) = &previous_client {
                prior(peer.clone(), event.clone());
            }
            if peer == requester {
                let mut data = evidence.lock().unwrap();
                if data.requester_wake
                    && event.starts_with("control_queue")
                    && event.contains("hint=true ready=true")
                    && data.ready_requester_hint.is_none()
                {
                    data.ready_requester_hint = Some(event.clone());
                    data.record(clock, "requester_hint_ready");
                }
                if data.requester_wake
                    && data.requester_nonce.is_none()
                    && let Some(body) = event.strip_prefix("control_request_body=")
                {
                    data.requester_nonce =
                        crate::node::account_documents::wire::DocumentRequest::decode(
                            &hex::decode(body).unwrap(),
                        )
                        .and_then(|request| request.operation.map(|probe| probe.nonce));
                }
                if data.requester_wake
                    && event == "negotiation_begin domain=Some(GroupControl)"
                    && data.requester_control.is_none()
                {
                    data.requester_control = Some((clock.elapsed().as_millis(), event.clone()));
                    data.requester_page = tokio::task::try_id();
                    data.record(clock, "requester_control_started");
                }
            }
            if peer == exact {
                pages.lock().unwrap().observe(&event);
                // A paid live pair is also unprotected. Seed the new row only
                // at a genuinely unpaired dispatch, before its request snapshot.
                if event.starts_with("control_task paired=false protected=false ")
                    && seed_pending.swap(false, Ordering::AcqRel)
                {
                    partial_keys.lock().unwrap().push(
                        crate::node::account_documents::sync::seed_legacy_control_page_for_test(
                            &source,
                            crate::eventlog::ConversationId::new([67; 32]),
                        ),
                    );
                }
                {
                    let mut data = evidence.lock().unwrap();
                    if event == "negotiation_begin domain=Some(GroupControl)" {
                        data.active_page = tokio::task::try_id();
                    }
                    if data.natural_page.is_some_and(|task| {
                        event == format!("control_cancel task={task:?} reason=same_source")
                    }) {
                        data.source_aborts += 1;
                        data.record(clock, "same_source_owned_abort");
                        aborted.store(true, Ordering::Release);
                    } else if event.starts_with("control_cancel ") {
                        data.record(clock, event.clone());
                    }
                    if event == "control_pair_retired" {
                        data.pair_retirements += 1;
                        data.record(clock, "pair_retired");
                    }
                    if event.starts_with("page_end") && tokio::task::try_id() == data.prefix_page {
                        data.prefix_paid =
                            event.contains("outcome=ok") && event.contains("after=true");
                        data.record(clock, format!("prefix_completed {event}"));
                    }
                    if event.starts_with("page_end") && tokio::task::try_id() == data.resume_page {
                        data.resume_paid =
                            event.contains("outcome=ok") && event.contains("after=false");
                        data.record(clock, format!("original_resume_completed {event}"));
                    }
                }
                if (event.starts_with("control_") && !event.starts_with("control_request_body="))
                    || event.starts_with("page_end")
                {
                    record_b_trace(&trace, clock, "executor_to_unrelated_A", event);
                }
            }
        }
    }));
    let previous_server = source.account_session_test_hook.lock().unwrap().clone();
    *source.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let exact = executor.identity.public();
        let aborted = aborted.clone();
        let trace = trace.clone();
        let phase = phase.clone();
        let original = gates.original.clone();
        let prefix = gates.prefix.clone();
        let resume = gates.resume.clone();
        let continuation = gates.continuation.clone();
        let arrived = arrived.clone();
        let prefix_arrived = prefix_arrived.clone();
        let resume_arrived = resume_arrived.clone();
        let checkpoint = Arc::new(Mutex::new(None));
        let evidence = gates.evidence.clone();
        let pages = pages.clone();
        move |peer, frame| {
            let prior = previous_server.clone();
            let exact = exact.clone();
            let aborted = aborted.clone();
            let trace = trace.clone();
            let phase = phase.clone();
            let original = original.clone();
            let prefix = prefix.clone();
            let resume = resume.clone();
            let continuation = continuation.clone();
            let arrived = arrived.clone();
            let prefix_arrived = prefix_arrived.clone();
            let resume_arrived = resume_arrived.clone();
            let checkpoint = checkpoint.clone();
            let evidence = evidence.clone();
            let pages = pages.clone();
            Box::pin(async move {
                if peer != exact {
                    if let Some(prior) = prior {
                        prior(peer, frame).await;
                    }
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
                let Some(page) = pages.lock().unwrap().live_request(&request) else {
                    return;
                };
                let dispatch = page
                    .dispatch()
                    .expect("the exact child carries complete dispatch flags");
                let after = request
                    .operation
                    .as_ref()
                    .and_then(|probe| probe.after.as_ref())
                    .map(|after| {
                        if after.legacy_leave {
                            crate::node::AccountDocumentKey::GroupLegacyOperation {
                                channel: after.channel.clone(),
                                source_device: after.source_device.clone(),
                            }
                        } else {
                            crate::node::AccountDocumentKey::GroupOperation {
                                channel: after.channel.clone(),
                                source_device: after.source_device.clone(),
                            }
                        }
                    })
                    .or(request.after.clone());
                let stamp = (
                    request.version,
                    after.clone(),
                    request.known_digest,
                    request.operation.as_ref().map(|probe| probe.known_digest),
                );
                match phase.load(Ordering::Acquire) {
                    0 if matches!(
                        after,
                        Some(crate::node::AccountDocumentKey::GroupLegacyOperation { .. })
                    ) =>
                    {
                        if !dispatch.unpaired_background() {
                            return;
                        }
                        assert!(
                            page.origin.contains("protected=false"),
                            "the first tail must be an actual natural comparison"
                        );
                        *checkpoint.lock().unwrap() = Some(stamp.clone());
                        {
                            let mut data = evidence.lock().unwrap();
                            data.original = Some(stamp);
                            data.natural_page = Some(page.task);
                            // Cold comparisons can retire before this natural
                            // read. Count only the newly established pair.
                            data.source_aborts = 0;
                            data.pair_retirements = 0;
                            data.record(clock, "natural_tail_held");
                        }
                        phase.store(1, Ordering::Release);
                        record_b_trace(
                            &trace,
                            clock,
                            "fixture",
                            "executor_natural_tail_held".into(),
                        );
                        arrived.add_permits(1);
                        let _ = original.acquire().await;
                    }
                    1 if after.is_none() => {
                        assert!(
                            aborted.load(Ordering::Acquire),
                            "a genuine newer Wake must preempt the exact unpaired natural task: page={page:?}; evidence={:?}",
                            evidence.lock().unwrap()
                        );
                        assert!(
                            page.origin
                                .starts_with("control_task paired=true protected=true "),
                            "{page:?}"
                        );
                        {
                            let mut data = evidence.lock().unwrap();
                            data.prefix_page = Some(page.task);
                            data.record(clock, "protected_prefix_held");
                        }
                        phase.store(2, Ordering::Release);
                        record_b_trace(
                            &trace,
                            clock,
                            "fixture",
                            "executor_protected_prefix_held".into(),
                        );
                        prefix_arrived.add_permits(1);
                        let _ = prefix.acquire().await;
                    }
                    2 => {
                        assert!(page.origin.contains("protected=true"));
                        assert_eq!(
                            checkpoint.lock().unwrap().as_ref(),
                            Some(&stamp),
                            "the original resume must preserve exact scope, continuation and both digests"
                        );
                        {
                            let mut data = evidence.lock().unwrap();
                            data.resumed = Some(stamp);
                            data.resume_page = Some(page.task);
                            data.record(clock, "exact_original_resume_held");
                        }
                        phase.store(3, Ordering::Release);
                        record_b_trace(
                            &trace,
                            clock,
                            "fixture",
                            "executor_original_resume_held".into(),
                        );
                        resume_arrived.add_permits(1);
                        let _ = resume.acquire().await;
                        record_b_trace(
                            &trace,
                            clock,
                            "fixture",
                            "executor_original_resume_released".into(),
                        );
                    }
                    3 => {
                        {
                            let mut data = evidence.lock().unwrap();
                            // The fault ends at the genuine requester handoff.
                            // Later owned replacements/resumes may be required
                            // to finish that same request and must run normally.
                            if data.requester_control.is_some() {
                                if data.post_handoff_pages.len() < 8 {
                                    let origin = dispatch.turn_origin();
                                    data.post_handoff_pages.push((
                                        clock.elapsed().as_millis(),
                                        page.task,
                                        request.operation.as_ref().map(|probe| probe.nonce),
                                        page.origin.contains("protected=true"),
                                        origin,
                                    ));
                                    data.record(clock, "post_handoff_continuation_allowed");
                                }
                                return;
                            }
                            if !data.prefix_paid
                                || !data.resume_paid
                                || !data.requester_wake
                                || data.pair_retirements != 0
                            {
                                data.invalid = Some(format!(
                                    "continuation did not retain the same fully-paid pair: protected={} origin={}",
                                    page.origin.contains("protected=true"),
                                    dispatch.turn_origin()
                                ));
                                data.record(clock, "invalid_boundary_not_trapped");
                                return;
                            }
                            data.continuation_trapped = true;
                            data.trapped_protected = Some(page.origin.contains("protected=true"));
                            data.trapped_origin = Some(dispatch.turn_origin());
                            data.trapped_after_requester = data.requester_control.is_some();
                            data.trapped_page = Some(page.task);
                            data.trapped_nonce =
                                request.operation.as_ref().map(|probe| probe.nonce);
                            data.record(clock, "paid_pair_foreground_continuation_trapped");
                        }
                        record_b_trace(
                            &trace,
                            clock,
                            "fixture",
                            "executor_unrelated_continuation_trapped".into(),
                        );
                        let _ = continuation.acquire().await;
                    }
                    _ => {}
                }
            })
        }
    }));
    executor.account_sync_notify.notify_one();
    super::cold_account_setup(arrived.acquire())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the real natural tail must arrive: {error:?}; pages={:?}; evidence={:?}",
                pages.lock().unwrap().summary(),
                gates.evidence.lock().unwrap()
            )
        })
        .unwrap()
        .forget();
    // Change the actual full-scope digest after the held snapshot. The newer
    // Wake must create a nonempty partial prefix, not an empty comparison that
    // would legitimately retire as soon as the old tail is repaid.
    partial_keys.lock().unwrap().push(
        crate::node::account_documents::sync::seed_legacy_control_page_for_test(
            source,
            crate::eventlog::ConversationId::new([68; 32]),
        ),
    );
    source.note_account_local_change();
    send_boundary_wake(source, executor).await;
    super::cold_account_setup(prefix_arrived.acquire())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the real protected prefix must arrive: {error:?}; evidence={:?}",
                gates.evidence.lock().unwrap()
            )
        })
        .unwrap()
        .forget();
    // The original socket is now cancelled. Close its obsolete responder gate
    // so it cannot consume an endpoint permit while the real resume is served.
    gates.original.close();
    gates.prefix.close();
    super::cold_account_setup(resume_arrived.acquire())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the exact protected resume must arrive: {error:?}; pages={:?}; evidence={:?}",
                pages.lock().unwrap().summary(),
                gates.evidence.lock().unwrap()
            )
        })
        .unwrap()
        .forget();
    gates
}

async fn run_same_source_receipt(scenario: SameSourceScenario) {
    let partial = !matches!(scenario, SameSourceScenario::Fresh);
    let other_selected = matches!(
        scenario,
        SameSourceScenario::OtherSelected | SameSourceScenario::PaidBoundary
    );
    let paid_boundary = matches!(scenario, SameSourceScenario::PaidBoundary);
    let preparation_wake = matches!(scenario, SameSourceScenario::InterruptedPreparation);
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
    // Import the immutable prefix through the real background workers before
    // arming a held request. Only row 66 is introduced at the natural snapshot;
    // the operation budgets must not include cold replication of rows 1..65.
    let partial_keys = Arc::new(Mutex::new(Vec::new()));
    if partial {
        for value in 1..=65u8 {
            partial_keys.lock().unwrap().push(
                crate::node::account_documents::sync::seed_legacy_control_page_for_test(
                    &owner,
                    crate::eventlog::ConversationId::new([value; 32]),
                ),
            );
        }
    }
    let prefix_metadata = Arc::new(
        partial_keys
            .lock()
            .unwrap()
            .iter()
            .map(|key| owner.account_document_meta(key).unwrap())
            .collect::<Vec<_>>(),
    );
    let mut siblings = vec![owner.clone(), reference.clone()];
    siblings.extend(other.iter().cloned());
    let mut cold_scopes = Vec::new();
    for target in &siblings {
        let mut peers = Vec::new();
        for peer in &siblings {
            if peer.user_id() == target.user_id() {
                continue;
            }
            let scope = Arc::new(Mutex::new(ColdControlScope::default()));
            cold_scopes.push((target.clone(), peer.clone(), scope.clone()));
            peers.push((peer.identity.public(), Arc::downgrade(peer), scope));
        }
        *target.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let target = Arc::downgrade(target);
            let expected = prefix_metadata.clone();
            move |public, event| {
                let Some((_, peer, scope)) = peers.iter().find(|(exact, _, _)| *exact == public)
                else {
                    return;
                };
                let corpus_present = event == "document_scope_settled scope=Control"
                    && target
                        .upgrade()
                        .zip(peer.upgrade())
                        .is_some_and(|(target, peer)| {
                            expected.iter().all(|meta| {
                                target.account_document_meta(&meta.key).as_ref() == Some(meta)
                                    && peer.account_document_meta(&meta.key).as_ref() == Some(meta)
                            })
                        });
                scope.lock().unwrap().observe(&event, corpus_present);
            }
        }));
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
    if let Some(other) = &other {
        other.publish_account_group_access(&mut None);
    }
    super::cold_account_control_setup(
        super::observe_account_fixture_condition(|| {
            cold_scopes.iter().all(|(node, peer, scope)| {
                super::has_account_group_operation_support(
                    node,
                    peer,
                    Some(AccountGroupOperationKind::Rename),
                ) && scope.lock().unwrap().ready()
            })
        }),
        async {
            super::message_action_reference::pull_reference_action_control(
                &reference,
                &owner,
                endpoints[0],
                group,
            )
            .await;
            if let Some(other) = &other {
                super::message_action_reference::pull_reference_action_control(
                    &reference,
                    other,
                    endpoints[3],
                    other_group,
                )
                .await;
            }
        },
    )
    .await;
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(reference.channel_members(group).is_empty());
    assert!(reference.pending_account_operation_sources().is_empty());
    if partial {
        let last = crate::node::AccountDocumentKey::GroupLegacyOperation {
            channel: hex::encode([66; 32]),
            source_device: owner.user_id(),
        };
        assert!(
            siblings
                .iter()
                .all(|node| node.account_document_meta(&last).is_none()),
            "the controlled tail must remain absent until the natural snapshot"
        );
    }

    let b_trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let b_critical = Arc::new(Mutex::new(CriticalControlTrace::default()));
    let b_clock = std::time::Instant::now();
    let b_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(other) = &other {
        *other.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let critical = b_critical.clone();
            let active = b_active.clone();
            let requester = reference.identity.public();
            let source_a = owner.identity.public();
            let own = other.identity.public();
            move |peer, phase| {
                let side = if peer == requester {
                    "B_to_requester"
                } else if peer == source_a {
                    "B_to_A"
                } else if peer == own {
                    "B_publication"
                } else {
                    return;
                };
                critical.lock().unwrap().client(
                    b_clock,
                    side,
                    &phase,
                    active.load(std::sync::atomic::Ordering::Acquire),
                );
            }
        }));
    }
    // Keep the setup interval too: a natural Control read may legitimately
    // yield to authenticated publication before either operation is admitted.
    let abort_trace: BTrace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let active_a_page = Arc::new(Mutex::new(None::<tokio::task::Id>));
    let control_pages = Arc::new(Mutex::new(ControlTestPages::default()));
    let classes = Arc::new(Mutex::new(Vec::new()));
    let aborts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let held_pages = Arc::new(Mutex::new(Vec::<(tokio::task::Id, usize)>::new()));
    // The exact preemption contract begins at the authenticated held read,
    // not earlier natural publication/cold setup traffic.
    let abort_window = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let preparation_candidate =
        Arc::new(Mutex::new(None::<(tokio::task::Id, std::sync::Weak<()>)>));
    let preparation_cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let preparation_wake_acknowledged = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *reference.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let classes = classes.clone();
        let aborts = aborts.clone();
        let held_pages = held_pages.clone();
        let abort_window = abort_window.clone();
        let retired = retired.clone();
        let exact_owner = owner.identity.public();
        let exact_requester = reference.identity.public();
        let exact_other = other.as_ref().map(|other| other.identity.public());
        let b_trace = b_trace.clone();
        let b_critical = b_critical.clone();
        let b_active = b_active.clone();
        let abort_trace = abort_trace.clone();
        let active_a_page = active_a_page.clone();
        let control_pages = control_pages.clone();
        let preparation_candidate = preparation_candidate.clone();
        let preparation_cancelled = preparation_cancelled.clone();
        move |peer, phase| {
            if peer == exact_owner {
                control_pages.lock().unwrap().observe(&phase);
                if preparation_candidate
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|(task, _)| {
                        phase == format!("control_cancel task={task:?} reason=same_source")
                    })
                {
                    preparation_cancelled.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            let side = if peer == exact_owner {
                Some("requester_to_A")
            } else if exact_other.as_ref() == Some(&peer) {
                Some("requester_to_B")
            } else if peer == exact_requester {
                Some("requester_publication")
            } else {
                None
            };
            if let Some(side) = side {
                b_critical.lock().unwrap().client(
                    b_clock,
                    side,
                    &phase,
                    b_active.load(std::sync::atomic::Ordering::Acquire),
                );
            }
            if b_active.load(std::sync::atomic::Ordering::Relaxed)
                && (peer == exact_owner
                    || exact_other.as_ref() == Some(&peer)
                    || peer == exact_requester)
            {
                record_b_trace(
                    &b_trace,
                    b_clock,
                    if peer == exact_owner {
                        "requester_to_A"
                    } else if peer == exact_requester {
                        "requester_publication"
                    } else {
                        "requester_to_B"
                    },
                    phase.clone(),
                );
            }
            if peer != exact_owner {
                return;
            }
            // Cursor.domain may name a skipped domain from another lane.
            // Negotiation identifies the actual GroupControl exchange task.
            let starts_page = phase.starts_with("negotiation_begin domain=Some(GroupControl)");
            let ends_page = phase.starts_with("page_end ")
                && tokio::task::try_id().is_some()
                && tokio::task::try_id() == *active_a_page.lock().unwrap();
            if phase.starts_with("control_dispatch ") {
                *active_a_page.lock().unwrap() = None;
            } else if starts_page {
                *active_a_page.lock().unwrap() = tokio::task::try_id();
            }
            if starts_page
                || ends_page
                || phase.starts_with("control_turn_origin ")
                || phase.starts_with("control_dispatch ")
                || phase.starts_with("control_owned_abort ")
                || phase == "control_pair_retired"
            {
                record_b_trace(
                    &abort_trace,
                    b_clock,
                    "requester_to_A",
                    format!(
                        "page_task={:?} held_window={} {phase}",
                        *active_a_page.lock().unwrap(),
                        abort_window.load(std::sync::atomic::Ordering::Acquire)
                    ),
                );
            }
            if ends_page {
                *active_a_page.lock().unwrap() = None;
            }
            if phase.starts_with("control_turn_origin") || phase.starts_with("control_dispatch") {
                let mut rows = classes.lock().unwrap();
                if rows.len() == 16 {
                    rows.remove(0);
                }
                rows.push(phase);
            } else if phase == "control_pair_retired" {
                retired.store(true, std::sync::atomic::Ordering::Relaxed);
            } else if phase.starts_with("control_cancel task=")
                && abort_window.load(std::sync::atomic::Ordering::Acquire)
                && let Some((_, count)) =
                    held_pages.lock().unwrap().iter_mut().find(|(task, _)| {
                        phase.starts_with(&format!("control_cancel task={task:?} "))
                    })
            {
                *count += 1;
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
    *owner.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let source = Arc::downgrade(&owner);
        let active_operation = active_operation.clone();
        let trace = abort_trace.clone();
        let peer = reference.identity.public();
        move |observed, phase| {
            if phase.starts_with("operation_local_published") {
                if let Some(source) = source.upgrade() {
                    record_b_trace(
                        &trace,
                        b_clock,
                        "A_publication",
                        result_snapshot(&source, group, *active_operation.lock().unwrap()),
                    );
                }
            } else if observed == peer
                && ((phase.starts_with("control_")
                    && !phase.starts_with("control_queue")
                    && !phase.starts_with("control_refresh"))
                    || phase.starts_with("wake_")
                    || phase.starts_with("negotiation_"))
            {
                record_b_trace(&trace, b_clock, "A_to_requester", phase);
            }
        }
    }));
    *reference.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let trace = abort_trace.clone();
        let peer = owner.identity.public();
        let writer = owner.user_id();
        move |observed, frame| {
            let trace = trace.clone();
            let peer = peer.clone();
            let writer = writer.clone();
            Box::pin(async move {
                if observed == peer {
                    record_control_frame(
                        &trace,
                        b_clock,
                        "requester_receive",
                        &frame,
                        group,
                        &writer,
                    );
                }
            })
        }
    }));
    let checkpoints = Arc::new(Mutex::new(Vec::new()));
    let tail_revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let rejected = Arc::new(Mutex::new([0usize; 6]));
    *owner.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let release = release.clone();
        let checkpoints = checkpoints.clone();
        let exact_reference = reference.identity.public();
        let owner_id = owner.user_id();
        let source = Arc::downgrade(&owner);
        let requester = Arc::downgrade(&reference);
        let active_operation = active_operation.clone();
        let resumed_signal = resumed_signal.clone();
        let resumed_release = resumed_release.clone();
        let partial_keys = partial_keys.clone();
        let tail_revision = tail_revision.clone();
        let preparation_candidate = preparation_candidate.clone();
        let preparation_wake_acknowledged = preparation_wake_acknowledged.clone();
        let abort_trace = abort_trace.clone();
        let abort_window = abort_window.clone();
        let held_pages = held_pages.clone();
        let control_pages = control_pages.clone();
        let rejected = rejected.clone();
        move |peer, frame| {
            let owner_id = owner_id.clone();
            let source = source.clone();
            let requester = requester.clone();
            let active_operation = active_operation.clone();
            let resumed_signal = resumed_signal.clone();
            let resumed_release = resumed_release.clone();
            let partial_keys = partial_keys.clone();
            let tail_revision = tail_revision.clone();
            let preparation_candidate = preparation_candidate.clone();
            let preparation_wake_acknowledged = preparation_wake_acknowledged.clone();
            let abort_trace = abort_trace.clone();
            let abort_window = abort_window.clone();
            let held_pages = held_pages.clone();
            let release = release.clone();
            let arrived = arrived.clone();
            let checkpoints = checkpoints.clone();
            let exact_reference = exact_reference.clone();
            let control_pages = control_pages.clone();
            let rejected = rejected.clone();
            Box::pin(async move {
                if peer != exact_reference {
                    return;
                }
                record_control_frame(&abort_trace, b_clock, "A_server", &frame, group, &owner_id);
                let AccountHistoryFrame::DocumentsRequest { body } = frame else {
                    return;
                };
                let request =
                    crate::node::account_documents::wire::DocumentRequest::decode(&body).unwrap();
                if request.domain != AccountDocumentDomain::GroupControl {
                    return;
                }
                if let Some(source) = source.upgrade() {
                    record_b_trace(
                        &abort_trace,
                        b_clock,
                        "A_snapshot_start",
                        result_snapshot(&source, group, *active_operation.lock().unwrap()),
                    );
                }
                let Some(page) = control_pages.lock().unwrap().live_request(&request) else {
                    rejected.lock().unwrap()[0] += 1;
                    return;
                };
                let Some(dispatch) = page.dispatch() else {
                    rejected.lock().unwrap()[1] += 1;
                    return;
                };
                if arrived.lock().unwrap().is_some() && !dispatch.unpaired_background() {
                    let mut rejected = rejected.lock().unwrap();
                    rejected[2] += usize::from(dispatch.paired);
                    rejected[3] += usize::from(dispatch.protected);
                    rejected[4] += usize::from(!dispatch.natural_origin());
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
                    && dispatch.unpaired_background()
                    && arrived.lock().unwrap().is_some()
                {
                    // The immutable prefix is already imported and its old
                    // comparisons are paid. A newer Wake may retire a candidate
                    // during preparation. Revise the same tail at the next
                    // genuine unpaired comparison, within the original cold
                    // budget; stop changing it as soon as the hold is captured.
                    let source = source.upgrade().unwrap();
                    let revision = {
                        // Capture takes this same mutex. Keep the final check,
                        // revision and synchronous publication indivisible so
                        // an old responder cannot mutate after another holds.
                        let admission = arrived.lock().unwrap();
                        if admission.is_none() {
                            return;
                        }
                        let revision =
                            tail_revision.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                        let key = crate::node::account_documents::sync::seed_legacy_control_page_revision_for_test(
                            &source,
                            crate::eventlog::ConversationId::new([66; 32]),
                            revision,
                        );
                        if revision == 1 {
                            partial_keys.lock().unwrap().push(key);
                        } else {
                            assert!(
                                partial_keys.lock().unwrap().last() == Some(&key),
                                "preparation may revise only the same controlled tail key"
                            );
                        }
                        revision
                    };
                    if preparation_wake && revision == 1 {
                        let lifetime = control_pages
                            .lock()
                            .unwrap()
                            .origins
                            .iter()
                            .find(|origin| origin.task == page.task)
                            .and_then(|origin| origin.lifetime.clone())
                            .expect("the first preparation request owns a live child");
                        *preparation_candidate.lock().unwrap() = Some((page.task, lifetime));
                        source.note_account_local_change();
                        send_boundary_wake(&source, &requester.upgrade().unwrap()).await;
                        preparation_wake_acknowledged
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                }
                if partial && !matches!(&effective_after, Some(crate::node::account_documents::AccountDocumentKey::GroupLegacyOperation { .. })) {
                    rejected.lock().unwrap()[5] += 1;
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
                        assert!(
                            tail_revision.load(std::sync::atomic::Ordering::Acquire) > 0
                                && partial_keys.lock().unwrap().len() == 66,
                            "the held tail must follow the prepared prefix and its one new row"
                        );
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
                    let task = page.task;
                    held_pages.lock().unwrap().push((task, 0));
                    abort_window.store(true, std::sync::atomic::Ordering::Release);
                    record_b_trace(
                        &abort_trace,
                        b_clock,
                        "fixture",
                        format!("A_original_read_held page_task={:?}", Some(page.task)),
                    );
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
                            dispatch.protected,
                            "the paired background page must be protected from publication hints"
                        );
                        let task = page.task;
                        {
                            let mut pages = held_pages.lock().unwrap();
                            assert!(
                                pages.len() == 1 && pages[0].0 != task,
                                "the second held read owns a separate bounded page task"
                            );
                            pages.push((task, 0));
                        }
                        record_b_trace(
                            &abort_trace,
                            b_clock,
                            "fixture",
                            format!("A_paired_resume_held page_task={:?}", Some(page.task)),
                        );
                        let _ = signal.send(());
                        let _ = resumed_release.acquire().await;
                    }
                }
            })
        }
    }));
    reference.account_sync_notify.notify_one();
    super::cold_account_setup(async {
        held.await?;
        if preparation_wake {
            super::observe_account_fixture_condition(|| {
                preparation_wake_acknowledged.load(std::sync::atomic::Ordering::Acquire)
            })
            .await;
        }
        Ok::<(), oneshot::error::RecvError>(())
    })
        .await
        .unwrap_or_else(|error| {
            let pages = control_pages.lock().unwrap();
            panic!(
                "the natural same-source background read must be held before admission: {error:?}; tail_revision={} checkpoints={} observed_pages={} live_pages={} preparation_cancelled={} preparation_retired={} wake_acknowledged={} rejected_missing_live_dispatch_paired_protected_origin_continuation={:?}",
                tail_revision.load(std::sync::atomic::Ordering::Acquire),
                checkpoints.lock().unwrap().len(),
                pages.pages.len(),
                pages.pages.iter().filter(|page| pages.is_active(page.task)).count(),
                preparation_cancelled.load(std::sync::atomic::Ordering::Acquire),
                preparation_candidate.lock().unwrap().as_ref().is_some_and(|(_, lifetime)| lifetime.upgrade().is_none()),
                preparation_wake_acknowledged.load(std::sync::atomic::Ordering::Acquire),
                rejected.lock().unwrap()
            )
        })
        .unwrap();
    if preparation_wake {
        let candidate = preparation_candidate.lock().unwrap();
        let (original, lifetime) = candidate
            .as_ref()
            .expect("the authenticated Wake must target the first real preparation request");
        assert!(
            preparation_cancelled.load(std::sync::atomic::Ordering::Acquire)
                && lifetime.upgrade().is_none()
                && held_pages
                    .lock()
                    .unwrap()
                    .first()
                    .is_some_and(|(task, _)| task != original)
                && tail_revision.load(std::sync::atomic::Ordering::Acquire) >= 2,
            "the retired preparation request must be replaced by a newly revised, genuinely unpaired live hold"
        );
        assert!(
            prefix_metadata
                .iter()
                .all(|meta| owner.account_document_meta(&meta.key).as_ref() == Some(meta)),
            "re-arming the tail must preserve all 65 signed prefix rows"
        );
    }
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
    record_b_trace(&abort_trace, b_clock, "fixture", "A_admission_begin".into());
    reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "After same-source hold".into(),
            },
            |commit| {
                let task = held_pages
                    .lock()
                    .unwrap()
                    .first()
                    .expect("the exact natural page is held")
                    .0;
                assert!(
                    control_pages.lock().unwrap().is_active(task),
                    "the exact held natural task must be live at admission"
                );
                commit()
            },
        )
        .unwrap();
    record_b_trace(&abort_trace, b_clock, "fixture", "A_admitted".into());
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
    record_b_trace(
        &abort_trace,
        b_clock,
        "fixture",
        if receipt.is_ok() {
            "A_private_receipt"
        } else {
            "A_deadline_expired"
        }
        .into(),
    );
    let before_release_requester = reference
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let before_release_source = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let mut second_id = None;
    let mut executor_boundary = None;
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
            if paid_boundary {
                // This variant faults B's unrelated incoming-hint scheduler,
                // not the parent's selected-B-vs-held-A boundary. Finish that
                // exact warmup pair before rows 67/68 create a different pair.
                retired.store(false, std::sync::atomic::Ordering::Release);
                release.close();
                resumed_release.close();
                super::cold_account_setup(async {
                    while !retired.load(std::sync::atomic::Ordering::Acquire)
                        || !partial_keys.lock().unwrap().iter().all(|key| {
                            reference.account_document_meta(key).is_some()
                        })
                    {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap_or_else(|error| panic!("the exact parent pair must retire before the independent fault: {error:?}; trace={:?}", abort_trace.lock().unwrap()));
                record_b_trace(
                    &abort_trace,
                    b_clock,
                    "fixture",
                    "parent_pair_retired_before_new_fault".into(),
                );
            }
            let second = reference
                .new_account_group_operation_id(&account.account_id())
                .unwrap();
            second_id = Some(second);
            b_active.store(true, std::sync::atomic::Ordering::Relaxed);
            b_critical
                .lock()
                .unwrap()
                .record(b_clock, "fixture", "B_admission_begin".into());
            record_b_trace(&b_trace, b_clock, "fixture", "B_admission_begin".into());
            record_b_trace(&abort_trace, b_clock, "fixture", "B_admission_begin".into());
            let previous_b_client = other.account_client_test_hook.lock().unwrap().clone();
            *other.account_client_test_hook.lock().unwrap() = Some(Arc::new({
                let previous = previous_b_client;
                let trace = b_trace.clone();
                let requester = reference.identity.public();
                let own = other.identity.public();
                move |peer, phase| {
                    if let Some(previous) = &previous {
                        previous(peer.clone(), phase.clone());
                    }
                    if peer == requester || peer == own {
                        record_b_trace(&trace, b_clock, "B_client", phase);
                    }
                }
            }));
            for (side, target, expected) in [
                ("B_server", other, reference.identity.public()),
                ("requester_server", &reference, other.identity.public()),
                ("A_server_for_B", &owner, other.identity.public()),
            ] {
                let previous = target.account_session_test_hook.lock().unwrap().clone();
                *target.account_session_test_hook.lock().unwrap() = Some(Arc::new({
                    let trace = b_trace.clone();
                    let critical = b_critical.clone();
                    move |peer, frame| {
                        let trace = trace.clone();
                        let critical = critical.clone();
                        let expected = expected.clone();
                        let previous = previous.clone();
                        Box::pin(async move {
                            if peer != expected {
                                if let Some(previous) = previous {
                                    previous(peer, frame).await;
                                }
                                return;
                            }
                            critical.lock().unwrap().frame(b_clock, side, &frame);
                            use crate::node::account_documents::wire::DocumentRequest;
                            let phase = match &frame {
                            AccountHistoryFrame::DocumentsRequest{body} => DocumentRequest::decode(body).filter(|request|request.domain==AccountDocumentDomain::GroupControl).map(|request| format!("Control_request version={} partial={} nonce={:?}",request.version,request.after.is_some() || request.operation.as_ref().is_some_and(|probe|probe.after.is_some()),request.operation.as_ref().map(|probe|hex::encode(&probe.nonce[..4])))),
                            AccountHistoryFrame::DocumentHeader{body} => serde_json::from_slice::<crate::node::AccountDocumentMeta>(body).ok().filter(|meta|matches!(&meta.key,crate::node::AccountDocumentKey::GroupOperation{channel,..} if *channel==hex::encode(other_group.as_bytes()))).map(|meta|format!("B_operation_header revision={} hash={}",meta.revision,hex::encode(&meta.content_hash[..4]))),
                            AccountHistoryFrame::DocumentsComplete{body} => serde_json::from_slice::<serde_json::Value>(body).ok().filter(|value|value.get("domain").and_then(|v|v.as_str())==Some("group_control")).map(|value|format!("Control_complete next={} enabled={}",!value["next"].is_null(),value["enabled"])),
                            _ => None,
                        };
                            if let Some(phase) = phase {
                                record_b_trace(&trace, b_clock, side, phase);
                            }
                            if let Some(previous) = previous {
                                previous(peer, frame).await;
                            }
                        })
                    }
                }));
            }

            if paid_boundary {
                executor_boundary = Some(
                    hold_original_pair_resume(
                        &owner,
                        other,
                        &reference,
                        &partial_keys,
                        &b_trace,
                        b_clock,
                    )
                    .await,
                );
            }
            let request_wake_before_submit = reference.account_wake_token_for_test().1;
            b_critical.lock().unwrap().begin_budget(b_clock);
            reference
                .submit_account_group_operation_if(
                    &account.account_id(),
                    second,
                    other_group,
                    AccountGroupOperation::Rename {
                        name: "Other executor overtook background".into(),
                    },
                    |commit| {
                        if let Some(boundary) = &executor_boundary {
                            let task = boundary
                                .evidence
                                .lock()
                                .unwrap()
                                .resume_page
                                .expect("the exact executor resume is held");
                            assert!(
                                boundary.pages.lock().unwrap().is_active(task),
                                "the exact executor resume must be live at B admission"
                            );
                        } else {
                            let task = held_pages
                                .lock()
                                .unwrap()
                                .last()
                                .expect("the exact paired resume is held")
                                .0;
                            assert!(
                                control_pages.lock().unwrap().is_active(task),
                                "the exact paired resume must be live at B admission"
                            );
                        }
                        commit()
                    },
                )
                .unwrap();
            record_b_trace(&b_trace, b_clock, "fixture", "B_admitted".into());
            b_critical
                .lock()
                .unwrap()
                .record(b_clock, "fixture", "B_admitted".into());
            record_b_trace(&abort_trace, b_clock, "fixture", "B_admitted".into());
            Some(
                tokio::time::timeout(Duration::from_secs(8), async {
                    if let Some(boundary) = &executor_boundary {
                        // Wait inside the ORIGINAL operation budget for its
                        // actual signed request publication, then deliver a real
                        // authenticated Wake before paying the held resume page.
                        let own_key = crate::node::AccountDocumentKey::GroupOperation {
                            channel: hex::encode(other_group.as_bytes()),
                            source_device: reference.user_id(),
                        };
                        while reference.account_wake_token_for_test().1 <= request_wake_before_submit || !reference.account_document(&own_key).unwrap().and_then(|document| {
                            crate::node::account_group::operations::wire::OperationFrame::decode(
                                document.payload(), &reference.account.public().ed25519_pub,
                            )
                        }).is_some_and(|frame| {
                            frame.group == other_group
                                && frame.proof.public() == reference.identity.public()
                                && frame.projection.requests.iter().any(|row| row.reference.id == second)
                        }) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        send_boundary_wake(&reference, other).await;
                        {
                            let mut data = boundary.evidence.lock().unwrap();
                            data.requester_wake = true;
                            data.record(b_clock, "authenticated_requester_wake_acknowledged");
                        }
                        record_b_trace(
                            &b_trace,
                            b_clock,
                            "fixture",
                            "incoming_request_wake_acknowledged_before_paid_resume".into(),
                        );
                        boundary.resume.close();
                    }
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
    let b_critical_snapshot = other_receipt.as_ref().map(|result| {
        let mut trace = b_critical.lock().unwrap();
        trace.record(
            b_clock,
            "fixture",
            if result.is_ok() {
                "B_private_receipt"
            } else {
                "B_deadline_expired"
            }
            .into(),
        );
        let snapshot = trace.snapshot();
        eprintln!(
            "B_critical_at_receipt_deadline paid_boundary={paid_boundary} snapshot={snapshot:?}"
        );
        snapshot
    });
    if let Some(result) = &other_receipt {
        record_b_trace(
            &b_trace,
            b_clock,
            "fixture",
            if result.is_ok() {
                "B_private_receipt"
            } else {
                "B_deadline_expired"
            }
            .into(),
        );
    }
    let b_failure_snapshot = if other_receipt.as_ref().is_some_and(Result::is_err) {
        second_id.map(|second| {
            (
                b_operation_snapshot(&reference, other.as_ref().unwrap(), other_group, second),
                b_operation_snapshot(other.as_ref().unwrap(), &reference, other_group, second),
                classes.lock().unwrap().clone(),
                b_critical_snapshot.clone(),
            )
        })
    } else {
        None
    };
    if let Some((requester, executor, _, _)) = &b_failure_snapshot {
        eprintln!(
            "B_metadata_at_deadline paid_boundary={paid_boundary} requester={requester} executor={executor}"
        );
    }
    if let Some(result) = &other_receipt {
        record_b_trace(
            &abort_trace,
            b_clock,
            "fixture",
            if result.is_ok() {
                "B_private_receipt"
            } else {
                "B_deadline_expired"
            }
            .into(),
        );
    }
    let aborts_before_release = aborts.load(std::sync::atomic::Ordering::Relaxed);
    let held_abort_counts = held_pages.lock().unwrap().clone();
    let effects = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EventKind::ChannelRename)
        .collect::<Vec<_>>();
    release.close();
    resumed_release.close();
    let paid_boundary_evidence = executor_boundary
        .as_ref()
        .map(|boundary| boundary.evidence.lock().unwrap().clone());
    if let Some(evidence) = &paid_boundary_evidence {
        eprintln!("paid_pair_boundary_evidence={evidence:?}");
    }
    drop(executor_boundary);
    let private_failed = receipt.is_err();
    // Draining both preserved background comparisons is fixture recovery,
    // outside the unchanged eight-second A/B operation assertions above.
    let recovered = super::cold_account_setup(async {
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
            let b_receipt_recovered = !paid_boundary
                || second_id.is_some_and(|second| {
                    let source = other.as_ref().unwrap();
                    source
                        .account_group_operation_status(&account.account_id(), second)
                        .unwrap()
                        .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed)
                        && reference.account_group_operation_inputs().into_iter().any(
                            |(_, _, frame)| {
                                frame.group == other_group
                                    && frame.proof.public() == source.identity.public()
                                    && frame.projection.results.iter().any(|row| {
                                        row.reference.id == second
                                            && row.status.phase
                                                == AccountGroupOperationPhase::Committed
                                    })
                            },
                        )
                });
            if checkpoints.lock().unwrap().len() >= 2
                && partial_complete
                && b_receipt_recovered
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
    if other_selected {
        eprintln!("B_after_release paid_boundary={paid_boundary} recovery={recovery_snapshot:?}");
    }
    *owner.account_session_test_hook.lock().unwrap() = None;
    *owner.account_client_test_hook.lock().unwrap() = None;
    *reference.account_client_test_hook.lock().unwrap() = None;
    *reference.account_session_test_hook.lock().unwrap() = None;
    if let Some(other) = &other {
        *other.account_client_test_hook.lock().unwrap() = None;
        *other.account_session_test_hook.lock().unwrap() = None;
    }
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
    if let Some(evidence) = &paid_boundary_evidence {
        assert!(
            evidence.prefix_paid
                && evidence.resume_paid
                && evidence.requester_wake
                && evidence.original == evidence.resumed
                && evidence.source_aborts == 1
                && evidence.invalid.is_none(),
            "the deterministic boundary must pay both protected pages and preserve the exact original checkpoint: {evidence:?}"
        );
        if other_receipt.as_ref().is_some_and(Result::is_err) {
            assert!(
                evidence.continuation_trapped
                    && evidence.ready_requester_hint.is_some()
                    && evidence.requester_control.is_none()
                    && evidence.trapped_protected == Some(true)
                    && evidence.trapped_origin.as_deref()
                        == Some("control_turn_origin resumed=false replacement=false"),
                "a RED must show the genuine ready requester blocked by the still-live paid pair: {evidence:?}"
            );
        } else {
            assert!(
                evidence.requester_control.is_some()
                    && (!evidence.continuation_trapped || evidence.trapped_after_requester),
                "the genuine incoming requester must run before any unrelated continuation after both owed pages are paid: {evidence:?}"
            );
        }
    }
    assert!(
        recovered.is_ok(),
        "releasing the old metadata read must restore ordinary progress and public receipt: {recovery_snapshot:?}; B_failure_snapshot={b_failure_snapshot:?}; paid_boundary={paid_boundary_evidence:?}; abort_tail={:?}",
        abort_trace
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(16)
            .collect::<Vec<_>>()
    );
    assert!(
        before_release_source
            .as_ref()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed),
        "the real source must commit while its prior read is held; abort_trace={:?}",
        abort_trace.lock().unwrap()
    );
    let (requester, source) = receipt.unwrap_or_else(|_| panic!("same-source pre-operation background read blocked its new private receipt: requester={:?} source={:?} abort_trace={:?}", before_release_requester.map(|status| status.phase), before_release_source.map(|status| status.phase), abort_trace.lock().unwrap()));
    assert_eq!(held_abort_counts.len(), if other_selected { 2 } else { 1 });
    assert!(
        if paid_boundary {
            held_abort_counts.iter().map(|(_, count)| *count).eq([1, 0])
        } else {
            held_abort_counts.iter().all(|(_, count)| *count == 1)
        },
        "each exact held page must retain its scenario's cancellation count: held={held_abort_counts:?}; all_aborts={:?}",
        abort_trace.lock().unwrap()
    );
    assert_eq!(
        aborts_before_release,
        if other_selected && !paid_boundary {
            2
        } else {
            1
        },
        "the captured held tasks yield exactly once each despite repeated status polling; unrelated metadata preemptions remain visible in abort_trace={:?}",
        abort_trace.lock().unwrap()
    );
    assert!(
        preserved,
        "the authenticated comparison checkpoint must not be reset or skipped"
    );
    if other_selected {
        let events = other_receipt.expect("the second real operation was admitted after A committed")
            .unwrap_or_else(|_| panic!("a new selected executor must overtake A's held paired background within its unchanged eight seconds: {b_failure_snapshot:?}; paid_boundary={paid_boundary_evidence:?}; held_tasks={held_abort_counts:?}"));
        assert_eq!(events.len(), 1);
        let other = other.as_ref().unwrap();
        let event = other
            .log
            .lock()
            .unwrap()
            .get(&events[0])
            .unwrap()
            .unwrap()
            .clone();
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
