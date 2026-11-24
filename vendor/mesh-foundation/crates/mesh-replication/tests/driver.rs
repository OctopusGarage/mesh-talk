use mesh_replication::{
    driver::{DriverLimits, SyncDriver},
    pull::Record,
    range::RangeIndex,
};
#[tokio::test]
async fn driver_only_reports_complete_after_discovered_records_commit() {
    let mut remote = RangeIndex::default();
    remote.insert([3; 32]);
    let mut local = RangeIndex::default();
    let mut driver = SyncDriver::new([1; 32], [2; 32], &local, remote.revision(), [4; 32]);
    let limits = DriverLimits {
        pull: mesh_replication::pull::PullTurnLimits {
            requests: 1024,
            ..Default::default()
        },
        nodes: 1024,
        pages: 1024,
        scanned: 1024,
        ..DriverLimits::default()
    };
    let result = driver
        .turn(
            &mut local,
            limits,
            || true,
            async |prefix| Ok::<_, ()>(remote.frame([1; 32], [4; 32], prefix)),
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
    assert!(result.complete);
    assert_eq!(result.committed, 1);
    assert_eq!(
        local.summary(mesh_replication::range::Prefix::root()),
        remote.summary(mesh_replication::range::Prefix::root())
    );
}

#[tokio::test]
async fn driver_resumes_default_budget_and_invalidates_external_index_changes() {
    let mut remote = RangeIndex::default();
    for n in 0u8..201 {
        remote.insert([n; 32]);
    }
    let mut local = RangeIndex::default();
    let mut driver = SyncDriver::new([1; 32], [2; 32], &local, remote.revision(), [4; 32]);
    let mut turns = 0;
    loop {
        let result = driver
            .turn(
                &mut local,
                DriverLimits::default(),
                || true,
                async |p| Ok::<_, ()>(remote.frame([1; 32], [4; 32], p)),
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
        assert!(result.requests <= 8);
        assert!(result.pages <= 8);
        assert!(result.committed <= 200);
        turns += 1;
        if result.complete {
            break;
        }
        assert!(turns < 100);
    }
    assert!(turns > 1);
    local.insert([255; 32]);
    assert!(matches!(
        driver
            .turn(
                &mut local,
                DriverLimits::default(),
                || true,
                async |p| Ok::<_, ()>(remote.frame([1; 32], [4; 32], p)),
                async |_, _| Ok::<_, ()>(vec![]),
                |_| Ok::<_, ()>(())
            )
            .await,
        Err(mesh_replication::driver::DriverError::Invalidated { .. })
    ));
}

#[tokio::test]
async fn revocation_during_fetch_prevents_commit_and_completion() {
    use std::cell::Cell;
    let authorized = Cell::new(true);
    let mut remote = RangeIndex::default();
    remote.insert([3; 32]);
    let mut local = RangeIndex::default();
    let mut driver = SyncDriver::new([1; 32], [2; 32], &local, remote.revision(), [4; 32]);
    let result = driver
        .turn(
            &mut local,
            DriverLimits::default(),
            || authorized.get(),
            async |p| Ok::<_, ()>(remote.frame([1; 32], [4; 32], p)),
            async |ids, _| {
                authorized.set(false);
                Ok::<_, ()>(vec![Record {
                    id: ids[0],
                    body: vec![1],
                }])
            },
            |_| -> Result<(), ()> { panic!("revoked commit") },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(local.revision(), 0);
}

#[tokio::test]
async fn cancelled_fetch_retains_discovered_work_and_failed_commit_can_retry() {
    let mut remote = RangeIndex::default();
    remote.insert([3; 32]);
    let mut local = RangeIndex::default();
    let mut driver = SyncDriver::new([1; 32], [2; 32], &local, remote.revision(), [4; 32]);
    let mut future = Box::pin(driver.turn(
        &mut local,
        DriverLimits::default(),
        || true,
        async |p| Ok::<_, ()>(remote.frame([1; 32], [4; 32], p)),
        async |_, _| std::future::pending::<Result<Vec<Record>, ()>>().await,
        |_| Ok::<_, ()>(()),
    ));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(std::future::Future::poll(future.as_mut(), &mut cx).is_pending());
    drop(future);
    let error = driver
        .turn(
            &mut local,
            DriverLimits::default(),
            || true,
            async |_| -> Result<_, ()> { panic!("discovered leaf should be retained") },
            async |ids, _| {
                Ok::<_, ()>(vec![Record {
                    id: ids[0],
                    body: vec![1],
                }])
            },
            |_| Err(()),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        mesh_replication::driver::DriverError::Pull {
            progress: mesh_replication::driver::DriverProgress { committed: 0, .. },
            ..
        }
    ));
    let result = driver
        .turn(
            &mut local,
            DriverLimits::default(),
            || true,
            async |_| -> Result<_, ()> { panic!("discovered leaf should be retained") },
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
    assert!(result.complete);
    assert_eq!(result.committed, 1);
}

#[tokio::test]
async fn cancellation_after_a_committed_batch_keeps_index_and_checkpoint_in_step() {
    use std::cell::Cell;
    let mut remote = RangeIndex::default();
    remote.insert([3; 32]);
    remote.insert([4; 32]);
    let mut local = RangeIndex::default();
    let mut driver = SyncDriver::new([1; 32], [2; 32], &local, remote.revision(), [4; 32]);
    let calls = Cell::new(0);
    let limits = DriverLimits {
        pull: mesh_replication::pull::PullTurnLimits {
            requests: 1024,
            ..Default::default()
        },
        nodes: 1024,
        pages: 1024,
        scanned: 1024,
        batch: mesh_replication::pull::PullLimits {
            records_per_batch: 1,
            bytes_per_batch: 100,
        },
    };
    let mut future = Box::pin(driver.turn(
        &mut local,
        limits,
        || true,
        async |p| Ok::<_, ()>(remote.frame([1; 32], [4; 32], p)),
        async |ids, _| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                std::future::pending::<()>().await;
            }
            Ok::<_, ()>(vec![Record {
                id: ids[0],
                body: vec![1],
            }])
        },
        |_| Ok::<_, ()>(()),
    ));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(std::future::Future::poll(future.as_mut(), &mut cx).is_pending());
    drop(future);
    assert_eq!(
        local.summary(mesh_replication::range::Prefix::root()).count,
        1
    );
    let result = driver
        .turn(
            &mut local,
            limits,
            || true,
            async |p| Ok::<_, ()>(remote.frame([1; 32], [4; 32], p)),
            async |ids, _| {
                assert_eq!(ids, &[[4; 32]]);
                Ok::<_, ()>(vec![Record {
                    id: ids[0],
                    body: vec![1],
                }])
            },
            |_| Ok::<_, ()>(()),
        )
        .await
        .unwrap();
    assert!(result.complete);
    assert_eq!(result.committed, 1);
}

#[tokio::test]
async fn completed_driver_rejects_a_rebuilt_same_revision_different_set() {
    let mut remote = RangeIndex::default();
    remote.insert([1; 32]);
    let mut local = RangeIndex::default();
    local.insert([1; 32]);
    let mut driver = SyncDriver::new([1; 32], [2; 32], &local, remote.revision(), [3; 32]);
    let p = driver
        .turn(
            &mut local,
            DriverLimits::default(),
            || true,
            async |prefix| Ok::<_, ()>(remote.frame([1; 32], [3; 32], prefix)),
            async |_, _| -> Result<Vec<Record>, ()> { panic!("equal sets need no body") },
            |_| Ok::<_, ()>(()),
        )
        .await
        .unwrap();
    assert!(p.complete);
    let revision = local.revision();
    let mut replacement = RangeIndex::default();
    replacement.insert([2; 32]);
    assert_eq!(revision, replacement.revision());
    let p = driver
        .turn(
            &mut replacement,
            DriverLimits::default(),
            || true,
            async |_| -> Result<_, ()> { panic!("stale driver must fail before querying") },
            async |_, _| -> Result<Vec<Record>, ()> {
                panic!("stale driver must fail before fetching")
            },
            |_| Ok::<_, ()>(()),
        )
        .await;
    assert!(matches!(
        p,
        Err(mesh_replication::driver::DriverError::Invalidated { .. })
    ));
}
