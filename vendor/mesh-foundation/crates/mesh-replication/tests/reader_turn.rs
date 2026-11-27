use mesh_replication::{
    range::{Checkpoint, Prefix, Progress, RangeIndex, RangeRead},
    reader::{PrepareError, ReaderLimits, ReaderSource, prepare},
};
use std::cell::Cell;
fn source() -> ReaderSource {
    ReaderSource {
        scope: [1; 32],
        authority: [2; 32],
        view: [3; 32],
        revision: 1,
    }
}
fn indexes() -> (RangeIndex, RangeIndex) {
    let local = RangeIndex::default();
    let mut remote = RangeIndex::default();
    for n in 0..65u8 {
        remote.insert([n; 32]);
    }
    (local, remote)
}

#[tokio::test]
async fn prepared_frontier_is_released_only_after_successful_commit() {
    let (local, remote) = indexes();
    let mut source = source();
    source.revision = remote.revision();
    let checkpoint =
        Checkpoint::for_reader(source.scope, source.authority, &local, source.revision).unwrap();
    let pending = checkpoint.next_query();
    let batch = prepare(
        &checkpoint,
        &local,
        source,
        ReaderLimits {
            queries: 16,
            records: 32,
        },
        || true,
        async |p| Ok::<_, ()>(remote.frame(source.scope, source.view, p)),
    )
    .await
    .unwrap();
    assert_eq!(checkpoint.next_query(), pending);
    assert!(!batch.missing().is_empty());
    assert!(batch.missing().len() <= 32);
    let expected = batch.missing().to_vec();
    assert_eq!(
        batch
            .commit(|ids| {
                assert_eq!(ids, expected);
                Err::<(), _>("rollback")
            })
            .unwrap_err(),
        "rollback"
    );
    assert_eq!(checkpoint.next_query(), pending);
    let batch = prepare(
        &checkpoint,
        &local,
        source,
        ReaderLimits {
            queries: 16,
            records: 32,
        },
        || true,
        async |p| Ok::<_, ()>(remote.frame(source.scope, source.view, p)),
    )
    .await
    .unwrap();
    let (next, difference) = batch
        .commit(|ids| {
            assert_eq!(ids, expected);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(difference.missing, expected);
    assert_eq!(difference.progress, Progress::More);
    assert_ne!(next.next_query(), checkpoint.next_query());
}

#[tokio::test]
async fn continuation_never_loses_a_leaf_batch_when_record_budget_is_hit() {
    let (local, remote) = indexes();
    let mut source = source();
    source.revision = remote.revision();
    let mut checkpoint =
        Checkpoint::for_reader(source.scope, source.authority, &local, source.revision).unwrap();
    let mut committed = std::collections::BTreeSet::new();
    for _ in 0..65 {
        let batch = prepare(
            &checkpoint,
            &local,
            source,
            ReaderLimits {
                queries: 8,
                records: 40,
            },
            || true,
            async |p| Ok::<_, ()>(remote.frame(source.scope, source.view, p)),
        )
        .await
        .unwrap();
        assert!(batch.examined() <= 8);
        let done = batch.progress() == &Progress::Complete;
        let (next, _) = batch
            .commit(|ids| {
                for id in ids {
                    assert!(committed.insert(*id));
                }
                Ok::<_, ()>(())
            })
            .unwrap();
        checkpoint = next;
        if done {
            break;
        }
    }
    assert_eq!(committed.len(), 65);
    assert!(!checkpoint.has_pending());
}

struct Broken;
impl RangeRead for Broken {
    type Error = &'static str;
    fn revision(&self) -> u64 {
        0
    }
    fn summary(&self, _: Prefix) -> Result<mesh_replication::range::Summary, Self::Error> {
        Err("disk offline")
    }
    fn contains(&self, _: &[u8; 32]) -> Result<bool, Self::Error> {
        Err("disk offline")
    }
}
#[tokio::test]
async fn errors_cancellation_and_revocation_leave_original_checkpoint_untouched() {
    let (local, remote) = indexes();
    let mut s = source();
    s.revision = remote.revision();
    let cp = Checkpoint::for_reader(s.scope, s.authority, &local, s.revision).unwrap();
    assert!(matches!(
        prepare(
            &cp,
            &Broken,
            s,
            ReaderLimits::default(),
            || true,
            async |_| Ok::<_, ()>(remote.frame(s.scope, s.view, Prefix::root()))
        )
        .await,
        Err(PrepareError::Read("disk offline"))
    ));
    assert!(matches!(
        prepare(
            &cp,
            &local,
            s,
            ReaderLimits::default(),
            || true,
            async |_| Err::<mesh_replication::range::SummaryFrame, _>("closed")
        )
        .await,
        Err(PrepareError::Query("closed"))
    ));
    let allowed = Cell::new(true);
    assert!(matches!(
        prepare(
            &cp,
            &local,
            s,
            ReaderLimits::default(),
            || allowed.get(),
            async |p| {
                allowed.set(false);
                Ok::<_, ()>(remote.frame(s.scope, s.view, p))
            }
        )
        .await,
        Err(PrepareError::Invalidated)
    ));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(1),
            prepare(
                &cp,
                &local,
                s,
                ReaderLimits::default(),
                || true,
                async |_| std::future::pending::<Result<_, ()>>().await
            )
        )
        .await
        .is_err()
    );
    assert_eq!(cp.next_query(), Some(Prefix::root()));
}

#[tokio::test]
async fn too_small_allowance_and_invalid_frames_are_explicit_not_completion() {
    let local = RangeIndex::default();
    let mut remote = RangeIndex::default();
    remote.insert([1; 32]);
    remote.insert([2; 32]);
    let mut s = source();
    s.revision = remote.revision();
    let cp = Checkpoint::for_reader(s.scope, s.authority, &local, s.revision).unwrap();
    assert!(matches!(
        prepare(
            &cp,
            &local,
            s,
            ReaderLimits {
                queries: 8,
                records: 1
            },
            || true,
            async |p| Ok::<_, ()>(remote.frame(s.scope, s.view, p))
        )
        .await,
        Err(PrepareError::RecordAllowance {
            required: 2,
            allowance: 1
        })
    ));
    assert!(matches!(
        prepare(
            &cp,
            &local,
            s,
            ReaderLimits::default(),
            || true,
            async |p| Ok::<_, ()>(remote.frame([9; 32], s.view, p))
        )
        .await,
        Err(PrepareError::InvalidFrame)
    ));
    assert!(matches!(
        prepare(
            &cp,
            &local,
            s,
            ReaderLimits {
                queries: 0,
                records: 1
            },
            || true,
            async |_| panic!("must not query")
        )
        .await,
        Err(PrepareError::<std::convert::Infallible, ()>::InvalidLimits)
    ));
    assert_eq!(cp.next_query(), Some(Prefix::root()));
}

#[tokio::test]
async fn completed_checkpoint_still_requalifies_source_and_local_snapshot() {
    let local = RangeIndex::default();
    let s = source();
    let mut s = ReaderSource {
        revision: local.revision(),
        ..s
    };
    let cp = Checkpoint::for_reader(s.scope, s.authority, &local, s.revision).unwrap();
    let prepared = prepare(
        &cp,
        &local,
        s,
        ReaderLimits::default(),
        || true,
        async |p| Ok::<_, ()>(local.frame(s.scope, s.view, p)),
    )
    .await
    .unwrap();
    let (cp, result) = prepared.commit(|_| Ok::<_, ()>(())).unwrap();
    assert_eq!(result.progress, Progress::Complete);
    s.revision += 1;
    assert!(matches!(
        prepare(
            &cp,
            &local,
            s,
            ReaderLimits::default(),
            || true,
            async |_| panic!("must not query stale source")
        )
        .await,
        Err(PrepareError::<std::convert::Infallible, ()>::Invalidated)
    ));
    s.revision -= 1;
    let mut changed = RangeIndex::default();
    changed.insert([9; 32]);
    assert!(matches!(
        prepare(
            &cp,
            &changed,
            s,
            ReaderLimits::default(),
            || true,
            async |_| panic!("must not query changed local view")
        )
        .await,
        Err(PrepareError::<std::convert::Infallible, ()>::Invalidated)
    ));
}

#[tokio::test]
async fn cancelling_after_partial_discovery_or_during_body_fetch_releases_no_frontier() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    let (local, remote) = indexes();
    let mut s = source();
    s.revision = remote.revision();
    let cp = Checkpoint::for_reader(s.scope, s.authority, &local, s.revision).unwrap();
    let calls = Cell::new(0);
    let mut preparation = Box::pin(prepare(
        &cp,
        &local,
        s,
        ReaderLimits::default(),
        || true,
        async |p| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Ok::<_, ()>(remote.frame(s.scope, s.view, p))
            } else {
                std::future::pending().await
            }
        },
    ));
    poll_fn(|cx| {
        assert!(preparation.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(calls.get(), 2, "the root was processed before cancellation");
    drop(preparation);
    assert_eq!(cp.next_query(), Some(Prefix::root()));
    let prepared = prepare(
        &cp,
        &local,
        s,
        ReaderLimits::default(),
        || true,
        async |p| Ok::<_, ()>(remote.frame(s.scope, s.view, p)),
    )
    .await
    .unwrap();
    let commits = Cell::new(0);
    let mut body_fetch = Box::pin(async {
        std::future::pending::<()>().await;
        prepared
            .commit(|_| {
                commits.set(commits.get() + 1);
                Ok::<_, ()>(())
            })
            .unwrap()
    });
    poll_fn(|cx| {
        assert!(body_fetch.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(body_fetch);
    assert_eq!(commits.get(), 0);
    assert_eq!(cp.next_query(), Some(Prefix::root()));
}
