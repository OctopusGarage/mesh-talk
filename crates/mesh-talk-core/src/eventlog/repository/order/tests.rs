use super::super::tests::{database, event};
use super::*;
use crate::identity::device::DeviceIdentity;
fn tiny() -> OrderWorkBudget {
    OrderWorkBudget {
        vertices: 1,
        edges: 1,
        bytes: 256,
    }
}
fn finish(repo: &EventRepository, conv: &ConversationId, kind: CausalOrder) {
    for _ in 0..20000 {
        let step = repo.order_step(conv, kind, tiny()).unwrap();
        assert!(step.seeded + step.ranked + step.cleaned <= 1);
        assert!(step.edges <= 1);
        assert!(step.charged_bytes <= 256);
        if step.readiness == OrderReadiness::Ready {
            return;
        }
    }
    panic!("bounded order did not complete");
}
fn ids(repo: &EventRepository, conv: &ConversationId, kind: CausalOrder) -> Vec<EventId> {
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let OrderReadState::Ready(page) = repo
            .causal_headers_page(
                conv,
                kind,
                cursor.as_ref(),
                pages::PageBudget {
                    rows: 2,
                    bytes: 4096,
                },
            )
            .unwrap()
        else {
            panic!("order pending")
        };
        ids.extend(page.items.into_iter().map(|h| h.id));
        cursor = page.next;
        if cursor.is_none() {
            return ids;
        }
    }
}
#[test]
fn exact_causal_generations_match_kahn_with_inverted_clocks_and_safe_tail() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let a = event(&identity, 1, vec![], 100, EventKind::Message);
    let b = event(&identity, 2, vec![a.id], 1, EventKind::Message);
    let profile = event(&identity, 3, vec![], 200, EventKind::Profile);
    let mut all = vec![a, b, profile];
    let conv = all[0].conversation_id;
    for e in &all {
        repo.append(e).unwrap();
    }
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::Logical,
            None,
            pages::PageBudget::default()
        )
        .unwrap(),
        OrderReadState::Pending
    ));
    finish(&repo, &conv, CausalOrder::Logical);
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    // Previous final rank is Profile 200. Then inverted child leaves last below max.
    let child = event(&identity, 4, vec![all[2].id], 0, EventKind::Message);
    repo.append(&child).unwrap();
    all.push(child);
    finish(&repo, &conv, CausalOrder::Logical);
    let cursor = match repo
        .causal_headers_page(
            &conv,
            CausalOrder::Logical,
            None,
            pages::PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap()
    {
        OrderReadState::Ready(p) => p.next.unwrap(),
        _ => panic!(),
    };
    let late = event(&identity, 5, vec![], 50, EventKind::Message);
    repo.append(&late).unwrap();
    all.push(late);
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::Logical,
            Some(&cursor),
            pages::PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::Logical,
            None,
            pages::PageBudget::default()
        )
        .unwrap(),
        OrderReadState::Pending
    ));
    finish(&repo, &conv, CausalOrder::Logical);
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    // Above every prior vertex: direct rank append with zero order maintenance.
    let tail = event(&identity, 6, vec![all[3].id], 201, EventKind::Message);
    repo.append(&tail).unwrap();
    all.push(tail);
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
#[test]
fn safe_writers_do_not_restart_dirty_base_and_publish_current_order() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let mut all = Vec::new();
    for n in 1..=128 {
        let e = event(&identity, n, vec![], n, EventKind::Message);
        repo.append(&e).unwrap();
        all.push(e);
    }
    let conv = all[0].conversation_id;
    repo.ensure_order(&conv, CausalOrder::Logical).unwrap();
    let mut published = false;
    for n in 129..=2000 {
        let e = event(&identity, n, vec![all[0].id], n, EventKind::Message);
        repo.append(&e).unwrap();
        all.push(e);
        // Small base work, enough tail throughput to catch the producer.
        let step = repo
            .order_step(
                &conv,
                CausalOrder::Logical,
                OrderWorkBudget {
                    vertices: 4,
                    edges: 2,
                    bytes: 1024,
                },
            )
            .unwrap();
        assert!(step.seeded + step.ranked + step.cleaned <= 4 && step.edges <= 2);
        if step.readiness == OrderReadiness::Ready {
            published = true;
            break;
        }
    }
    assert!(
        published,
        "safe writes must not starve generation publication"
    );
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
#[test]
fn fanout_drain_is_bounded_resumable_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let root = event(&identity, 1, vec![], 100, EventKind::Message);
    let mut all = vec![root];
    for n in 2..=130 {
        all.push(event(&identity, n, vec![all[0].id], 1, EventKind::Message));
    }
    for e in &all {
        repo.append(e).unwrap();
    }
    let conv = all[0].conversation_id;
    let mut saw_edge = false;
    for _ in 0..1000 {
        let step = repo
            .order_step(&conv, CausalOrder::Logical, tiny())
            .unwrap();
        if step.edges == 1 {
            saw_edge = true;
            break;
        }
    }
    assert!(saw_edge);
    drop(repo);
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    finish(&repo, &conv, CausalOrder::Logical);
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
#[test]
fn logical_wall_generations_and_domains_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let conv = ConversationId::new([1; 32]);
    let a = Event::new(
        &identity,
        conv,
        1,
        vec![],
        1,
        100,
        EventKind::Message,
        vec![1],
    );
    let b = Event::new(
        &identity,
        conv,
        2,
        vec![],
        100,
        1,
        EventKind::Message,
        vec![2],
    );
    for e in [&a, &b] {
        repo.append(e).unwrap();
        other.append(e).unwrap();
    }
    finish(&repo, &conv, CausalOrder::Logical);
    finish(&repo, &conv, CausalOrder::WallClock);
    assert_eq!(ids(&repo, &conv, CausalOrder::Logical), vec![a.id, b.id]);
    assert_eq!(ids(&repo, &conv, CausalOrder::WallClock), vec![b.id, a.id]);
    assert!(matches!(
        other
            .causal_headers_page(
                &conv,
                CausalOrder::Logical,
                None,
                pages::PageBudget::default()
            )
            .unwrap(),
        OrderReadState::Pending
    ));
    let unknown = ConversationId::new([99; 32]);
    assert!(
        matches!(repo.causal_headers_page(&unknown,CausalOrder::Logical,None,pages::PageBudget::default()).unwrap(),OrderReadState::Ready(p) if p.items.is_empty())
    );
}
#[test]
fn causal_body_pages_preflight_bytes_and_bind_read_kind() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let a = event(&identity, 1, vec![], 1, EventKind::Message);
    let b = Event::new(
        &identity,
        a.conversation_id,
        2,
        vec![],
        2,
        2,
        EventKind::Message,
        vec![3; 65536],
    );
    repo.append(&a).unwrap();
    repo.append(&b).unwrap();
    finish(&repo, &a.conversation_id, CausalOrder::Logical);
    assert!(matches!(
        repo.causal_bodies_page(
            &a.conversation_id,
            CausalOrder::Logical,
            None,
            pages::PageBudget { rows: 1, bytes: 1 }
        ),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let budget = pages::PageBudget {
        rows: 1,
        bytes: 4096,
    };
    let OrderReadState::Ready(page) = repo
        .causal_bodies_page(&a.conversation_id, CausalOrder::Logical, None, budget)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.items, vec![a.clone()]);
    assert!(page.charged_bytes <= 4096);
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    let cursor = page.next.unwrap();
    assert!(matches!(
        repo.causal_headers_page(
            &a.conversation_id,
            CausalOrder::Logical,
            Some(&cursor),
            budget
        ),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        repo.causal_bodies_page(
            &a.conversation_id,
            CausalOrder::Logical,
            Some(&cursor),
            budget
        ),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
#[test]
fn edge_failure_rolls_back_decrement_and_persisted_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let a = event(&identity, 1, vec![], 100, EventKind::Message);
    let b = event(&identity, 2, vec![a.id], 1, EventKind::Message);
    repo.append(&a).unwrap();
    repo.append(&b).unwrap();
    // Seed both and pop root without enough bytes to drain the edge.
    repo.order_step(
        &a.conversation_id,
        CausalOrder::Logical,
        OrderWorkBudget {
            vertices: 3,
            edges: 1,
            bytes: 384,
        },
    )
    .unwrap();
    db.transaction(|c|{c.execute_batch("CREATE TRIGGER order_fail BEFORE UPDATE OF child_after ON event_order_jobs WHEN NEW.child_after>=0 BEGIN SELECT RAISE(ABORT,'injected order cursor failure'); END")?;Ok(())}).unwrap();
    assert!(
        repo.order_step(&a.conversation_id, CausalOrder::Logical, tiny())
            .is_err()
    );
    db.read(|c|{
        let (degree,cursor):(i64,i64)=c.query_row("SELECT w.remaining_degree,j.child_after FROM event_order_work w JOIN event_order_jobs j ON j.job=w.job WHERE w.id=?1",[b.id.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!((degree,cursor),(1,-1));Ok(())
    }).unwrap();
    db.transaction(|c| {
        c.execute_batch("DROP TRIGGER order_fail")?;
        Ok(())
    })
    .unwrap();
    finish(&repo, &a.conversation_id, CausalOrder::Logical);
    assert_eq!(
        ids(&repo, &a.conversation_id, CausalOrder::Logical),
        vec![a.id, b.id]
    );
}
#[test]
fn corrupt_remaining_degree_is_local_error_and_never_publishes_partial_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repo.append(&e).unwrap();
    repo.order_step(
        &e.conversation_id,
        CausalOrder::Logical,
        OrderWorkBudget {
            vertices: 1,
            edges: 1,
            bytes: 128,
        },
    )
    .unwrap();
    db.transaction(|c| {
        c.execute("UPDATE event_order_work SET remaining_degree=1", [])?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        repo.order_step(&e.conversation_id, CausalOrder::Logical, tiny()),
        Err(LogError::CorruptFile(_))
    ));
    assert!(matches!(
        repo.causal_headers_page(
            &e.conversation_id,
            CausalOrder::Logical,
            None,
            pages::PageBudget::default()
        )
        .unwrap(),
        OrderReadState::Pending
    ));
}
#[test]
fn obsolete_cleanup_is_paced_and_repeated_unsafe_writes_do_not_spawn_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let mut all = Vec::new();
    for n in 1..=16 {
        let e = event(&identity, n, vec![], 1000 + n, EventKind::Message);
        repo.append(&e).unwrap();
        all.push(e);
    }
    let conv = all[0].conversation_id;
    repo.order_step(
        &conv,
        CausalOrder::Logical,
        OrderWorkBudget {
            vertices: 8,
            edges: 1,
            bytes: 1024,
        },
    )
    .unwrap();
    for n in 17..=32 {
        let e = event(&identity, n, vec![], 1000 - n, EventKind::Message);
        repo.append(&e).unwrap();
        all.push(e);
        repo.ensure_order(&conv, CausalOrder::Logical).unwrap();
    }
    db.read(|c| {
        assert_eq!(
            c.query_row("SELECT count(*) FROM event_order_jobs", [], |r| r
                .get::<_, i64>(0))?,
            1
        );
        Ok(())
    })
    .unwrap();
    let step = repo
        .order_step(&conv, CausalOrder::Logical, tiny())
        .unwrap();
    assert_eq!(step.cleaned, 1);
    finish(&repo, &conv, CausalOrder::Logical);
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
}
#[test]
fn ready_selection_and_cutoff_edges_use_indexes_without_temp_sort() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let _repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c|{
        for sql in ["EXPLAIN QUERY PLAN SELECT id FROM event_order_work WHERE job=zeroblob(16) AND status=0 AND remaining_degree=0 ORDER BY priority,id LIMIT 1", "EXPLAIN QUERY PLAN SELECT child,child_commit FROM retained_event_parents WHERE domain=zeroblob(16) AND parent=zeroblob(32) AND child_commit>0 AND child_commit<=100 ORDER BY child_commit,child LIMIT 1"] {
            let mut stmt=c.prepare(sql)?;let mut rows=stmt.query([])?;let mut indexed=false;
            while let Some(row)=rows.next()? {let detail:String=row.get(3)?;assert!(!detail.contains("TEMP B-TREE"),"{detail}");indexed|=detail.contains("COVERING INDEX");}
            assert!(indexed);
        }Ok(())
    }).unwrap();
}

#[test]
fn ties_multiple_conversations_and_continued_safe_append_preserve_canonical_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let mut all = Vec::new();
    for n in 1..=16 {
        let e = event(&identity, n, vec![], 7, EventKind::Message);
        repo.append(&e).unwrap();
        all.push(e);
    }
    let conv = all[0].conversation_id;
    let merge = event(
        &identity,
        17,
        all.iter().map(|e| e.id).collect(),
        0,
        EventKind::Message,
    );
    repo.append(&merge).unwrap();
    all.push(merge);
    finish(&repo, &conv, CausalOrder::Logical);
    let cursor = match repo
        .causal_headers_page(
            &conv,
            CausalOrder::Logical,
            None,
            pages::PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap()
    {
        OrderReadState::Ready(p) => p.next.unwrap(),
        _ => panic!(),
    };
    let separate = Event::new(
        &identity,
        ConversationId::new([2; 32]),
        1,
        vec![],
        0,
        0,
        EventKind::Message,
        vec![],
    );
    repo.append(&separate).unwrap();
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::Logical,
            Some(&cursor),
            pages::PageBudget::default()
        )
        .unwrap(),
        OrderReadState::Ready(_)
    ));
    assert!(matches!(
        other.causal_headers_page(
            &conv,
            CausalOrder::Logical,
            Some(&cursor),
            pages::PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::WallClock,
            Some(&cursor),
            pages::PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    for n in 18..=48 {
        let e = event(
            &identity,
            n,
            vec![all[(n as usize) % all.len()].id],
            n,
            if n % 2 == 0 {
                EventKind::Profile
            } else {
                EventKind::Message
            },
        );
        repo.append(&e).unwrap();
        all.push(e);
        assert_eq!(
            ids(&repo, &conv, CausalOrder::Logical),
            crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
        );
    }
}
#[test]
fn missing_owned_order_index_is_not_healed_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.transaction(|c| {
        c.execute_batch("DROP INDEX event_order_ready")?;
        Ok(())
    })
    .unwrap();
    drop(repo);
    assert!(matches!(
        EventRepository::new(db.clone(), EventDomainId([1; 16])),
        Err(LogError::IncompatibleEventSchema)
    ));
    db.read(|c| {
        assert!(!c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='event_order_ready')",
            [],
            |r| r.get::<_, bool>(0)
        )?);
        Ok(())
    })
    .unwrap();
}

#[test]
fn ensure_and_step_reject_missing_current_generation_as_local_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repo.append(&e).unwrap();
    finish(&repo, &e.conversation_id, CausalOrder::Logical);
    db.transaction(|c| {
        c.execute("DELETE FROM event_order_current", [])?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        repo.ensure_order(&e.conversation_id, CausalOrder::Logical),
        Err(LogError::CorruptFile(_))
    ));
    assert!(matches!(
        repo.order_step(&e.conversation_id, CausalOrder::Logical, tiny()),
        Err(LogError::CorruptFile(_))
    ));
}

#[test]
fn causal_page_rejects_missing_ready_binding_as_local_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repo.append(&e).unwrap();
    finish(&repo, &e.conversation_id, CausalOrder::Logical);
    db.transaction(|c| {
        c.execute("DELETE FROM event_order_current", [])?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        repo.causal_headers_page(
            &e.conversation_id,
            CausalOrder::Logical,
            None,
            pages::PageBudget::default()
        ),
        Err(LogError::CorruptFile(_))
    ));
}

#[test]
fn deep_chain_steps_charge_actual_work_and_resume_mid_chain_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let mut all: Vec<Event> = Vec::new();
    for seq in 1..=128 {
        let parents = all.last().map(|e| vec![e.id]).unwrap_or_default();
        // Alternating priorities cannot shortcut the actual multi-level ancestry.
        let clock = if seq % 2 == 0 { seq } else { 1000 - seq };
        let e = event(&identity, seq, parents, clock, EventKind::Message);
        repo.append(&e).unwrap();
        all.push(e);
    }
    let conv = all[0].conversation_id;
    let budget = OrderWorkBudget {
        vertices: 1,
        edges: 1,
        bytes: 192,
    };
    let mut seeded = 0;
    let mut ranked = 0;
    let mut edges = 0;
    let mut reopened = false;
    let mut published = false;
    for _ in 0..1024 {
        let step = repo
            .order_step(&conv, CausalOrder::Logical, budget)
            .unwrap();
        assert!(step.seeded + step.ranked + step.cleaned <= 1);
        assert!(step.edges <= 1);
        assert_eq!(step.cleaned, 0);
        assert_eq!(
            step.charged_bytes,
            128 * (step.seeded + step.ranked) + 64 * step.edges
        );
        assert!(step.charged_bytes <= budget.bytes);
        assert_eq!(
            repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        seeded += step.seeded;
        ranked += step.ranked;
        edges += step.edges;
        if edges == 64 && !reopened {
            let before = repo
                .database
                .read(|c| {
                    let job = repo.order_job(c, &conv, CausalOrder::Logical)?.unwrap();
                    Ok((job.id, job.seeded, job.next_rank, job.pop, job.child_after))
                })
                .unwrap();
            assert_eq!((before.1, before.2), (128, 64));
            assert!(before.3.is_some());
            assert!(before.4 >= 0);
            drop(repo);
            repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
            let after = repo
                .database
                .read(|c| {
                    let job = repo.order_job(c, &conv, CausalOrder::Logical)?.unwrap();
                    Ok((job.id, job.seeded, job.next_rank, job.pop, job.child_after))
                })
                .unwrap();
            assert_eq!(after, before);
            reopened = true;
        }
        if step.readiness == OrderReadiness::Ready {
            published = true;
            break;
        }
    }
    assert!(reopened && published);
    assert_eq!((seeded, ranked, edges), (128, 128, 127));
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn inverted_wall_ancestry_repairs_with_safe_tail_while_logical_order_stays_ready() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let conv = ConversationId::new([1; 32]);
    let parent = Event::new(
        &identity,
        conv,
        1,
        vec![],
        1,
        100,
        EventKind::Message,
        vec![],
    );
    let child = Event::new(
        &identity,
        conv,
        2,
        vec![parent.id],
        2,
        1,
        EventKind::Message,
        vec![],
    );
    let mut all = vec![parent, child];
    for e in &all {
        repo.append(e).unwrap();
    }
    for kind in [CausalOrder::Logical, CausalOrder::WallClock] {
        finish(&repo, &conv, kind);
    }
    assert_eq!(
        ids(&repo, &conv, CausalOrder::WallClock),
        vec![all[0].id, all[1].id]
    );
    let late = Event::new(
        &identity,
        conv,
        3,
        vec![],
        3,
        50,
        EventKind::Message,
        vec![],
    );
    repo.append(&late).unwrap();
    all.push(late);
    assert_eq!(
        ids(&repo, &conv, CausalOrder::Logical),
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
    );
    assert!(matches!(
        repo.causal_headers_page(
            &conv,
            CausalOrder::WallClock,
            None,
            pages::PageBudget::default()
        )
        .unwrap(),
        OrderReadState::Pending
    ));
    // Finish only obsolete cleanup, leaving the new closed base private.
    let mut base_job = None;
    for _ in 0..32 {
        let step = repo
            .order_step(&conv, CausalOrder::WallClock, tiny())
            .unwrap();
        assert_eq!(step.readiness, OrderReadiness::Pending);
        let candidate = repo
            .database
            .read(|c| {
                Ok(repo
                    .order_job(c, &conv, CausalOrder::WallClock)?
                    .filter(|j| j.base_count == 3 && j.phase == SEED)
                    .map(|j| j.id))
            })
            .unwrap();
        if candidate.is_some() {
            base_job = candidate;
            break;
        }
    }
    let base_job = base_job.expect("wall repair must reach its new immutable base");
    let mut published = false;
    for seq in 4..=32 {
        let e = Event::new(
            &identity,
            conv,
            seq,
            vec![all[1].id],
            seq,
            200 + seq,
            EventKind::Message,
            vec![],
        );
        repo.append(&e).unwrap();
        all.push(e);
        let step = repo
            .order_step(
                &conv,
                CausalOrder::WallClock,
                OrderWorkBudget {
                    vertices: 3,
                    edges: 1,
                    bytes: 512,
                },
            )
            .unwrap();
        assert!(step.seeded + step.ranked + step.cleaned <= 3 && step.edges <= 1);
        assert_eq!(
            step.charged_bytes,
            128 * (step.seeded + step.ranked + step.cleaned) + 64 * step.edges
        );
        assert!(step.charged_bytes <= 512);
        let job = repo
            .database
            .read(|c| {
                let job = repo.order_job(c, &conv, CausalOrder::WallClock)?.unwrap();
                Ok((job.id, job.base_count, job.count))
            })
            .unwrap();
        assert_eq!(job, (base_job, 3, all.len() as i64));
        assert_eq!(
            ids(&repo, &conv, CausalOrder::Logical),
            crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], true)
        );
        assert_eq!(
            repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        if step.readiness == OrderReadiness::Ready {
            published = true;
            break;
        }
    }
    assert!(
        published,
        "safe wall-clock writers must permit current publication"
    );
    let wall = ids(&repo, &conv, CausalOrder::WallClock);
    assert_eq!(&wall[..3], &[all[2].id, all[0].id, all[1].id]);
    assert_eq!(
        wall,
        crate::eventlog::order::ordered_ids(&all.iter().collect::<Vec<_>>(), &[], false)
    );
    // Both exact generations now append directly, without another work step.
    let seq = all.len() as u64 + 1;
    let e = Event::new(
        &identity,
        conv,
        seq,
        vec![all[1].id],
        seq,
        1000,
        EventKind::Profile,
        vec![],
    );
    repo.append(&e).unwrap();
    all.push(e);
    for kind in [CausalOrder::Logical, CausalOrder::WallClock] {
        assert_eq!(
            ids(&repo, &conv, kind),
            crate::eventlog::order::ordered_ids(
                &all.iter().collect::<Vec<_>>(),
                &[],
                kind == CausalOrder::Logical
            )
        );
    }
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn obsolete_job_cleanup_and_foreign_key_checks_do_not_scan_current_generations() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let _repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        let cipher: String = c.query_row("PRAGMA cipher_version", [], |r| r.get(0))?;
        assert!(!cipher.is_empty());
        assert_eq!(
            c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
            1
        );
        let mut scans = Vec::new();
        let mut indexed_current_lookups = 0;
        for sql in [
            "EXPLAIN QUERY PLAN DELETE FROM event_order_current WHERE job=zeroblob(16)",
            "EXPLAIN QUERY PLAN DELETE FROM event_order_jobs WHERE job=zeroblob(16)",
        ] {
            let mut stmt = c.prepare(sql)?;
            let mut rows = stmt.query([])?;
            let mut indexed_current = false;
            while let Some(row) = rows.next()? {
                let detail: String = row.get(3)?;
                println!("SQLCipher {cipher}: {sql}: {detail}");
                if detail.contains("event_order_current") {
                    if detail.contains("SCAN ") {
                        scans.push(detail.clone());
                    }
                    indexed_current |= detail.contains("SEARCH ") && detail.contains("job=");
                }
            }
            indexed_current_lookups += usize::from(indexed_current);
        }
        assert!(
            scans.is_empty(),
            "cleanup must not scan unrelated current generations: {scans:?}"
        );
        assert_eq!(
            indexed_current_lookups, 2,
            "both explicit deletion and parent FK checks need indexed job lookups"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn durable_rank_resume_accepts_only_the_same_published_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let first = event(&identity, 1, vec![], 100, EventKind::Message);
    let conv = first.conversation_id;
    repo.append(&first).unwrap();
    finish(&repo, &conv, CausalOrder::Logical);
    let before = repo
        .order_generation(&conv, CausalOrder::Logical)
        .unwrap()
        .unwrap();
    let tail = event(&identity, 2, vec![first.id], 101, EventKind::Message);
    repo.append(&tail).unwrap();
    let after = repo
        .order_generation(&conv, CausalOrder::Logical)
        .unwrap()
        .unwrap();
    assert_eq!(before.job, after.job);
    assert!(after.source_revision > before.source_revision);
    let OrderReadState::Ready(page) = repo
        .causal_headers_after_rank(
            &conv,
            CausalOrder::Logical,
            before.job,
            0,
            pages::PageBudget::default(),
        )
        .unwrap()
    else {
        panic!("safe tail should remain ready")
    };
    assert_eq!(
        page.items.iter().map(|h| h.id).collect::<Vec<_>>(),
        vec![tail.id]
    );
    let OrderReadState::Ready(empty) = repo
        .causal_headers_after_rank(
            &conv,
            CausalOrder::Logical,
            before.job,
            1,
            pages::PageBudget::default(),
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(empty.items.is_empty());
    let late = event(&identity, 3, vec![], 1, EventKind::Message);
    repo.append(&late).unwrap();
    assert!(
        repo.order_generation(&conv, CausalOrder::Logical)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        repo.causal_headers_after_rank(
            &conv,
            CausalOrder::Logical,
            before.job,
            1,
            pages::PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    finish(&repo, &conv, CausalOrder::Logical);
    assert_ne!(
        before.job,
        repo.order_generation(&conv, CausalOrder::Logical)
            .unwrap()
            .unwrap()
            .job
    );
    assert!(matches!(
        repo.causal_headers_after_rank(
            &conv,
            CausalOrder::Logical,
            before.job,
            1,
            pages::PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
#[test]
fn commit_discovery_is_bounded_metadata_and_advances_across_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    for n in 1..=140 {
        repo.append(&event(&identity, n, vec![], n, EventKind::Profile))
            .unwrap();
    }
    let page = repo.committed_headers_page(None, 128).unwrap();
    assert_eq!(page.len(), 128);
    assert!(page.iter().all(|h| h.active));
    let last = page.last().unwrap().ordinal;
    let conv = page[0].header.conversation;
    repo.begin_retirement(&conv).unwrap();
    let tail = repo.committed_headers_page(Some(last), 128).unwrap();
    assert_eq!(tail.len(), 12);
    assert!(tail.iter().all(|h| !h.active && h.ordinal > last));
    assert!(
        repo.committed_headers_page(Some(tail.last().unwrap().ordinal), 128)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        repo.committed_headers_page(None, 129),
        Err(LogError::InvalidPageBudget)
    ));
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn runtime_metadata_combined_history_keeps_delayed_parent_order_and_reuses_unchanged_ranks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([2; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let parent = event(&author, 1, vec![], 100, EventKind::Message);
    let control = event(&author, 2, vec![parent.id], 1, EventKind::KeyRotation);
    let child = event(&author, 3, vec![control.id], 2, EventKind::Message);
    let signed = vec![parent, control, child];
    let conv = signed[0].conversation_id;
    for event in &signed {
        repo.append(event).unwrap();
    }
    finish(&repo, &conv, CausalOrder::WallClock);
    repo.initialize_history_order().unwrap();
    let generation = repo
        .history_generation(
            &conv,
            b"revision 1",
            0,
            repo.order_generation(&conv, CausalOrder::WallClock)
                .unwrap(),
        )
        .unwrap();
    let archive = [
        (EventId::new([21; 32]), 0),
        (EventId::new([22; 32]), 50),
        (EventId::new([23; 32]), 101),
    ];
    let mut visible = vec![(signed[0].id, 100), (signed[2].id, 2)];
    visible.extend(archive);
    for (ordinal, (id, clock)) in visible.iter().enumerate() {
        repo.history_add(
            &conv,
            generation.job,
            combined::HistorySelector {
                id: *id,
                clock: *clock,
                source_kind: 2,
                ordinal: ordinal as i64 + 1,
                priority: 2,
                sequence: 0,
                charged_bytes: 400,
            },
            None,
        )
        .unwrap();
    }
    let mut ready = repo.history_order_step(generation.job).unwrap();
    while !ready.ready {
        ready = repo.history_order_step(generation.job).unwrap();
    }
    let expected =
        crate::eventlog::order::ordered_ids(&signed.iter().collect::<Vec<_>>(), &archive, false)
            .into_iter()
            .filter(|id| visible.iter().any(|(candidate, _)| candidate == id))
            .collect::<Vec<_>>();
    let selected = repo
        .history_selectors(ready.job, 0, ready.count, 4096)
        .unwrap();
    assert_eq!(
        selected.iter().map(|row| row.id).collect::<Vec<_>>(),
        expected
    );
    let warm = repo
        .history_generation(
            &conv,
            b"revision 1",
            0,
            repo.order_generation(&conv, CausalOrder::WallClock)
                .unwrap(),
        )
        .unwrap();
    assert!(warm.ready);
    assert_eq!(warm.job, ready.job);
    assert_eq!(repo.body_read_counts(), (0, 0));
    assert!(matches!(
        repo.history_selectors(ready.job, 0, ready.count, 100),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert_eq!(repo.body_read_counts(), (0, 0));
}

#[test]
fn runtime_metadata_interrupted_history_retirement_cannot_reuse_partial_ready_generation() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([2; 16])).unwrap();
    let conv = ConversationId::new([27; 32]);
    repo.initialize_history_order().unwrap();
    let generation = repo
        .history_generation(&conv, b"original visibility", 0, None)
        .unwrap();
    let populate = |repo: &EventRepository, job| {
        for n in 0u64..300 {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&n.to_be_bytes());
            repo.history_add(
                &conv,
                job,
                combined::HistorySelector {
                    id: EventId::new(id),
                    clock: n,
                    source_kind: 2,
                    ordinal: n as i64 + 1,
                    priority: 2,
                    sequence: 0,
                    charged_bytes: 1,
                },
                None,
            )
            .unwrap();
        }
        loop {
            if repo.history_order_step(job).unwrap().ready {
                break;
            }
        }
    };
    populate(&repo, generation.job);
    db.transaction(|c| {
        c.execute_batch("CREATE TEMP TRIGGER interrupt_history_retirement BEFORE DELETE ON history_order_candidates WHEN (SELECT count(*) FROM history_order_rows WHERE job=OLD.job)<129 BEGIN SELECT RAISE(ABORT,'interrupted retirement'); END;")?;
        Ok(())
    }).unwrap();
    assert!(
        repo.history_generation(&conv, b"changed visibility", 0, None)
            .is_err()
    );
    db.transaction(|c| {
        let remaining: i64 =
            c.query_row("SELECT count(*) FROM history_order_rows", [], |r| r.get(0))?;
        assert_eq!(
            remaining, 172,
            "first bounded cleanup transaction committed before the failure"
        );
        c.execute_batch("DROP TRIGGER interrupt_history_retirement")?;
        Ok(())
    })
    .unwrap();
    drop(repo);
    drop(db);
    let repo = EventRepository::new(database(&dir), EventDomainId([2; 16])).unwrap();
    repo.initialize_history_order().unwrap();
    let resumed = repo
        .history_generation(&conv, b"original visibility", 0, None)
        .unwrap();
    assert!(
        !resumed.ready,
        "a reverted visibility token must not resurrect partially retired ranks"
    );
    assert_ne!(resumed.job, generation.job);
    populate(&repo, resumed.job);
    let ready = repo
        .history_generation(&conv, b"original visibility", 0, None)
        .unwrap();
    assert_eq!(ready.count, 300);
    for start in (0..300).step_by(128) {
        let rows = repo
            .history_selectors(ready.job, start, (start + 128).min(300), 4096)
            .unwrap();
        assert_eq!(rows.first().unwrap().clock, start as u64);
        assert_eq!(rows.len(), (300 - start).min(128));
    }
    assert_eq!(repo.body_read_counts(), (0, 0));
}
