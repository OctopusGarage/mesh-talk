use mesh_mailbox::{
    Record, RecordId, ScopeId,
    client::{MailboxTurnErrorKind, MailboxTurnLimits, sync_turn},
    model::{MailboxCursor, MailboxDeltaPage},
    ports::MailboxTransport,
};
use mesh_replication::pull::PullTurnLimits;
use std::{collections::VecDeque, time::Duration};
struct Queue {
    pages: VecDeque<MailboxDeltaPage>,
    bodies: VecDeque<Vec<Record>>,
    pending_body: bool,
    calls: usize,
}
impl Queue {
    fn new(pages: Vec<MailboxDeltaPage>, bodies: Vec<Vec<Record>>) -> Self {
        Self {
            pages: pages.into(),
            bodies: bodies.into(),
            pending_body: false,
            calls: 0,
        }
    }
}
impl MailboxTransport for Queue {
    type Error = &'static str;
    async fn delta(
        &mut self,
        _: ScopeId,
        _: Option<&MailboxCursor>,
        _: usize,
        _: usize,
    ) -> Result<MailboxDeltaPage, Self::Error> {
        self.calls += 1;
        self.pages.pop_front().ok_or("closed")
    }
    async fn bodies(
        &mut self,
        _: ScopeId,
        _: &MailboxCursor,
        _: &[RecordId],
        _: usize,
    ) -> Result<Vec<Record>, Self::Error> {
        self.calls += 1;
        if self.pending_body {
            std::future::pending().await
        } else {
            self.bodies.pop_front().ok_or("closed")
        }
    }
}
fn scope() -> ScopeId {
    ScopeId::new([1; 32])
}
fn cursor(after: i64) -> MailboxCursor {
    MailboxCursor {
        store_uuid: [1; 16],
        scope: scope(),
        epoch: 0,
        revision: 3,
        after,
        window: None,
    }
}
fn page(after: i64, ids: Vec<RecordId>, complete: bool) -> MailboxDeltaPage {
    let mut c = cursor(after);
    if !complete {
        c.window = Some((3, 3));
    }
    MailboxDeltaPage {
        cursor: c,
        ids,
        complete,
        reset: false,
    }
}
fn record(byte: u8) -> Record {
    Record::new(scope(), vec![byte])
}
#[tokio::test]
async fn empty_completion_is_committed_and_failed_commit_keeps_prior() {
    let prior = cursor(0);
    let p = page(3, vec![], true);
    let mut q = Queue::new(vec![p.clone()], vec![]);
    let mut saved = None;
    let result = sync_turn(
        &mut q,
        scope(),
        Some(prior.clone()),
        MailboxTurnLimits::default(),
        |r, c| {
            assert!(r.is_empty());
            saved = Some(c.clone());
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap();
    assert!(result.work.complete);
    assert_eq!(saved, Some(p.cursor.clone()));
    let mut q = Queue::new(vec![p], vec![]);
    let err = sync_turn(
        &mut q,
        scope(),
        Some(prior.clone()),
        MailboxTurnLimits::default(),
        |_, _| Err::<(), _>("disk full"),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err.kind,
        MailboxTurnErrorKind::Commit("disk full")
    ));
    assert_eq!(err.progress.cursor, Some(prior));
}
#[tokio::test]
async fn initial_and_resumed_nonadvancing_pages_never_acknowledge() {
    for prior in [None, Some(cursor(0))] {
        let mut q = Queue::new(vec![page(0, vec![], false)], vec![]);
        let mut saved = false;
        let err = sync_turn(
            &mut q,
            scope(),
            prior.clone(),
            MailboxTurnLimits::default(),
            |_, _| {
                saved = true;
                Ok::<_, ()>(())
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err.kind, MailboxTurnErrorKind::InvalidPage));
        assert!(!saved);
        assert_eq!(err.progress.cursor, prior);
    }
}
#[tokio::test]
async fn source_reset_cannot_acknowledge_a_new_identity() {
    let mut p = page(0, vec![], true);
    p.reset = true;
    p.cursor.store_uuid = [9; 16];
    let mut q = Queue::new(vec![p], vec![]);
    let err = sync_turn(
        &mut q,
        scope(),
        Some(cursor(0)),
        MailboxTurnLimits::default(),
        |_, _| Ok::<_, ()>(()),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err.kind,
        MailboxTurnErrorKind::CheckpointInvalidated
    ));
    assert_eq!(err.progress.cursor, Some(cursor(0)));
}
#[tokio::test]
async fn unfinished_window_cannot_move_or_finish_before_its_upper() {
    let mut prior = cursor(1);
    prior.window = Some((3, 3));
    for (after, window) in [(2, Some((4, 4))), (2, None)] {
        let mut p = page(after, vec![], window.is_none());
        p.cursor.window = window;
        let mut q = Queue::new(vec![p], vec![]);
        let err = sync_turn(
            &mut q,
            scope(),
            Some(prior.clone()),
            MailboxTurnLimits::default(),
            |_, _| Ok::<_, ()>(()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err.kind, MailboxTurnErrorKind::InvalidPage));
        assert_eq!(err.progress.cursor, Some(prior.clone()));
    }
}
#[tokio::test]
async fn duplicate_ids_wrong_scope_and_oversized_pages_fail_before_body_fetch() {
    let r = record(1);
    let mut wrong = page(1, vec![r.id], true);
    wrong.cursor.scope = ScopeId::new([8; 32]);
    for p in [
        page(3, vec![r.id, r.id], true),
        wrong,
        page(3, vec![record(1).id, record(2).id], true),
    ] {
        let mut q = Queue::new(vec![p], vec![]);
        let limits = MailboxTurnLimits {
            page_records: 1,
            ..Default::default()
        };
        let err = sync_turn(&mut q, scope(), None, limits, |_, _| Ok::<_, ()>(()))
            .await
            .unwrap_err();
        assert!(matches!(err.kind, MailboxTurnErrorKind::InvalidPage));
        assert_eq!(q.calls, 1);
    }
}
#[tokio::test]
async fn hash_scope_order_and_byte_validation_precede_commit() {
    let r = record(1);
    let mut bad_hash = r.clone();
    bad_hash.body[0] = 2;
    let mut bad_scope = r.clone();
    bad_scope.scope = ScopeId::new([7; 32]);
    for body in [
        bad_hash,
        bad_scope,
        record(2),
        Record::new(scope(), vec![0; 2048]),
    ] {
        let mut q = Queue::new(vec![page(3, vec![r.id], true)], vec![vec![body]]);
        let mut saved = false;
        let mut limits = MailboxTurnLimits::default();
        limits.work.bytes = 1024;
        let err = sync_turn(&mut q, scope(), None, limits, |_, _| {
            saved = true;
            Ok::<_, ()>(())
        })
        .await
        .unwrap_err();
        assert!(matches!(
            err.kind,
            MailboxTurnErrorKind::InvalidRecords | MailboxTurnErrorKind::BodyLimit
        ));
        assert!(!saved);
        assert!(err.progress.cursor.is_none());
    }
}
#[tokio::test]
async fn a_one_byte_payload_budget_and_one_request_probe_are_valid() {
    let r = record(1);
    let limits = MailboxTurnLimits {
        work: PullTurnLimits {
            records: 1,
            bytes: 1,
            requests: 2,
        },
        pages: 1,
        page_records: 1,
    };
    let mut q = Queue::new(vec![page(3, vec![r.id], true)], vec![vec![r.clone()]]);
    let p = sync_turn(&mut q, scope(), None, limits, |_, _| Ok::<_, ()>(()))
        .await
        .unwrap();
    assert_eq!(p.work.bytes, 1);
    assert!(p.work.complete);
    let mut q = Queue::new(vec![page(3, vec![r.id], true)], vec![]);
    let mut limits = limits;
    limits.work.requests = 1;
    let p = sync_turn(&mut q, scope(), None, limits, |_, _| -> Result<(), ()> {
        panic!("no body commit")
    })
    .await;
    assert!(p.is_ok());
    assert_eq!(q.calls, 1);
}
#[tokio::test]
async fn cancellation_while_loading_a_body_keeps_the_cursor_unacknowledged() {
    let r = record(1);
    let mut q = Queue::new(vec![page(3, vec![r.id], true)], vec![]);
    q.pending_body = true;
    let mut committed = false;
    let result = tokio::time::timeout(
        Duration::from_millis(10),
        sync_turn(
            &mut q,
            scope(),
            Some(cursor(0)),
            MailboxTurnLimits::default(),
            |_, _| {
                committed = true;
                Ok::<_, ()>(())
            },
        ),
    )
    .await;
    assert!(result.is_err());
    assert!(!committed);
}
#[tokio::test]
async fn later_failure_preserves_only_earlier_atomic_page_progress() {
    let first = record(1);
    let second = record(2);
    let mut bad = second.clone();
    bad.body[0] = 3;
    let mut q = Queue::new(
        vec![
            page(1, vec![first.id], false),
            page(3, vec![second.id], true),
        ],
        vec![vec![first], vec![bad]],
    );
    let mut committed = 0;
    let err = sync_turn(
        &mut q,
        scope(),
        None,
        MailboxTurnLimits::default(),
        |r, _| {
            committed += r.len();
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap_err();
    assert_eq!(committed, 1);
    assert_eq!(err.progress.work.committed, 1);
    assert_eq!(err.progress.cursor.unwrap().after, 1);
}
