use mesh_replication::{history::*, pull::Record};
fn binding() -> HistoryBinding {
    HistoryBinding {
        scope: [1; 32],
        source: [2; 32],
        authority: [3; 32],
        snapshot: [4; 32],
        upper: 202,
    }
}
fn page(first: u64, last: u64, next: Option<u64>) -> HistoryPage {
    HistoryPage {
        binding: binding(),
        records: (last..=first)
            .rev()
            .map(|position| HistoryRecord {
                position,
                record: Record {
                    id: [position as u8; 32],
                    body: vec![1],
                },
            })
            .collect(),
        next: next.map(|before| HistoryCursor {
            binding: binding(),
            before,
        }),
        examined: (first - last + 1) as usize,
    }
}
#[tokio::test]
async fn recent_200_then_older_page_is_not_whole_group_equality() {
    let mut saved = None;
    let mut records = Vec::new();
    let first = fetch_page(
        binding(),
        None,
        HistoryLimits::default(),
        || true,
        async |_| Ok::<_, ()>(page(201, 2, Some(2))),
        |p| {
            records.extend(p.records.iter().map(|r| r.position));
            saved = p.next;
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap();
    assert_eq!(first.committed, 200);
    assert!(!first.exhausted);
    assert_eq!(saved.unwrap().before, 2);
    let end = fetch_page(
        binding(),
        saved,
        HistoryLimits::default(),
        || true,
        async |_| Ok::<_, ()>(page(1, 1, None)),
        |p| {
            records.extend(p.records.iter().map(|r| r.position));
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap();
    assert!(end.exhausted);
    assert_eq!(records.len(), 201);
    assert_eq!(records[200], 1);
}
#[tokio::test]
async fn validation_precedes_atomic_body_and_cursor_commit() {
    let mut invalid = Vec::new();
    let mut p = page(4, 3, Some(3));
    p.records[1].record.id = p.records[0].record.id;
    invalid.push(p);
    let mut p = page(4, 3, Some(3));
    p.records.reverse();
    invalid.push(p);
    let mut p = page(4, 3, Some(3));
    p.next.as_mut().unwrap().binding.authority = [9; 32];
    invalid.push(p);
    let mut p = page(4, 3, Some(3));
    p.next.as_mut().unwrap().before = 202;
    invalid.push(p);
    let mut p = page(4, 3, Some(3));
    p.records[0].position = 202;
    invalid.push(p);
    let mut p = page(4, 3, Some(3));
    p.examined = 2001;
    invalid.push(p);
    for p in invalid {
        let mut called = false;
        assert!(
            fetch_page(
                binding(),
                None,
                HistoryLimits::default(),
                || true,
                async |_| Ok::<_, ()>(p.clone()),
                |_| {
                    called = true;
                    Ok::<_, ()>(())
                }
            )
            .await
            .is_err()
        );
        assert!(!called);
    }
}
#[tokio::test]
async fn failed_commit_revocation_and_cancellation_never_acknowledge_a_cursor() {
    let err = fetch_page(
        binding(),
        None,
        HistoryLimits::default(),
        || true,
        async |_| Ok::<_, ()>(page(4, 3, Some(3))),
        |_| Err("disk"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HistoryError::Commit("disk")));
    let mut authorization = 0;
    let mut committed = false;
    let err = fetch_page(
        binding(),
        None,
        HistoryLimits::default(),
        || {
            authorization += 1;
            authorization == 1
        },
        async |_| Ok::<_, ()>(page(4, 3, Some(3))),
        |_| {
            committed = true;
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HistoryError::Unauthorized));
    assert!(!committed);
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(5),
        fetch_page(
            binding(),
            None,
            HistoryLimits::default(),
            || true,
            async |_| std::future::pending::<Result<HistoryPage, ()>>().await,
            |_| {
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
async fn bytes_and_empty_filtered_pages_have_independent_limits() {
    let limits = HistoryLimits {
        bytes: 1,
        ..HistoryLimits::default()
    };
    assert!(
        fetch_page(
            binding(),
            None,
            limits,
            || true,
            async |_| Ok::<_, ()>(page(4, 3, Some(3))),
            |_| Ok::<_, ()>(())
        )
        .await
        .is_err()
    );
    let filtered = HistoryPage {
        binding: binding(),
        records: vec![],
        next: Some(HistoryCursor {
            binding: binding(),
            before: 100,
        }),
        examined: 100,
    };
    let progress = fetch_page(
        binding(),
        None,
        HistoryLimits::default(),
        || true,
        async |_| Ok::<_, ()>(filtered.clone()),
        |_| Ok::<_, ()>(()),
    )
    .await
    .unwrap();
    assert_eq!(progress.committed, 0);
    assert!(!progress.exhausted);
    let mut stale = HistoryCursor {
        binding: binding(),
        before: 100,
    };
    stale.binding.source = [9; 32];
    let mut fetched = false;
    assert!(
        fetch_page(
            binding(),
            Some(stale),
            HistoryLimits::default(),
            || true,
            async |_| {
                fetched = true;
                Ok::<_, ()>(page(4, 3, None))
            },
            |_| Ok::<_, ()>(())
        )
        .await
        .is_err()
    );
    assert!(!fetched);
}

#[tokio::test]
async fn snapshot_changes_and_invalid_limits_fail_before_commit_or_fetch() {
    let mut changed = page(4, 3, None);
    changed.binding.upper += 1;
    assert!(matches!(
        fetch_page(
            binding(),
            None,
            HistoryLimits::default(),
            || true,
            async |_| Ok::<_, ()>(changed.clone()),
            |_| Ok::<_, ()>(())
        )
        .await,
        Err(HistoryError::InvalidPage)
    ));
    for limits in [
        HistoryLimits {
            records: 0,
            ..HistoryLimits::default()
        },
        HistoryLimits {
            bytes: 0,
            ..HistoryLimits::default()
        },
        HistoryLimits {
            examined: 1,
            ..HistoryLimits::default()
        },
    ] {
        let mut fetched = false;
        assert!(matches!(
            fetch_page(
                binding(),
                None,
                limits,
                || true,
                async |_| {
                    fetched = true;
                    Ok::<_, ()>(page(4, 3, None))
                },
                |_| Ok::<_, ()>(())
            )
            .await,
            Err(HistoryError::InvalidLimits)
        ));
        assert!(!fetched);
    }
    let mut empty = HistoryPage {
        binding: binding(),
        records: vec![],
        next: Some(HistoryCursor {
            binding: binding(),
            before: 202,
        }),
        examined: 1,
    };
    assert!(
        fetch_page(
            binding(),
            None,
            HistoryLimits::default(),
            || true,
            async |_| Ok::<_, ()>(empty.clone()),
            |_| Ok::<_, ()>(())
        )
        .await
        .is_err()
    );
    empty.next.as_mut().unwrap().before = 201;
    empty.examined = 0;
    assert!(
        fetch_page(
            binding(),
            None,
            HistoryLimits::default(),
            || true,
            async |_| Ok::<_, ()>(empty.clone()),
            |_| Ok::<_, ()>(())
        )
        .await
        .is_err()
    );
}

#[test]
fn stored_reader_failures_preserve_discovery_until_all_local_reads_succeed() {
    use mesh_replication::range::{Checkpoint, Prefix, RangeIndex, RangeRead, Summary};
    struct Store {
        index: RangeIndex,
        fail: std::cell::Cell<bool>,
    }
    impl RangeRead for Store {
        type Error = &'static str;
        fn revision(&self) -> u64 {
            self.index.revision()
        }
        fn summary(&self, p: Prefix) -> Result<Summary, Self::Error> {
            Ok(self.index.summary(p))
        }
        fn contains(&self, id: &[u8; 32]) -> Result<bool, Self::Error> {
            if *id == [3; 32] && self.fail.get() {
                Err("disk read failed")
            } else {
                Ok(false)
            }
        }
    }
    let local = Store {
        index: RangeIndex::default(),
        fail: std::cell::Cell::new(true),
    };
    let mut remote = RangeIndex::default();
    remote.insert([2; 32]);
    remote.insert([3; 32]);
    let mut checkpoint =
        Checkpoint::for_reader([1; 32], [2; 32], &local, remote.revision()).unwrap();
    let frame = remote.frame([1; 32], [4; 32], Prefix::root());
    assert!(
        checkpoint
            .accept_reader(
                &local,
                [1; 32],
                [4; 32],
                [2; 32],
                remote.revision(),
                frame.clone()
            )
            .is_err()
    );
    assert_eq!(checkpoint.next_query(), Some(Prefix::root()));
    local.fail.set(false);
    assert_eq!(
        checkpoint
            .accept_reader(&local, [1; 32], [4; 32], [2; 32], remote.revision(), frame)
            .unwrap()
            .unwrap(),
        vec![[2; 32], [3; 32]]
    );
    assert!(!checkpoint.has_pending());
}
