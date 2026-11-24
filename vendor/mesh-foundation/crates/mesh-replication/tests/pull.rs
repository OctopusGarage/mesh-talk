use mesh_replication::{
    CompleteInventory,
    pull::{
        PullCommitProgress, PullErrorKind, PullLimits, Record, pull_missing, pull_missing_observed,
    },
};

fn inventory(ids: &[u8]) -> CompleteInventory {
    let ids: Vec<_> = ids.iter().map(|id| [*id; 32]).collect();
    CompleteInventory::from_ids(&ids, 4).unwrap()
}

#[tokio::test]
async fn reports_committed_prefix_when_the_next_batch_is_invalid() {
    let remote = inventory(&[1, 2, 3]);
    let local = inventory(&[1]);
    let mut committed = Vec::new();
    let mut progress = Vec::new();
    let error = pull_missing_observed(
        &remote,
        &local,
        PullLimits {
            records_per_batch: 1,
            bytes_per_batch: 4,
        },
        async |ids, bytes| {
            assert_eq!(bytes, 4);
            let [id] = ids else {
                panic!("expected one requested record")
            };
            let reply = if *id == [3; 32] { [4; 32] } else { *id };
            Ok::<_, ()>(vec![Record {
                id: reply,
                body: vec![1],
            }])
        },
        |records| {
            committed.push(records[0].id);
            Ok::<_, ()>(())
        },
        |event| progress.push(event),
    )
    .await
    .unwrap_err();
    assert_eq!(committed, [[2; 32]]);
    assert_eq!(error.committed, 1);
    assert_eq!(
        progress,
        [PullCommitProgress {
            batch_records: 1,
            committed_records: 1
        }]
    );
    assert_eq!(error.kind, PullErrorKind::UnexpectedRecords);
}

#[tokio::test]
async fn rejects_duplicate_and_oversized_responses_before_commit() {
    let remote = inventory(&[1, 2]);
    let local = inventory(&[]);
    for (second, expected) in [
        (1, PullErrorKind::UnexpectedRecords),
        (2, PullErrorKind::BodyLimit),
    ] {
        let error = pull_missing(
            &remote,
            &local,
            PullLimits {
                records_per_batch: 2,
                bytes_per_batch: 1,
            },
            async |_: &[[u8; 32]], _| {
                Ok::<_, ()>(vec![
                    Record {
                        id: [1; 32],
                        body: vec![],
                    },
                    Record {
                        id: [second; 32],
                        body: if second == 2 { vec![0, 1] } else { vec![] },
                    },
                ])
            },
            |_| -> Result<(), ()> { panic!("invalid response was committed") },
        )
        .await
        .unwrap_err();
        assert_eq!(error.committed, 0);
        assert_eq!(error.kind, expected);
    }
}

#[tokio::test]
async fn equal_inventory_does_not_fetch_or_commit() {
    let local = inventory(&[1]);
    let committed = pull_missing(
        &local,
        &local,
        PullLimits {
            records_per_batch: 1,
            bytes_per_batch: 1,
        },
        async |_: &[[u8; 32]], _| -> Result<Vec<Record>, ()> { panic!("nothing is missing") },
        |_| -> Result<(), ()> { panic!("nothing was fetched") },
    )
    .await
    .unwrap();
    assert_eq!(committed, 0);
}

#[tokio::test]
async fn bounded_turn_reports_more_and_resumes_from_committed_inventory() {
    use mesh_replication::pull::{PullTurnLimits, pull_missing_bounded};
    let ids: Vec<_> = (0..201)
        .map(|n: u16| {
            let mut id = [0; 32];
            id[..2].copy_from_slice(&n.to_be_bytes());
            id
        })
        .collect();
    let remote = CompleteInventory::from_ids(&ids, 201).unwrap();
    let local = CompleteInventory::from_ids(&[], 201).unwrap();
    let mut stored = Vec::new();
    let first = pull_missing_bounded(
        &remote,
        &local,
        PullLimits {
            records_per_batch: 50,
            bytes_per_batch: 1024,
        },
        PullTurnLimits::default(),
        async |ids, _| {
            Ok::<_, ()>(
                ids.iter()
                    .map(|id| Record {
                        id: *id,
                        body: vec![1],
                    })
                    .collect(),
            )
        },
        |records| {
            stored.extend(records.iter().map(|r| r.id));
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap();
    assert_eq!(first.committed, 200);
    assert!(!first.complete);
    let local = CompleteInventory::from_ids(&stored, 201).unwrap();
    let second = pull_missing_bounded(
        &remote,
        &local,
        PullLimits {
            records_per_batch: 50,
            bytes_per_batch: 1024,
        },
        PullTurnLimits::default(),
        async |ids, _| {
            Ok::<_, ()>(
                ids.iter()
                    .map(|id| Record {
                        id: *id,
                        body: vec![1],
                    })
                    .collect(),
            )
        },
        |_| Ok::<_, ()>(()),
    )
    .await
    .unwrap();
    assert_eq!(second.committed, 1);
    assert!(second.complete);
}

#[tokio::test]
async fn bounded_bytes_requests_and_failed_commit_do_not_acknowledge_uncommitted_data() {
    use mesh_replication::pull::{PullTurnLimits, pull_missing_bounded};
    let remote = inventory(&[1, 2, 3]);
    let local = inventory(&[]);
    for (bytes, requests) in [(2, 8), (8, 2)] {
        let result = pull_missing_bounded(
            &remote,
            &local,
            PullLimits {
                records_per_batch: 1,
                bytes_per_batch: 8,
            },
            PullTurnLimits {
                records: 200,
                bytes,
                requests,
            },
            async |ids, _| {
                Ok::<_, ()>(vec![Record {
                    id: ids[0],
                    body: vec![1],
                }])
            },
            |_| Ok::<_, ()>(()),
        )
        .await
        .unwrap();
        assert_eq!(result.committed, 2);
        assert_eq!(result.bytes, 2);
        assert!(!result.complete);
    }
    let mut calls = 0;
    let error = pull_missing_bounded(
        &remote,
        &local,
        PullLimits {
            records_per_batch: 1,
            bytes_per_batch: 8,
        },
        PullTurnLimits::default(),
        async |ids, _| {
            Ok::<_, ()>(vec![Record {
                id: ids[0],
                body: vec![1],
            }])
        },
        |_| {
            calls += 1;
            if calls == 2 { Err(()) } else { Ok(()) }
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.committed, 1);
    assert_eq!(error.kind, PullErrorKind::Commit(()));
    let error = pull_missing_bounded(
        &remote,
        &local,
        PullLimits {
            records_per_batch: 1,
            bytes_per_batch: 8,
        },
        PullTurnLimits {
            records: 200,
            bytes: 1,
            requests: 8,
        },
        async |ids, _| {
            Ok::<_, ()>(vec![Record {
                id: ids[0],
                body: vec![1; 2],
            }])
        },
        |_| -> Result<(), ()> { panic!("oversize must not commit") },
    )
    .await
    .unwrap_err();
    assert_eq!(error.committed, 0);
    assert_eq!(error.kind, PullErrorKind::BodyLimit);
}

#[tokio::test]
async fn cancelled_turn_resumes_using_durably_committed_ids() {
    use mesh_replication::pull::{PullTurnLimits, pull_missing_bounded};
    use std::sync::{Arc, Mutex};
    let remote = inventory(&[1, 2]);
    let local = inventory(&[]);
    let stored = Arc::new(Mutex::new(Vec::new()));
    let commits = stored.clone();
    let mut calls = 0;
    let mut future = Box::pin(pull_missing_bounded(
        &remote,
        &local,
        PullLimits {
            records_per_batch: 1,
            bytes_per_batch: 8,
        },
        PullTurnLimits::default(),
        async |ids, _| {
            calls += 1;
            if calls == 2 {
                std::future::pending::<()>().await;
            }
            Ok::<_, ()>(vec![Record {
                id: ids[0],
                body: vec![1],
            }])
        },
        move |records| {
            commits.lock().unwrap().extend(records.iter().map(|r| r.id));
            Ok::<_, ()>(())
        },
    ));
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    assert!(std::future::Future::poll(future.as_mut(), &mut context).is_pending());
    drop(future);
    let local = CompleteInventory::from_ids(&stored.lock().unwrap(), 4).unwrap();
    let result = pull_missing_bounded(
        &remote,
        &local,
        PullLimits {
            records_per_batch: 1,
            bytes_per_batch: 8,
        },
        PullTurnLimits::default(),
        async |ids, _| {
            assert_eq!(ids, &[[2; 32]]);
            Ok::<_, ()>(vec![Record {
                id: ids[0],
                body: vec![1],
            }])
        },
        |_| Ok::<_, ()>(()),
    )
    .await
    .unwrap();
    assert_eq!(result.committed, 1);
    assert!(result.complete);
}
