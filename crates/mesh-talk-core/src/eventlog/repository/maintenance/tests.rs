use super::*;
use crate::eventlog::repository::tests::{database, event};
use crate::identity::device::DeviceIdentity;

fn finish(repo: &EventRepository, token: ProfileMaintenanceToken) -> MaintenanceProgress {
    for _ in 0..1000 {
        let progress = repo
            .step_profile_maintenance(
                token,
                PageBudget {
                    rows: 2,
                    bytes: 1024,
                },
            )
            .unwrap();
        assert!(progress.examined <= 2);
        assert_eq!(progress.charged_bytes, progress.examined * 512);
        if progress.state == MaintenanceState::Ready {
            return progress;
        }
        assert_eq!(progress.state, MaintenanceState::Pending);
    }
    panic!("bounded scan did not finish")
}
#[test]
fn profile_snapshot_preserves_newest_two_and_referenced_parents() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let parent = event(&author, 1, vec![], 1000, EventKind::Profile);
    repo.append(&parent).unwrap();
    // The high clock ancestor is referenced at snapshot selection; deleting its
    // old child must not make the ancestor a candidate in this publication.
    let child = event(&author, 2, vec![parent.id], 0, EventKind::Profile);
    repo.append(&child).unwrap();
    let mut keep = Vec::new();
    for seq in 3..=12 {
        let profile = event(&author, seq, vec![], seq, EventKind::Profile);
        if seq >= 11 {
            keep.push(profile.id)
        }
        repo.append(&profile).unwrap();
    }
    repo.request_profile_maintenance(&parent.conversation_id)
        .unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    let progress = finish(&repo, token);
    assert_eq!(progress.candidates, 9);
    assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 9);
    assert!(repo.has(&parent.id).unwrap());
    assert!(!repo.has(&child.id).unwrap());
    for id in keep {
        assert!(repo.has(&id).unwrap())
    }
    assert_eq!(repo.count(&parent.conversation_id).unwrap(), 3);
    let charge = repo.database.read(|c| repo.domain_charge_on(c)).unwrap();
    assert_eq!(charge.event_count, 3);
    assert_eq!(charge.estimated_bytes, 3 * 384);
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
#[test]
fn conversation_drop_sql_failure_rolls_back_partial_metadata_and_edges() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let first = event(&author, 1, vec![], 1, EventKind::Message);
    let second = event(&author, 2, vec![first.id], 2, EventKind::Message);
    repo.append(&first).unwrap();
    repo.append(&second).unwrap();
    let before = repo.fingerprint_and_len(&first.conversation_id).unwrap();
    let metadata = repo.metadata(&first.conversation_id).unwrap();
    db.transaction(|c| {c.execute_batch("CREATE TEMP TRIGGER fail_drop BEFORE DELETE ON retained_events WHEN (SELECT count(*) FROM retained_events WHERE domain=OLD.domain AND conversation=OLD.conversation)<2 BEGIN SELECT RAISE(ABORT,'injected SQLCipher mutation failure'); END;")?;Ok(())}).unwrap();
    assert!(repo.drop_conversation(&first.conversation_id).is_err());
    assert_eq!(repo.metadata(&first.conversation_id).unwrap(), metadata);
    assert_eq!(
        repo.fingerprint_and_len(&first.conversation_id).unwrap(),
        before
    );
    assert_eq!(repo.heads(&first.conversation_id).unwrap(), vec![second.id]);
    let parents = repo
        .parent_ids_page(&second.id, None, PageBudget::default())
        .unwrap()
        .unwrap();
    assert_eq!(parents.items, vec![first.id]);
    assert_eq!(repo.estimated_bytes().unwrap(), 800);
}

#[test]
fn profile_scan_reopens_and_stale_work_is_cleaned_within_budget() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let mut conv = None;
    for seq in 1..=16 {
        let e = event(&author, seq, vec![], seq, EventKind::Profile);
        conv = Some(e.conversation_id);
        repo.append(&e).unwrap();
    }
    let conv = conv.unwrap();
    repo.request_profile_maintenance(&conv).unwrap();
    let old = repo.begin_profile_maintenance().unwrap();
    assert!(matches!(
        repo.step_profile_maintenance(
            old,
            PageBudget {
                rows: 0,
                bytes: 512
            }
        ),
        Err(LogError::InvalidPageBudget)
    ));
    assert!(matches!(
        repo.step_profile_maintenance(
            old,
            PageBudget {
                rows: 1,
                bytes: 511
            }
        ),
        Err(LogError::PageBudgetTooSmall { required: 512 })
    ));
    finish(&repo, old);
    let e = event(&author, 17, vec![], 17, EventKind::Profile);
    repo.append(&e).unwrap();
    assert_eq!(
        repo.apply_profile_maintenance(old).unwrap().state,
        MaintenanceState::Stale
    );
    let new = repo.begin_profile_maintenance().unwrap();
    let cleanup = repo
        .step_profile_maintenance(
            new,
            PageBudget {
                rows: 1,
                bytes: 512,
            },
        )
        .unwrap();
    assert_eq!(cleanup.state, MaintenanceState::Pending);
    assert_eq!(cleanup.examined, 1);
    drop(repo);
    let reopened = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    let complete = finish(&reopened, new);
    assert_eq!(complete.candidates, 15);
    assert_eq!(reopened.apply_profile_maintenance(new).unwrap().dropped, 15);
    assert_eq!(reopened.count(&conv).unwrap(), 2);
}

#[test]
fn profile_amortization_uses_global_estimate_and_preserves_requests() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let mut conv = None;
    for seq in 1..=9 {
        let e = event(&author, seq, vec![], seq, EventKind::Profile);
        conv = Some(e.conversation_id);
        repo.append(&e).unwrap();
    }
    let conv = conv.unwrap();
    repo.request_profile_maintenance(&conv).unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    assert_eq!(finish(&repo, token).candidates, 7);
    assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 0);
    let cached = repo
        .step_profile_maintenance(
            token,
            PageBudget {
                rows: 1,
                bytes: 512,
            },
        )
        .unwrap();
    assert_eq!(cached.state, MaintenanceState::Ready);
    assert_eq!((cached.examined, cached.charged_bytes), (0, 0));

    let tenth = event(&author, 10, vec![], 10, EventKind::Profile);
    repo.append(&tenth).unwrap();
    let huge = Event::new(
        &DeviceIdentity::generate(),
        ConversationId::new([9; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![0; 1024 * 1024],
    );
    repo.append(&huge).unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    assert_eq!(finish(&repo, token).candidates, 8);
    assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 0);
    repo.drop_conversation(&huge.conversation_id).unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    finish(&repo, token);
    assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 8);
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn profile_apply_sql_failure_rolls_back_and_retry_updates_author_clock_and_cursor() {
    use crate::eventlog::repository::order::{
        CausalOrder, OrderReadState, OrderReadiness, OrderWorkBudget,
    };
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let mut conv = None;
    // Highest author sequence is obsolete under the independent Lamport order.
    for seq in 1..=12 {
        let e = event(&author, seq, vec![], 100 - seq, EventKind::Profile);
        conv = Some(e.conversation_id);
        repo.append(&e).unwrap();
    }
    let conv = conv.unwrap();
    repo.ensure_order(&conv, CausalOrder::Logical).unwrap();
    for _ in 0..100 {
        if repo
            .order_step(&conv, CausalOrder::Logical, OrderWorkBudget::default())
            .unwrap()
            .readiness
            == OrderReadiness::Ready
        {
            break;
        }
    }
    let OrderReadState::Ready(page) = repo
        .causal_headers_page(
            &conv,
            CausalOrder::Logical,
            None,
            PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap()
    else {
        panic!("order not ready")
    };
    let cursor = page.next.unwrap();
    repo.request_profile_maintenance(&conv).unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    finish(&repo, token);
    let metadata = repo.metadata(&conv).unwrap();
    let fingerprint = repo.fingerprint_and_len(&conv).unwrap();
    let estimate = repo.estimated_bytes().unwrap();
    db.transaction(|c|{c.execute_batch("CREATE TEMP TRIGGER fail_profile BEFORE DELETE ON retained_events WHEN (SELECT count(*) FROM retained_events WHERE domain=OLD.domain AND conversation=OLD.conversation)<12 BEGIN SELECT RAISE(ABORT,'injected profile apply failure'); END;")?;Ok(())}).unwrap();
    assert!(repo.apply_profile_maintenance(token).is_err());
    assert_eq!(repo.metadata(&conv).unwrap(), metadata);
    assert_eq!(repo.fingerprint_and_len(&conv).unwrap(), fingerprint);
    assert_eq!(repo.estimated_bytes().unwrap(), estimate);
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::Logical,
            Some(&cursor),
            PageBudget::default()
        )
        .unwrap(),
        OrderReadState::Ready(_)
    ));
    db.transaction(|c| {
        c.execute_batch("DROP TRIGGER fail_profile")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 10);
    assert_eq!(
        repo.author_max(&conv, &Author::from_ed25519(author.public().ed25519_pub))
            .unwrap(),
        Some(2)
    );
    assert_eq!(repo.metadata(&conv).unwrap().unwrap().max_lamport, 99);
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::Logical,
            Some(&cursor),
            PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(repo.estimated_bytes().unwrap(), 768);
}

#[test]
fn maintenance_deletion_foreign_keys_use_exact_reverse_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let _repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        let enabled: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
        assert_eq!(enabled, 1);
        let mut stmt =
            c.prepare("EXPLAIN QUERY PLAN DELETE FROM retained_events WHERE domain=?1 AND id=?2")?;
        let plans: Vec<String> = stmt
            .query_map(params![[1u8; 16].as_slice(), [1u8; 32].as_slice()], |r| {
                r.get(3)
            })?
            .collect::<Result<_, _>>()?;
        assert!(
            plans
                .iter()
                .any(|p| p.contains("event_heads") && p.contains("domain=? AND id=?")),
            "retained event FK plan must seek exact head identity: {plans:?}"
        );
        assert!(
            !plans.iter().any(|p| p.contains("SCAN ")),
            "event delete has unrelated scan: {plans:?}"
        );
        let mut stmt = c.prepare(
            "EXPLAIN QUERY PLAN DELETE FROM event_order_jobs WHERE domain=?1 AND conversation=?2",
        )?;
        let plans: Vec<String> = stmt
            .query_map(params![[1u8; 16].as_slice(), [1u8; 32].as_slice()], |r| {
                r.get(3)
            })?
            .collect::<Result<_, _>>()?;
        assert!(
            !plans.iter().any(|p| p.contains("SCAN ")),
            "order cleanup has unrelated scan: {plans:?}"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn maintenance_refuses_corrupt_oversized_leaf_scratch_before_unbounded_clear() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Profile,
    );
    repo.append(&e).unwrap();
    repo.request_profile_maintenance(&e.conversation_id)
        .unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    db.transaction(|c| {
        for n in 0u8..4 {
            c.execute(
                "INSERT INTO event_profile_leaves VALUES(?1,?2,?3,384)",
                params![
                    token.job.as_slice(),
                    u64::from(n).to_be_bytes().as_slice(),
                    [n; 32].as_slice()
                ],
            )?;
        }
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        repo.step_profile_maintenance(
            token,
            PageBudget {
                rows: 1,
                bytes: 512
            }
        ),
        Err(LogError::CorruptFile(_))
    ));
    db.read(|c| {
        let rows: i64 = c.query_row(
            "SELECT count(*) FROM event_profile_leaves WHERE job=?1",
            [token.job.as_slice()],
            |r| r.get(0),
        )?;
        assert_eq!(rows, 4);
        Ok(())
    })
    .unwrap();
}

#[test]
fn profile_maintenance_matches_legacy_signed_ids_across_snapshot_passes() {
    use crate::eventlog::PersistentEventLog;
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    let mut legacy = PersistentEventLog::open(&dir.path().join("legacy.log"), "password").unwrap();
    let conv = ConversationId::new([1; 32]);
    for _ in 0..8 {
        let author = DeviceIdentity::generate();
        let parent = event(&author, 1, vec![], 0, EventKind::Profile);
        repo.append(&parent).unwrap();
        legacy.append(parent.clone()).unwrap();
        for seq in 2..=12 {
            let parents = if seq == 2 { vec![parent.id] } else { vec![] };
            // Equal clocks exercise exact signed-ID tie-breaking.
            let e = event(&author, seq, parents, seq / 3, EventKind::Profile);
            repo.append(&e).unwrap();
            legacy.append(e).unwrap();
        }
    }
    // One request must survive publication while deleted children expose new
    // superseded parent leaves; callers need not re-request the conversation.
    repo.request_profile_maintenance(&conv).unwrap();
    for _ in 0..3 {
        let token = repo.begin_profile_maintenance().unwrap();
        finish(&repo, token);
        assert_eq!(
            repo.apply_profile_maintenance(token).unwrap().dropped,
            legacy.compact_superseded_profiles(&conv).unwrap()
        );
        let ids = repo
            .ids_page(
                &conv,
                None,
                PageBudget {
                    rows: 1024,
                    bytes: 1024 * 1024,
                },
            )
            .unwrap()
            .items;
        let mut expected: Vec<_> = legacy.events(&conv).iter().map(|e| e.id).collect();
        expected.sort();
        assert_eq!(ids, expected);
        assert_eq!(repo.heads(&conv).unwrap(), legacy.heads(&conv));
        let (digest, count) = legacy.fingerprint_and_len(&conv);
        assert_eq!(
            repo.fingerprint_and_len(&conv).unwrap(),
            (digest, count as u64)
        );
    }
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn maintenance_event_delete_uses_no_unbounded_memory_temp_keyset() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let _repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        for sql in [DELETE_EVENT,DELETE_EDGE,DELETE_HEAD,DELETE_AUTHOR,DELETE_ORDER_ROW,DELETE_ORDER_WORK,DELETE_ORDER_JOB,DELETE_CANDIDATE,DELETE_REQUEST] {
            let mut stmt=c.prepare(&format!("EXPLAIN {sql}"))?;
            let args=vec![rusqlite::types::Value::Blob(vec![1;32]);stmt.parameter_count()];
            let opcodes:Vec<String>=stmt.query_map(rusqlite::params_from_iter(args),|r|r.get(1))?.collect::<Result<_,_>>()?;
            assert!(!opcodes.iter().any(|op|op=="OpenEphemeral" || op=="SorterOpen" || op=="RowSetAdd"),"deletion must not buffer an unbounded removed-ID set with temp_store=MEMORY: {sql}: {opcodes:?}");
        }
        Ok(())
    }).unwrap();
}

#[test]
fn maintenance_active_conversation_and_profile_keysets_seek_owned_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let _repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        for (sql,index,args) in [
            ("EXPLAIN QUERY PLAN SELECT conversation,revision,event_count,stored_bytes,max_lamport,v2_count,v3_count,v4_count,estimated_bytes FROM event_conversations INDEXED BY event_conversations_active WHERE domain=?1 AND lifecycle=0 AND event_count>0 AND conversation>?2 ORDER BY conversation LIMIT ?3", "event_conversations_active", vec![rusqlite::types::Value::Blob(vec![1;16]),rusqlite::types::Value::Blob(vec![1;32]),rusqlite::types::Value::Integer(2)]),
            ("EXPLAIN QUERY PLAN SELECT id,author,lamport,ciphertext_bytes,parent_count FROM retained_events WHERE domain=?1 AND conversation=?2 AND kind=8 AND (author,lamport,id)>(?3,?4,?5) ORDER BY author,lamport,id LIMIT 1", "retained_events_kind_author",vec![rusqlite::types::Value::Blob(vec![1;16]),rusqlite::types::Value::Blob(vec![1;32]),rusqlite::types::Value::Blob(vec![1;32]),rusqlite::types::Value::Blob(vec![1;8]),rusqlite::types::Value::Blob(vec![1;32])]),
        ] {
            let mut stmt=c.prepare(sql)?;
            let plans:Vec<String>=stmt.query_map(rusqlite::params_from_iter(args),|r|r.get(3))?.collect::<Result<_,_>>()?;
            assert!(plans.iter().any(|p|p.contains(index)),"keyset must seek {index}: {plans:?}");
            assert!(!plans.iter().any(|p|p.contains("SCAN ") || p.contains("TEMP B-TREE")),"keyset scans or sorts unrelated metadata: {plans:?}");
        }
        Ok(())
    }).unwrap();
}

#[test]
fn profile_pending_conversations_share_one_snapshot_and_domain_budget() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    let author = DeviceIdentity::generate();
    for conversation in [ConversationId::new([1; 32]), ConversationId::new([2; 32])] {
        for seq in 1..=6 {
            let e = Event::new(
                &author,
                conversation,
                seq,
                vec![],
                seq,
                seq,
                EventKind::Profile,
                vec![42; 128],
            );
            repo.append(&e).unwrap();
            other.append(&e).unwrap();
        }
        repo.request_profile_maintenance(&conversation).unwrap();
    }
    let token = repo.begin_profile_maintenance().unwrap();
    assert_eq!(
        other
            .step_profile_maintenance(token, PageBudget::default())
            .unwrap()
            .state,
        MaintenanceState::Stale
    );
    let snapshot = finish(&repo, token);
    assert_eq!(snapshot.candidates, 8);
    assert_eq!(snapshot.candidate_bytes, 8 * 384);
    assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 8);
    for conv in [ConversationId::new([1; 32]), ConversationId::new([2; 32])] {
        assert_eq!(repo.count(&conv).unwrap(), 2);
        assert_eq!(other.count(&conv).unwrap(), 6);
    }
}

#[test]
fn maintenance_checked_accounting_includes_many_parents_and_source_exhaustion() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let mut parents = Vec::new();
    for seq in 1..=64 {
        let e = event(&author, seq, vec![], seq, EventKind::Message);
        parents.push(e.id);
        repo.append(&e).unwrap();
    }
    let child = event(&author, 65, parents, 65, EventKind::Profile);
    repo.append(&child).unwrap();
    assert_eq!(repo.estimated_bytes().unwrap(), 65 * 384 + 64 * 32);
    let before = repo.metadata(&child.conversation_id).unwrap();
    db.transaction(|c| {
        c.execute(
            "UPDATE event_domains SET source_revision=?2 WHERE domain=?1",
            params![repo.domain.0.as_slice(), i64::MAX],
        )?;
        Ok(())
    })
    .unwrap();
    let next = event(&author, 66, vec![], 66, EventKind::Message);
    assert!(matches!(
        repo.append(&next),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert!(matches!(
        repo.drop_conversation(&child.conversation_id),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert!(matches!(
        repo.request_profile_maintenance(&child.conversation_id),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert_eq!(repo.metadata(&child.conversation_id).unwrap(), before);
    assert!(!repo.has(&next.id).unwrap());
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn drop_recreate_reopen_rejects_old_causal_and_domain_cursors_and_hides_tombstones() {
    use crate::eventlog::repository::order::{
        CausalOrder, OrderReadState, OrderReadiness, OrderWorkBudget,
    };
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let first = event(&author, 1, vec![], 1, EventKind::Message);
    let second = event(&author, 2, vec![first.id], 2, EventKind::Message);
    repo.append(&first).unwrap();
    repo.append(&second).unwrap();
    let other = Event::new(
        &author,
        ConversationId::new([9; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![0; 1024 * 1024],
    );
    repo.append(&other).unwrap();
    repo.ensure_order(&first.conversation_id, CausalOrder::Logical)
        .unwrap();
    for _ in 0..100 {
        if repo
            .order_step(
                &first.conversation_id,
                CausalOrder::Logical,
                OrderWorkBudget::default(),
            )
            .unwrap()
            .readiness
            == OrderReadiness::Ready
        {
            break;
        }
    }
    let OrderReadState::Ready(page) = repo
        .causal_headers_page(
            &first.conversation_id,
            CausalOrder::Logical,
            None,
            PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap()
    else {
        panic!("order not ready")
    };
    let causal = page.next.unwrap();
    let conversations = repo
        .conversations_page(
            None,
            PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap()
        .next
        .unwrap();
    let before = repo.metadata(&first.conversation_id).unwrap();
    let estimate = repo.estimated_bytes().unwrap();
    assert_eq!(
        repo.drop_conversation(&ConversationId::new([8; 32]))
            .unwrap(),
        0
    );
    assert_eq!(repo.metadata(&first.conversation_id).unwrap(), before);
    assert_eq!(repo.estimated_bytes().unwrap(), estimate);
    assert!(
        repo.conversations_page(Some(&conversations), PageBudget::default())
            .is_ok()
    );
    assert_eq!(repo.drop_conversation(&first.conversation_id).unwrap(), 2);
    assert_eq!(repo.drop_conversation(&first.conversation_id).unwrap(), 0);
    assert_eq!(
        repo.conversations_page(None, PageBudget::default())
            .unwrap()
            .items
            .iter()
            .map(|m| m.conversation)
            .collect::<Vec<_>>(),
        vec![other.conversation_id]
    );
    assert_eq!(
        repo.fingerprint_and_len(&first.conversation_id).unwrap().1,
        0
    );
    assert!(matches!(
        repo.causal_headers_page(
            &first.conversation_id,
            CausalOrder::Logical,
            Some(&causal),
            PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        repo.conversations_page(Some(&conversations), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
    repo.append_channel_batch_atomic(&[first.clone(), second.clone()])
        .unwrap();
    repo.ensure_order(&first.conversation_id, CausalOrder::Logical)
        .unwrap();
    for _ in 0..100 {
        if repo
            .order_step(
                &first.conversation_id,
                CausalOrder::Logical,
                OrderWorkBudget::default(),
            )
            .unwrap()
            .readiness
            == OrderReadiness::Ready
        {
            break;
        }
    }
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    drop(repo);
    let reopened = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    assert!(matches!(
        reopened.causal_headers_page(
            &first.conversation_id,
            CausalOrder::Logical,
            Some(&causal),
            PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(reopened.count(&other.conversation_id).unwrap(), 1);
}
