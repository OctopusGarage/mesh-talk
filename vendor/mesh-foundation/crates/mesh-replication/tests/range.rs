use mesh_replication::range::{Checkpoint, Prefix, Progress, RangeIndex};
#[test]
fn cached_equal_and_changed_views() {
    let mut a = RangeIndex::default();
    let mut b = RangeIndex::default();
    for n in 0u32..1000 {
        let mut id = [0; 32];
        id[..4].copy_from_slice(&n.to_be_bytes());
        a.insert(id);
        b.insert(id);
    }
    let mut c = Checkpoint::new([1; 32], [2; 32], &a, &b);
    let d = c.advance([1; 32], [2; 32], &a, &b, 1);
    assert_eq!(d.progress, Progress::Complete);
    assert_eq!(d.examined, 1);
    b.insert([255; 32]);
    assert_eq!(
        c.advance([1; 32], [2; 32], &a, &b, 1).progress,
        Progress::Invalidated
    );
    let mut c = Checkpoint::new([1; 32], [2; 32], &a, &b);
    let mut found = vec![];
    loop {
        let d = c.advance([1; 32], [2; 32], &a, &b, 20);
        assert!(d.examined <= 20);
        found.extend(d.missing);
        if d.progress == Progress::Complete {
            break;
        }
    }
    assert_eq!(found, vec![[255; 32]]);
    b.remove(&[255; 32]);
    assert_eq!(a.summary(Prefix::root()), b.summary(Prefix::root()));
}
#[test]
fn profile_rejects_wrong_scope_noncanonical_and_oversized_frames() {
    use mesh_replication::range::{Summary, SummaryFrame};
    let f = SummaryFrame {
        scope: [1; 32],
        view: [2; 32],
        prefix: Prefix::root(),
        singleton: None,
        leaves: Vec::new(),
        summary: Summary {
            count: 0,
            digest: [0; 32],
        },
    };
    let mut bytes = f.encode();
    assert_eq!(SummaryFrame::decode(&bytes, [1; 32], [2; 32]), Some(f));
    assert!(SummaryFrame::decode(&bytes, [3; 32], [2; 32]).is_none());
    bytes[70] = 1;
    assert!(SummaryFrame::decode(&bytes, [1; 32], [2; 32]).is_none());
    bytes.push(0);
    assert!(SummaryFrame::decode(&bytes, [1; 32], [2; 32]).is_none());
}

#[tokio::test]
async fn remote_queries_are_bounded_and_checkpoint_binds_snapshot() {
    use mesh_replication::range::{RemoteError, SummaryFrame};
    let a = RangeIndex::default();
    let mut b = RangeIndex::default();
    b.insert([9; 32]);
    let mut checkpoint = Checkpoint::new([1; 32], [2; 32], &a, &b);
    let mut found = vec![];
    loop {
        let mut calls = 0;
        let result = checkpoint
            .advance_remote(
                &a,
                [3; 32],
                [2; 32],
                b.revision(),
                10,
                || true,
                async |prefix| {
                    calls += 1;
                    Ok::<_, ()>(SummaryFrame {
                        scope: [1; 32],
                        view: [3; 32],
                        prefix,
                        summary: b.summary(prefix),
                        singleton: None,
                        leaves: Vec::new(),
                    })
                },
            )
            .await
            .unwrap();
        assert!(calls <= 10);
        found.extend(result.missing);
        if result.progress == Progress::Complete {
            break;
        }
    }
    assert_eq!(found, vec![[9; 32]]);
    assert!(matches!(
        checkpoint
            .advance_remote(
                &a,
                [4; 32],
                [2; 32],
                b.revision(),
                10,
                || true,
                async |_p| Err::<SummaryFrame, _>(())
            )
            .await,
        Err(RemoteError::Invalidated)
    ));
}

#[tokio::test]
async fn singleton_uses_one_rpc_and_requires_authority_after_await() {
    use mesh_replication::range::RemoteError;
    let a = RangeIndex::default();
    let mut b = RangeIndex::default();
    b.insert([42; 32]);
    let mut c = Checkpoint::for_remote([1; 32], [2; 32], &a, b.revision());
    let r = c
        .advance_remote(
            &a,
            [3; 32],
            [2; 32],
            b.revision(),
            1,
            || true,
            async |p| Ok::<_, ()>(b.frame([1; 32], [3; 32], p)),
        )
        .await
        .unwrap();
    assert_eq!(r.examined, 1);
    assert_eq!(r.missing, vec![[42; 32]]);
    assert_eq!(r.progress, Progress::Complete);
    let mut c = Checkpoint::for_remote([1; 32], [2; 32], &a, b.revision());
    let mut checks = 0;
    let r = c
        .advance_remote(
            &a,
            [3; 32],
            [2; 32],
            b.revision(),
            1,
            || {
                checks += 1;
                checks <= 2
            },
            async |p| Ok::<_, ()>(b.frame([1; 32], [3; 32], p)),
        )
        .await;
    assert!(matches!(r, Err(RemoteError::Invalidated)));
    let r = c
        .advance_remote(
            &a,
            [3; 32],
            [2; 32],
            b.revision(),
            1,
            || true,
            async |p| Ok::<_, ()>(b.frame([1; 32], [3; 32], p)),
        )
        .await
        .unwrap();
    assert_eq!(r.missing, vec![[42; 32]]);
}
#[test]
fn insertion_order_and_external_changes() {
    let mut a = RangeIndex::default();
    let mut b = RangeIndex::default();
    for n in 0..12 {
        a.insert([n; 32]);
    }
    for n in (0..12).rev() {
        b.insert([n; 32]);
    }
    assert_eq!(a.summary(Prefix::root()), b.summary(Prefix::root()));
    let mut c = Checkpoint::for_remote([1; 32], [2; 32], &a, b.revision());
    a.remove(&[0; 32]);
    assert!(!c.acknowledge_insertions(&mut a, &[[99; 32]]));
}

#[tokio::test]
async fn cancelled_discovery_does_not_consume_prior_prefixes() {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let a = RangeIndex::default();
    let mut b = RangeIndex::default();
    b.insert([0; 32]);
    b.insert([255; 32]);
    let mut checkpoint = Checkpoint::for_remote([1; 32], [2; 32], &a, b.revision());
    let mut calls = 0;
    {
        let pending = checkpoint.advance_remote(
            &a,
            [3; 32],
            [2; 32],
            b.revision(),
            10,
            || true,
            async |p| {
                calls += 1;
                if calls == 3 {
                    std::future::pending::<()>().await;
                }
                let mut frame = b.frame([1; 32], [3; 32], p);
                frame.leaves.clear(); // Exercise multi-query cancellation, not the leaf shortcut.
                Ok::<_, ()>(frame)
            },
        );
        let mut pending = pin!(pending);
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(pending.as_mut().poll(&mut context), Poll::Pending));
    }
    let result = checkpoint
        .advance_remote(
            &a,
            [3; 32],
            [2; 32],
            b.revision(),
            10,
            || true,
            async |p| Ok::<_, ()>(b.frame([1; 32], [3; 32], p)),
        )
        .await
        .unwrap();
    assert_eq!(result.missing, vec![[0; 32], [255; 32]]);
}

#[tokio::test]
async fn session_query_and_server_use_fixed_scope_bound_frames() {
    use mesh_replication::{
        range::{query_summary, serve_summary},
        session::ReconciliationSession,
    };
    use std::collections::VecDeque;
    struct Session {
        incoming: VecDeque<Vec<u8>>,
        sent: Vec<Vec<u8>>,
    }
    impl ReconciliationSession for Session {
        type Peer = ();
        type Error = &'static str;
        fn peer_identity(&self) -> &() {
            &()
        }
        async fn send(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
            self.sent.push(bytes.to_vec());
            Ok(())
        }
        async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
            self.incoming.pop_front().ok_or("closed")
        }
    }
    let mut index = RangeIndex::default();
    index.insert([5; 32]);
    let expected = index.frame([1; 32], [2; 32], Prefix::root());
    let mut requester = Session {
        incoming: VecDeque::from([expected.encode()]),
        sent: vec![],
    };
    assert_eq!(
        query_summary(&mut requester, [1; 32], [2; 32], Prefix::root())
            .await
            .unwrap(),
        expected
    );
    assert_eq!(requester.sent[0].len(), 102);
    let mut server = Session {
        incoming: VecDeque::from([requester.sent.remove(0)]),
        sent: vec![],
    };
    serve_summary(&mut server, &index, [1; 32], [2; 32], || true)
        .await
        .unwrap();
    assert_eq!(server.sent, vec![expected.encode()]);
}

#[test]
fn compressed_index_matches_rebuild_through_shuffled_insertions_and_removals() {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;
    let ids: Vec<[u8; 32]> = (0u64..24)
        .map(|n| Sha256::digest(n.to_le_bytes()).into())
        .collect();
    let mut active = BTreeSet::new();
    let mut index = RangeIndex::default();
    for turn in 0..48 {
        let id = ids[(turn * 7) % 24];
        if turn < 24 {
            assert!(index.insert(id));
            active.insert(id);
        } else {
            assert!(index.remove(&id));
            active.remove(&id);
        }
        let mut rebuilt = RangeIndex::default();
        for id in active.iter().rev() {
            rebuilt.insert(*id);
        }
        assert_eq!(
            index.summary(Prefix::root()),
            rebuilt.summary(Prefix::root())
        );
        assert!(index.stored_summaries() <= active.len().saturating_mul(2).saturating_sub(1));
        for bits in [1, 7, 16, 127, 255, 256] {
            let prefix = Prefix::new(bits, id).unwrap();
            assert_eq!(index.summary(prefix), rebuilt.summary(prefix));
        }
    }
    assert_eq!(index.stored_summaries(), 0);
}

#[test]
fn replacing_an_index_with_same_revision_invalidates_cached_comparisons() {
    use mesh_replication::range::{Checkpoint, Progress, RangeIndex};
    let mut local = RangeIndex::default();
    local.insert([1; 32]);
    let mut remote = RangeIndex::default();
    remote.insert([1; 32]);
    let mut c = Checkpoint::new([1; 32], [2; 32], &local, &remote);
    assert_eq!(
        c.advance([1; 32], [2; 32], &local, &remote, 1).progress,
        Progress::Complete
    );
    let mut rebuilt = RangeIndex::default();
    rebuilt.insert([2; 32]);
    assert_eq!(local.revision(), rebuilt.revision());
    assert_eq!(
        c.advance([1; 32], [2; 32], &rebuilt, &remote, 1).progress,
        Progress::Invalidated
    );
    assert_eq!(
        c.advance([1; 32], [2; 32], &local, &rebuilt, 1).progress,
        Progress::Invalidated
    );
}

#[tokio::test]
async fn leaf_batch_discovers_32_ids_in_one_rpc_and_rejects_forged_contents() {
    use mesh_replication::range::SummaryFrame;
    let mut remote = RangeIndex::default();
    for n in 0..32u8 {
        remote.insert([n; 32]);
    }
    let local = RangeIndex::default();
    let frame = remote.frame([1; 32], [2; 32], Prefix::root());
    assert_eq!(frame.leaves.len(), 32);
    assert_eq!(frame.encode().len(), 1166);
    assert!(SummaryFrame::decode(&frame.encode(), [1; 32], [2; 32]).is_some());
    let mut cp = Checkpoint::for_remote([1; 32], [3; 32], &local, remote.revision());
    let d = cp
        .advance_remote(
            &local,
            [2; 32],
            [3; 32],
            remote.revision(),
            1,
            || true,
            async |_| Ok::<_, ()>(frame.clone()),
        )
        .await
        .unwrap();
    assert_eq!(d.missing.len(), 32);
    assert_eq!(d.examined, 1);
    assert_eq!(d.progress, Progress::Complete);
    for kind in 0..4 {
        let mut bad = frame.clone();
        match kind {
            0 => bad.leaves[1] = bad.leaves[0],
            1 => bad.summary.digest[0] ^= 1,
            2 => bad.prefix = Prefix::new(1, [128; 32]).unwrap(),
            _ => bad.leaves.push([99; 32]),
        }
        assert!(SummaryFrame::decode(&bad.encode(), [1; 32], [2; 32]).is_none());
    }
}
