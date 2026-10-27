thread_local! {pub(super) static CUT: std::cell::Cell<Option<i64>> = const {std::cell::Cell::new(None)};}
use super::super::tests::{database, event};
use super::*;
use crate::identity::device::DeviceIdentity;
fn token(repo: &EventRepository, conv: &ConversationId) -> RetirementToken {
    match repo.begin_retirement(conv).unwrap() {
        RetirementBegin::Started(t) | RetirementBegin::Existing(t) => t,
        RetirementBegin::Absent => panic!("nonempty scope absent"),
    }
}
#[test]
fn retirement_reclaims_and_preserves_empty_tombstone_then_rejects_recreated_token() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repo.append(&e).unwrap();
    let t = token(&repo, &e.conversation_id);
    let mut done = false;
    for _ in 0..100 {
        if repo
            .reclaim_retirement(&t, RetirementBudget::default())
            .unwrap()
            .complete
        {
            done = true;
            break;
        }
    }
    assert!(done, "bounded reclamation must make progress and finalize");
    assert!(
        repo.reclaim_retirement(&t, RetirementBudget::default())
            .unwrap()
            .complete
    );
    assert_eq!(repo.count(&e.conversation_id).unwrap(), 0);
    repo.append(&e).unwrap();
    assert!(matches!(
        repo.reclaim_retirement(&t, RetirementBudget::default()),
        Err(LogError::StaleCursor)
    ));
}

use super::super::{
    maintenance::MaintenanceState,
    order::{CausalOrder, OrderReadState, OrderReadiness, OrderWorkBudget},
    pages::PageBudget,
};
use crate::eventlog::sync::network::{ExportPageBudget, IdPageBudget, NetworkSyncStore};
use std::sync::atomic::Ordering;
fn pb() -> PageBudget {
    PageBudget {
        rows: 1,
        bytes: 65536,
    }
}
fn small() -> RetirementBudget {
    RetirementBudget {
        auxiliary: 1,
        edges: 1,
        vertices: 1,
        bytes: 256,
    }
}
fn ready(repo: &EventRepository, conv: &ConversationId) {
    for kind in [CausalOrder::Logical, CausalOrder::WallClock] {
        for _ in 0..100 {
            if repo
                .order_step(conv, kind, OrderWorkBudget::default())
                .unwrap()
                .readiness
                == OrderReadiness::Ready
            {
                break;
            }
        }
        assert_eq!(
            repo.ensure_order(conv, kind).unwrap(),
            OrderReadiness::Ready
        );
    }
}
macro_rules! unavailable {
    ($call:expr) => {
        match $call {
            Err(e) => {
                assert!(matches!(e, LogError::ConversationRetiring), "{e}");
                assert!(!e.is_peer_rejection());
            }
            Ok(_) => panic!("retiring API succeeded: {}", stringify!($call)),
        }
    };
}
macro_rules! stale {
    ($call:expr) => {
        assert!(
            matches!($call, Err(LogError::StaleCursor)),
            "old binding must be stale: {}",
            stringify!($call)
        )
    };
}
#[test]
fn retirement_guards_every_ordinary_access_and_stales_previous_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let a = event(&identity, 1, vec![], 100, EventKind::Message);
    let b = event(&identity, 2, vec![], 1, EventKind::ChannelReplica);
    let d = event(
        &identity,
        3,
        vec![a.id, b.id],
        0,
        EventKind::ChannelAccountAuthority,
    );
    for e in [&a, &b, &d] {
        repo.append(e).unwrap();
    }
    let other = Event::new(
        &identity,
        ConversationId::new([2; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![7],
    );
    repo.append(&other).unwrap();
    let before = repo.metadata(&other.conversation_id).unwrap();
    let heads = repo.heads(&other.conversation_id).unwrap();
    ready(&repo, &a.conversation_id);
    let pages = [
        repo.headers_page(&a.conversation_id, None, pb())
            .unwrap()
            .next
            .unwrap(),
        repo.ids_page(&a.conversation_id, None, pb())
            .unwrap()
            .next
            .unwrap(),
        repo.parent_ids_page(&d.id, None, pb())
            .unwrap()
            .unwrap()
            .next
            .unwrap(),
    ];
    let order = match repo
        .causal_headers_page(&a.conversation_id, CausalOrder::Logical, None, pb())
        .unwrap()
    {
        OrderReadState::Ready(p) => p.next.unwrap(),
        _ => panic!(),
    };
    let domain = repo.conversations_page(None, pb()).unwrap().next.unwrap();
    let view = repo.open_view(&a.conversation_id).unwrap();
    repo.fingerprint_and_len(&a.conversation_id).unwrap();
    repo.request_profile_maintenance(&a.conversation_id)
        .unwrap();
    let profile = repo.begin_profile_maintenance().unwrap();
    let charge = repo.database.read(|c| repo.domain_charge_on(c)).unwrap();
    let t = token(&repo, &a.conversation_id);
    let again = token(&repo, &a.conversation_id);
    assert_eq!((t.epoch, t.revision), (again.epoch, again.revision));
    unavailable!(repo.append(&a));
    unavailable!(repo.append_channel_batch_atomic(std::slice::from_ref(&a)));
    unavailable!(repo.has(&a.id));
    unavailable!(repo.get(&a.id));
    unavailable!(repo.header(&a.id));
    unavailable!(repo.metadata(&a.conversation_id));
    unavailable!(repo.count(&a.conversation_id));
    unavailable!(repo.author_max(&a.conversation_id, &a.author));
    unavailable!(repo.next_sequence(&a.conversation_id, &a.author));
    unavailable!(repo.authored_event(&a.conversation_id, &a.author, a.seq));
    unavailable!(repo.heads(&a.conversation_id));
    unavailable!(repo.prepare(&a.conversation_id));
    unavailable!(repo.fingerprint_and_len(&a.conversation_id));
    unavailable!(repo.author_maxima_page(&a.conversation_id, None, pb()));
    unavailable!(repo.headers_page(&a.conversation_id, None, pb()));
    unavailable!(repo.events_page(&a.conversation_id, None, pb()));
    unavailable!(repo.ids_page(&a.conversation_id, None, pb()));
    unavailable!(repo.heads_page(&a.conversation_id, None, pb()));
    unavailable!(repo.parent_ids_page(&d.id, None, pb()));
    unavailable!(repo.ensure_order(&a.conversation_id, CausalOrder::Logical));
    unavailable!(repo.order_step(
        &a.conversation_id,
        CausalOrder::Logical,
        OrderWorkBudget::default()
    ));
    unavailable!(repo.causal_headers_page(&a.conversation_id, CausalOrder::Logical, None, pb()));
    unavailable!(repo.causal_bodies_page(&a.conversation_id, CausalOrder::Logical, None, pb()));
    unavailable!(repo.request_profile_maintenance(&a.conversation_id));
    unavailable!(repo.drop_conversation(&a.conversation_id));
    unavailable!(repo.open_view(&a.conversation_id));
    stale!(repo.headers_page(&a.conversation_id, Some(&pages[0]), pb()));
    stale!(repo.ids_page(&a.conversation_id, Some(&pages[1]), pb()));
    stale!(repo.parent_ids_page(&d.id, Some(&pages[2]), pb()));
    stale!(repo.causal_headers_page(&a.conversation_id, CausalOrder::Logical, Some(&order), pb()));
    stale!(repo.conversations_page(Some(&domain), pb()));
    stale!(repo.validate_view(&view));
    stale!(NetworkSyncStore::metadata(&repo, &view));
    stale!(repo.have_page(
        &view,
        None,
        IdPageBudget {
            ids: 1,
            examined: 1
        }
    ));
    stale!(repo.export_page(
        &view,
        &Default::default(),
        None,
        ExportPageBudget {
            examined: 1,
            events: 1,
            encoded_bytes: 65536,
            allocated_bytes: 65536
        }
    ));
    assert_eq!(
        repo.step_profile_maintenance(profile, pb()).unwrap().state,
        MaintenanceState::Stale
    );
    assert_eq!(
        repo.apply_profile_maintenance(profile).unwrap().state,
        MaintenanceState::Stale
    );
    assert_eq!(
        repo.conversations_page(None, pb()).unwrap().items[0].conversation,
        other.conversation_id
    );
    assert_eq!(repo.metadata(&other.conversation_id).unwrap(), before);
    assert_eq!(repo.heads(&other.conversation_id).unwrap(), heads);
    assert_eq!(
        repo.database.read(|c| repo.domain_charge_on(c)).unwrap(),
        charge
    );
    assert_eq!(repo.body_fetches.load(Ordering::Relaxed), 0);
    assert_eq!(repo.body_decodes.load(Ordering::Relaxed), 0);
}

fn exact_physical(repo: &EventRepository, conv: &ConversationId) {
    repo.database.read(|c|{
        let oracle=|scope:bool| -> Result<(i64,i64,i64),LogError>{
            let clause=if scope{"AND conversation=?2"}else{"AND ?2 IS NOT NULL"};
            Ok(c.query_row(&format!("SELECT count(*),coalesce(sum(stored_bytes),0),coalesce(sum(ciphertext_bytes+parent_count*32+256),0) FROM retained_events WHERE domain=?1 {clause}"),params![repo.domain.0.as_slice(),conv.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
        };
        let a=oracle(true)?;let m=repo.conversation_charge_on(c,conv)?.unwrap();assert_eq!((m.count,m.stored_bytes,m.estimated_bytes),(a.0 as u64,a.1 as u64,a.2 as u64));
        let a=oracle(false)?;let d=repo.domain_charge_on(c)?;assert_eq!((d.event_count,d.estimated_bytes),(a.0 as u64,a.2 as u64));
        let mut fk=c.prepare("PRAGMA foreign_key_check")?;assert!(fk.query([])?.next()?.is_none());Ok(())
    }).unwrap();
}
#[test]
fn retirement_tiny_steps_bound_fanin_edges_and_keep_exact_charges_at_every_commit() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let id = DeviceIdentity::generate();
    let mut roots = Vec::new();
    for seq in 1..=40 {
        let e = event(&id, seq, vec![], 1000 - seq, EventKind::Message);
        repo.append(&e).unwrap();
        roots.push(e.id);
    }
    let join = event(&id, 41, roots, 0, EventKind::ChannelAccountAuthority);
    repo.append(&join).unwrap();
    for seq in 42..=60 {
        repo.append(&event(
            &id,
            seq,
            vec![join.id],
            100 - seq,
            EventKind::Profile,
        ))
        .unwrap();
    }
    let oversized = Event::new(
        &id,
        join.conversation_id,
        61,
        vec![join.id],
        0,
        0,
        EventKind::Message,
        vec![9; 32768],
    );
    repo.append(&oversized).unwrap();
    repo.database
        .transaction(|c| {
            c.execute(
                "UPDATE retained_events SET body=zeroblob(length(body)) WHERE domain=?1 AND id=?2",
                params![repo.domain.0.as_slice(), oversized.id.as_bytes().as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    ready(&repo, &join.conversation_id);
    let t = token(&repo, &join.conversation_id);
    let mut edge_steps = 0;
    let mut total_edges = 0;
    let mut reclaimed = 0;
    let mut body = 0;
    let mut oversize = false;
    let mut complete = false;
    for _ in 0..2000 {
        let p = repo.reclaim_retirement(&t, small()).unwrap();
        assert!(p.auxiliary <= 1 && p.edges <= 1 && p.vertices <= 1);
        assert!(p.deleted_auxiliary <= p.auxiliary && p.deleted_vertices <= p.vertices);
        assert!(p.work_bytes <= 256 || p.oversized_body_bytes.is_some());
        assert!(p.transaction_latency.is_some());
        if p.edges > 0 {
            edge_steps += 1;
        }
        total_edges += p.edges;
        reclaimed += p.reclaimed_estimate;
        body += p.deleted_body_bytes;
        oversize |= p.oversized_body_bytes.is_some_and(|n| n >= 32768);
        exact_physical(&repo, &join.conversation_id);
        if p.complete {
            complete = true;
            break;
        }
    }
    assert!(complete && oversize);
    assert_eq!(total_edges, 60);
    assert_eq!(edge_steps, 60);
    assert!(reclaimed > 32768 && body > 32768);
    assert_eq!(repo.body_fetches.load(Ordering::Relaxed), 0);
    assert_eq!(repo.body_decodes.load(Ordering::Relaxed), 0);
}

#[test]
fn retirement_begin_unknown_empty_and_existing_are_constant_authority_operations() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let conv = ConversationId::new([1; 32]);
    for _ in 0..20 {
        assert!(matches!(
            repo.begin_retirement(&conv).unwrap(),
            RetirementBegin::Absent
        ));
        assert!(repo.resume_retirement(&conv).unwrap().is_none());
    }
    assert_eq!(repo.conversations_page(None, pb()).unwrap().items.len(), 0);
    repo.database
        .read(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM event_conversations", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM event_retirement_jobs", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repo.append(&e).unwrap();
    let t = token(&repo, &conv);
    for _ in 0..20 {
        let t2 = token(&repo, &conv);
        assert_eq!((t.epoch, t.revision), (t2.epoch, t2.revision));
    }
    while !repo
        .reclaim_retirement(&t, RetirementBudget::default())
        .unwrap()
        .complete
    {}
    assert!(matches!(
        repo.begin_retirement(&conv).unwrap(),
        RetirementBegin::Absent
    ));
    assert!(repo.reclaim_retirement(&t, small()).unwrap().complete);
    let mut wrong = t.clone();
    wrong.store_uuid[0] ^= 1;
    stale!(repo.reclaim_retirement(&wrong, small()));
    wrong = t.clone();
    wrong.domain.0[0] ^= 1;
    stale!(repo.reclaim_retirement(&wrong, small()));
    wrong = t.clone();
    wrong.conversation = ConversationId::new([3; 32]);
    stale!(repo.reclaim_retirement(&wrong, small()));
    wrong = t.clone();
    wrong.epoch += 1;
    stale!(repo.reclaim_retirement(&wrong, small()));
    repo.append(&e).unwrap();
    repo.drop_conversation(&conv).unwrap();
    stale!(repo.reclaim_retirement(&t, small()));
}

fn snapshot(repo: &EventRepository) -> Vec<rusqlite::types::Value> {
    repo.database.read(|c|{
        let mut result=Vec::new();
        let mut stmt=c.prepare("SELECT phase,vertex,vertex_commit,auxiliary,edges,vertices,work_bytes,reclaimed_estimate,deleted_body_bytes FROM event_retirement_jobs")?;
        let mut rows=stmt.query([])?;if let Some(r)=rows.next()? {for i in 0..9 {result.push(r.get(i)?);}}
        for table in ["retained_events","retained_event_parents","event_heads","event_author_max","event_order_work","event_order_rows","event_profile_candidates","event_profile_leaves"] {result.push(rusqlite::types::Value::Integer(c.query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get(0))?));}
        Ok(result)
    }).unwrap()
}
fn profile_staged(repo: &EventRepository, conv: &ConversationId) {
    repo.request_profile_maintenance(conv).unwrap();
    let p = repo.begin_profile_maintenance().unwrap();
    for _ in 0..200 {
        repo.step_profile_maintenance(
            p,
            PageBudget {
                rows: 1,
                bytes: 512,
            },
        )
        .unwrap();
        let staged=repo.database.read(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM event_profile_jobs j WHERE j.domain=?1 AND j.conversation=?2 AND EXISTS(SELECT 1 FROM event_profile_candidates x WHERE x.job=j.job AND x.conversation=?2))",params![repo.domain.0.as_slice(),conv.as_bytes().as_slice()],|r|r.get::<_,bool>(0))?)).unwrap();
        if staged {
            return;
        }
    }
    panic!("profile candidate not staged")
}
#[test]
fn retirement_rolls_back_each_phase_and_reopens_at_the_last_committed_prefix() {
    for phase in 0..=10 {
        let dir = tempfile::tempdir().unwrap();
        let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let id = DeviceIdentity::generate();
        let conv = ConversationId::new([1; 32]);
        let mut parent = None;
        for seq in 1..=8 {
            let e = event(&id, seq, vec![], seq, EventKind::Profile);
            repo.append(&e).unwrap();
            parent = Some(e.id);
        }
        repo.append(&event(&id, 9, vec![parent.unwrap()], 0, EventKind::Message))
            .unwrap();
        ready(&repo, &conv);
        profile_staged(&repo, &conv);
        let t = token(&repo, &conv);
        let mut hit = false;
        for _ in 0..300 {
            let before = snapshot(&repo);
            CUT.with(|v| v.set(Some(phase)));
            let result = repo.reclaim_retirement(&t, small());
            CUT.with(|v| v.set(None));
            if result.is_err() {
                assert_eq!(
                    snapshot(&repo),
                    before,
                    "phase {phase} changed committed prefix"
                );
                exact_physical(&repo, &conv);
                hit = true;
                break;
            }
            assert!(!result.unwrap().complete, "cut phase {phase} never reached");
        }
        assert!(hit, "missing injected phase {phase}");
        drop(t);
        drop(repo);
        let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let t = repo.resume_retirement(&conv).unwrap().unwrap();
        let mut done = false;
        for _ in 0..300 {
            let p = repo.reclaim_retirement(&t, small()).unwrap();
            exact_physical(&repo, &conv);
            if p.complete {
                done = true;
                break;
            }
        }
        assert!(done, "phase {phase} did not resume");
    }
}
#[test]
fn retirement_global_profile_cleanup_preserves_active_candidates_and_stales_old_workers() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let id = DeviceIdentity::generate();
    let active = ConversationId::new([0; 32]);
    let retiring = ConversationId::new([1; 32]);
    for conv in [active, retiring] {
        for seq in 1..=12 {
            repo.append(&Event::new(
                &id,
                conv,
                seq,
                vec![],
                seq,
                seq,
                EventKind::Profile,
                vec![8; 128],
            ))
            .unwrap();
        }
        repo.request_profile_maintenance(&conv).unwrap();
    }
    profile_staged(&repo, &retiring);
    let old = repo.begin_profile_maintenance().unwrap();
    let active_candidates = || {
        repo.database
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM event_profile_candidates WHERE conversation=?1",
                    [active.as_bytes().as_slice()],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap()
    };
    let old_candidates = active_candidates();
    assert_eq!(old_candidates, 10);
    let active_before = repo.metadata(&active).unwrap();
    ready(&repo, &retiring);
    let t = token(&repo, &retiring);
    for _ in 0..100 {
        let p = repo.reclaim_retirement(&t, small()).unwrap();
        assert_eq!(
            repo.step_profile_maintenance(old, pb()).unwrap().state,
            MaintenanceState::Stale
        );
        assert_eq!(
            repo.apply_profile_maintenance(old).unwrap().state,
            MaintenanceState::Stale
        );
        if p.phase >= 3 {
            break;
        }
    }
    assert_eq!(active_candidates(), old_candidates);
    assert_eq!(repo.metadata(&active).unwrap(), active_before);
    let fresh = repo.begin_profile_maintenance().unwrap();
    let mut ready = false;
    for _ in 0..1000 {
        if repo
            .step_profile_maintenance(
                fresh,
                PageBudget {
                    rows: 1,
                    bytes: 512,
                },
            )
            .unwrap()
            .state
            == MaintenanceState::Ready
        {
            ready = true;
            break;
        }
    }
    assert!(ready);
    assert_eq!(repo.apply_profile_maintenance(fresh).unwrap().dropped, 10);
    exact_physical(&repo, &retiring);
    for _ in 0..1000 {
        let p = repo.reclaim_retirement(&t, small()).unwrap();
        exact_physical(&repo, &retiring);
        if p.complete {
            return;
        }
    }
    panic!("profile race prevented retirement completion")
}
#[test]
fn retirement_corrupt_or_missing_progress_never_becomes_logical_absence() {
    for mutation in [
        "DELETE FROM event_retirement_jobs",
        "UPDATE event_retirement_jobs SET store_uuid=zeroblob(16)",
        "UPDATE event_retirement_jobs SET epoch=epoch+1",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let e = event(
            &DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repo.append(&e).unwrap();
        let t = token(&repo, &e.conversation_id);
        repo.database
            .transaction(|c| {
                c.execute(mutation, [])?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            repo.resume_retirement(&e.conversation_id),
            Err(LogError::CorruptFile(_))
        ));
        assert!(matches!(
            repo.count(&e.conversation_id),
            Err(LogError::CorruptFile(_))
        ));
        assert!(matches!(repo.get(&e.id), Err(LogError::CorruptFile(_))));
        assert!(matches!(
            repo.reclaim_retirement(&t, small()),
            Err(LogError::CorruptFile(_))
        ));
    }
}
#[test]
fn retirement_active_partial_index_bounds_sqlite_work_among_many_hidden_scopes() {
    use rusqlite::StatementStatus;
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let conv = ConversationId::new([255; 32]);
    repo.append(&Event::new(
        &DeviceIdentity::generate(),
        conv,
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![1],
    ))
    .unwrap();
    repo.database.transaction(|c|{
        // Metadata-only adversarial selector fixture. It deliberately places the
        // live scope after all hidden index keys, so LIMIT alone cannot save a scan.
        c.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT INTO event_conversations(domain,conversation,lifecycle,revision,event_count,stored_bytes,max_lamport,v2_count,v3_count,v4_count) SELECT ?1,cast(printf('%032d',x) AS BLOB),1,1,1,1,zeroblob(8),0,0,0 FROM n",[repo.domain.0.as_slice()])?;
        Ok(())
    }).unwrap();
    repo.database.read(|c|{
        let sql="SELECT conversation FROM event_conversations INDEXED BY event_conversations_active WHERE domain=?1 AND lifecycle=0 AND event_count>0 ORDER BY conversation LIMIT 2";
        let mut stmt=c.prepare(sql)?;let values=stmt.query_map([repo.domain.0.as_slice()],|r|r.get::<_,Vec<u8>>(0))?.collect::<Result<Vec<_>,_>>()?;assert_eq!(values,vec![conv.as_bytes().to_vec()]);
        let steps=stmt.get_status(StatementStatus::VmStep);println!("retirement active selector: 100000 hidden scopes, {steps} VM steps");assert!(steps<100);assert_eq!(stmt.get_status(StatementStatus::Sort),0);assert_eq!(stmt.get_status(StatementStatus::FullscanStep),0);
        for sql in ["EXPLAIN QUERY PLAN SELECT id FROM retained_events WHERE domain=zeroblob(16) AND conversation=zeroblob(32) ORDER BY commit_ordinal DESC,id DESC LIMIT 1","EXPLAIN QUERY PLAN SELECT parent FROM retained_event_parents WHERE domain=zeroblob(16) AND child=zeroblob(32) ORDER BY parent LIMIT 1","EXPLAIN QUERY PLAN DELETE FROM retained_events WHERE domain=zeroblob(16) AND id=zeroblob(32)"] {
            let mut stmt=c.prepare(sql)?;let mut rows=stmt.query([])?;while let Some(r)=rows.next()?{let detail:String=r.get(3)?;assert!(!detail.contains("SCAN ")&&!detail.contains("TEMP"),"{detail}");}
        }
        Ok(())
    }).unwrap();
    assert_eq!(
        repo.conversations_page(None, pb()).unwrap().items[0].conversation,
        conv
    );
}

#[test]
fn retirement_list_rejects_selected_active_scope_with_conflicting_job_authority() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repo.append(&e).unwrap();
    let _t = token(&repo, &e.conversation_id);
    repo.database
        .transaction(|c| {
            c.execute("UPDATE event_conversations SET lifecycle=0", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        matches!(
            repo.conversations_page(None, pb()),
            Err(LogError::CorruptFile(_))
        ),
        "selected active scope must validate matching authority"
    );
}

#[test]
fn retirement_begin_counter_exhaustion_rolls_back_logical_and_derived_authority() {
    for (table, column) in [
        ("event_conversations", "destructive_epoch"),
        ("event_conversations", "revision"),
        ("event_domains", "source_revision"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let e = event(
            &DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repo.append(&e).unwrap();
        ready(&repo, &e.conversation_id);
        repo.database
            .transaction(|c| {
                c.execute(
                    &format!("UPDATE {table} SET {column}=9223372036854775807"),
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let before = snapshot(&repo);
        assert!(matches!(
            repo.begin_retirement(&e.conversation_id),
            Err(LogError::RepositoryCounterExhausted)
        ));
        assert_eq!(snapshot(&repo), before);
        assert!(repo.has(&e.id).unwrap());
        assert_eq!(repo.count(&e.conversation_id).unwrap(), 1);
    }
}
#[test]
fn retirement_charge_underflow_rolls_back_body_deletion_and_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repo.append(&e).unwrap();
    let t = token(&repo, &e.conversation_id);
    for _ in 0..20 {
        if repo.reclaim_retirement(&t, small()).unwrap().phase == 6 {
            break;
        }
    }
    repo.database
        .transaction(|c| {
            c.execute("UPDATE event_domains SET event_count=0", [])?;
            Ok(())
        })
        .unwrap();
    let before = snapshot(&repo);
    assert!(matches!(
        repo.reclaim_retirement(&t, small()),
        Err(LogError::CorruptFile(_))
    ));
    assert_eq!(snapshot(&repo), before);
    repo.database
        .read(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM retained_events", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn retirement_on_entrypoints_reject_another_database_connection() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let first = EventRepository::new(database(&a), EventDomainId([1; 16])).unwrap();
    let second = EventRepository::new(database(&b), EventDomainId([1; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    first.append(&e).unwrap();
    second.append(&e).unwrap();
    let t = token(&first, &e.conversation_id);
    let before = second.metadata(&e.conversation_id).unwrap();
    stale!(
        second
            .database
            .read(|c| first.resume_retirement_on(c, &e.conversation_id))
    );
    stale!(
        second
            .database
            .transaction(|c| first.begin_retirement_on(c, &e.conversation_id))
    );
    assert_eq!(second.metadata(&e.conversation_id).unwrap(), before);
    let _second_token = token(&second, &e.conversation_id);
    stale!(
        second
            .database
            .transaction(|c| first.reclaim_retirement_on(c, &t, small()))
    );
    assert_eq!(
        second
            .database
            .read(|c| second.conversation_charge_on(c, &e.conversation_id))
            .unwrap()
            .unwrap()
            .count,
        1
    );
}
#[test]
fn retirement_old_page_and_order_cursors_stale_before_corrupt_progress() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let id = DeviceIdentity::generate();
    let a = event(&id, 1, vec![], 1, EventKind::Message);
    let b = event(&id, 2, vec![a.id], 2, EventKind::Message);
    repo.append(&a).unwrap();
    repo.append(&b).unwrap();
    ready(&repo, &a.conversation_id);
    let page = repo
        .headers_page(&a.conversation_id, None, pb())
        .unwrap()
        .next
        .unwrap();
    let order = match repo
        .causal_headers_page(&a.conversation_id, CausalOrder::Logical, None, pb())
        .unwrap()
    {
        OrderReadState::Ready(p) => p.next.unwrap(),
        _ => panic!(),
    };
    let _t = token(&repo, &a.conversation_id);
    repo.database
        .transaction(|c| {
            c.execute("DELETE FROM event_retirement_jobs", [])?;
            Ok(())
        })
        .unwrap();
    stale!(repo.headers_page(&a.conversation_id, Some(&page), pb()));
    stale!(repo.causal_headers_page(&a.conversation_id, CausalOrder::Logical, Some(&order), pb()));
    assert!(matches!(
        repo.headers_page(&a.conversation_id, None, pb()),
        Err(LogError::CorruptFile(_))
    ));
    assert!(matches!(
        repo.causal_headers_page(&a.conversation_id, CausalOrder::Logical, None, pb()),
        Err(LogError::CorruptFile(_))
    ));
    assert_eq!(repo.body_fetches.load(Ordering::Relaxed), 0);
}

#[test]
fn retirement_vertex_metadata_never_reads_body_or_trailing_payload_columns() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    repo.database.read(|c| {
        let body:i64=c.query_row("SELECT cid FROM pragma_table_info('retained_events') WHERE name='body'",[],|r|r.get(0))?;
        let last: i64 = c.query_row("SELECT max(cid) FROM pragma_table_info('retained_events')", [], |r| r.get(0))?;
        assert_eq!(body, last, "all scalar metadata must precede payload");
        let table_root:i64=c.query_row("SELECT rootpage FROM sqlite_master WHERE name='retained_events'",[],|r|r.get(0))?;
        let index_root:i64=c.query_row("SELECT rootpage FROM sqlite_master WHERE name='retained_events_commit'",[],|r|r.get(0))?;
        for (query,covering) in [(super::reclaim::SELECT_VERTEX_METADATA,false),(super::reclaim::SELECT_VERTEX_MAXIMUM,true)] {
            let mut stmt=c.prepare(&format!("EXPLAIN {query}"))?;
            let parameters=if covering {vec![rusqlite::types::Value::Blob(vec![1;16]),rusqlite::types::Value::Blob(vec![1;32])]}else{vec![rusqlite::types::Value::Blob(vec![1;16]),rusqlite::types::Value::Blob(vec![1;32]),rusqlite::types::Value::Blob(vec![2;32])]};
            let instructions=stmt.query_map(rusqlite::params_from_iter(parameters),|r|Ok((r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
            let main_cursors:Vec<_>=instructions.iter().filter(|(op,_,root)|op=="OpenRead"&&*root==table_root).map(|(_,cursor,_)|*cursor).collect();
            if covering {assert!(main_cursors.is_empty(),"commit maximum must use only its covering index");assert!(instructions.iter().any(|(op,_,root)|op=="OpenRead"&&*root==index_root));} else {assert!(!main_cursors.is_empty());}
            for (op,cursor,column) in &instructions {if op=="Column"&&main_cursors.contains(cursor) {assert!(*column<body,"main-table cursor {cursor} reads column {column} at/after body {body}; this can traverse a body-sized overflow chain");}}
        }
        Ok(())
    }).unwrap();
}

#[test]
fn retirement_main_payload_uses_integer_rowid_access_after_small_key_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    repo.database
        .read(|c| {
            let main_root: i64 = c.query_row(
                "SELECT rootpage FROM sqlite_master WHERE name='retained_events'",
                [],
                |r| r.get(0),
            )?;
            let mut stmt = c.prepare(&format!(
                "EXPLAIN {}",
                super::reclaim::SELECT_VERTEX_METADATA
            ))?;
            let ops = stmt
                .query_map(params![vec![1u8; 16], vec![1u8; 32], vec![2u8; 32]], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let main: Vec<_> = ops
                .iter()
                .filter(|(op, _, root, _)| op == "OpenRead" && *root == main_root)
                .map(|(_, cursor, _, _)| *cursor)
                .collect();
            assert_eq!(main.len(), 1);
            assert!(
                !ops.iter().any(|(op, cursor, _, _)| main.contains(cursor)
                    && matches!(
                        op.as_str(),
                        "SeekGE"
                            | "SeekGT"
                            | "SeekLE"
                            | "SeekLT"
                            | "IdxGE"
                            | "IdxGT"
                            | "IdxLE"
                            | "IdxLT"
                    )),
                "large payload table must never be sought as an index record"
            );
            assert!(
                ops.iter().any(|(op, cursor, _, table)| op == "DeferredSeek"
                    && !main.contains(cursor)
                    && main.contains(table)
                    || matches!(op.as_str(), "SeekRowid" | "NotExists") && main.contains(cursor)),
                "main payload lookup must use integer rowid/deferred seek"
            );
            assert!(
                ops.iter()
                    .any(|(op, cursor, _, _)| op == "SeekGE" && !main.contains(cursor)),
                "explicit composite PK remains a small secondary index"
            );
            Ok(())
        })
        .unwrap();
}
fn metadata_cache_misses(body_bytes: usize) -> i32 {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let e = Event::new(
        &DeviceIdentity::generate(),
        ConversationId::new([1; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![7; body_bytes],
    );
    repo.append(&e).unwrap();
    repo.database
        .read(|c| {
            let mut stmt = c.prepare(super::reclaim::SELECT_VERTEX_METADATA)?;
            let columns = stmt.column_count();
            c.execute_batch("PRAGMA shrink_memory")?;
            let misses = |reset: bool| {
                let mut current = 0;
                let mut high = 0;
                // SAFETY: c and its SQLite handle remain borrowed under the owner's
                // connection mutex. SQLite writes only these two stack integers.
                let rc = unsafe {
                    rusqlite::ffi::sqlite3_db_status(
                        c.handle(),
                        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
                        &mut current,
                        &mut high,
                        i32::from(reset),
                    )
                };
                assert_eq!(rc, rusqlite::ffi::SQLITE_OK);
                current
            };
            misses(true);
            let mut rows = stmt.query(params![
                repo.domain.0.as_slice(),
                e.conversation_id.as_bytes().as_slice(),
                e.id.as_bytes().as_slice()
            ])?;
            let row = rows.next()?.unwrap();
            for col in 0..columns {
                let _: i64 = row.get(col)?;
            }
            drop(rows);
            Ok(misses(false))
        })
        .unwrap()
}
#[test]
fn retirement_cold_metadata_reads_do_not_scale_with_overflow_body_size() {
    let small = metadata_cache_misses(128);
    let large = metadata_cache_misses(8 * 1024 * 1024);
    println!("cold SQLCipher metadata cache misses: small={small}, 8MiB={large}");
    assert!(
        large <= small + 8 && large < 32,
        "metadata seeks must not traverse the overflow body"
    );
}

#[test]
fn retirement_old_reclamation_token_stales_before_new_incarnation_progress_is_parsed() {
    for mutation in [
        "DELETE FROM event_retirement_jobs WHERE domain=?1 AND conversation=?2",
        "UPDATE event_retirement_jobs SET store_uuid=zeroblob(16) WHERE domain=?1 AND conversation=?2",
        "UPDATE event_retirement_jobs SET epoch=epoch+1 WHERE domain=?1 AND conversation=?2",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let e = event(
            &DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repo.append(&e).unwrap();
        let first = token(&repo, &e.conversation_id);
        let mut complete = false;
        for _ in 0..100 {
            if repo
                .reclaim_retirement(&first, RetirementBudget::default())
                .unwrap()
                .complete
            {
                complete = true;
                break;
            }
        }
        assert!(complete);
        assert!(repo.reclaim_retirement(&first, small()).unwrap().complete);
        repo.append(&e).unwrap();
        let current = token(&repo, &e.conversation_id);
        assert_ne!(
            (first.epoch, first.revision),
            (current.epoch, current.revision)
        );
        repo.database
            .transaction(|c| {
                c.execute(
                    mutation,
                    params![
                        repo.domain.0.as_slice(),
                        e.conversation_id.as_bytes().as_slice()
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        let before = snapshot(&repo);
        let result = repo.reclaim_retirement(&first, small());
        assert!(
            matches!(result, Err(LogError::StaleCursor)),
            "old token interpreted new progress: {result:?}"
        );
        assert_eq!(snapshot(&repo), before);
        assert!(
            matches!(
                repo.reclaim_retirement(&current, small()),
                Err(LogError::CorruptFile(_))
            ),
            "current token must report broken authority"
        );
        assert_eq!(snapshot(&repo), before);
    }
}
