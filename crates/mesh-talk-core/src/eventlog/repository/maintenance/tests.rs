use super::*;
use crate::eventlog::repository::tests::{database, event};
use crate::identity::device::DeviceIdentity;

#[test]
fn profile_work_observation_reports_stale_selection_without_writing_or_reading_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    for sequence in 1..=12 {
        repo.append(&event(
            &author,
            sequence,
            vec![],
            sequence,
            EventKind::Profile,
        ))
        .unwrap();
    }
    repo.request_profile_maintenance(&ConversationId::new([1; 32]))
        .unwrap();
    let absent = repo.observe_profile_work().unwrap();
    assert!(absent.requests_pending);
    assert!(absent.job.is_none());
    let token = repo.begin_profile_maintenance().unwrap();
    repo.step_profile_maintenance(
        token,
        PageBudget {
            rows: 2,
            bytes: 1024,
        },
    )
    .unwrap();
    let selected = repo.observe_profile_work().unwrap();
    assert_eq!(
        selected.job.as_ref().unwrap().revision,
        selected.source_revision
    );
    repo.append(&event(&author, 100, vec![], 100, EventKind::Message))
        .unwrap();
    let changes = db.read(|c| Ok(c.total_changes())).unwrap();
    let commits = db.successful_write_commits();
    let bodies = repo.body_read_counts();
    let stale = repo.observe_profile_work().unwrap();
    assert!(stale.source_revision > stale.job.as_ref().unwrap().revision);
    assert_eq!(
        stale.job, selected.job,
        "observation must expose, not repair, stale progress"
    );
    assert_eq!(repo.observe_profile_work().unwrap(), stale);
    assert_eq!(db.read(|c| Ok(c.total_changes())).unwrap(), changes);
    assert_eq!(db.successful_write_commits(), commits);
    assert_eq!(repo.body_read_counts(), bodies);
    let fresh = repo.begin_profile_maintenance().unwrap();
    finish(&repo, fresh);
    let complete = repo.observe_profile_work().unwrap();
    assert!(complete.job.as_ref().unwrap().complete);
    assert_eq!(
        complete.job.as_ref().unwrap().revision,
        complete.source_revision
    );
    assert_ne!(
        complete.job.as_ref().unwrap().cursor_tag,
        stale.job.as_ref().unwrap().cursor_tag
    );
}

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
fn completed_negative_profile_snapshot_survives_ordinary_appends_without_rescan() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    for seq in 1..=12 {
        repo.append(&event(&author, seq, vec![], seq, EventKind::Profile))
            .unwrap();
    }
    let ordinary = event(&author, 13, vec![], 13, EventKind::Message);
    repo.append(&ordinary).unwrap();
    let unrelated = Event::new(
        &author,
        ConversationId::new([9; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![0; 64 * 1024],
    );
    repo.append(&unrelated).unwrap();
    repo.request_profile_maintenance(&ordinary.conversation_id)
        .unwrap();
    let mut old = repo.begin_profile_maintenance().unwrap();
    let complete = finish(&repo, old);
    assert_eq!(complete.candidates, 10);
    assert!(complete.candidate_bytes < repo.estimated_bytes().unwrap() / 8);
    assert_eq!(repo.apply_profile_maintenance(old).unwrap().dropped, 0);
    let bodies = repo.body_read_counts();
    for (conv, parent, seq) in [
        (ordinary.conversation_id, ordinary.id, 14),
        (unrelated.conversation_id, unrelated.id, 2),
    ] {
        let before = repo.observe_profile_work().unwrap();
        let appended = Event::new(
            &author,
            conv,
            seq,
            vec![parent],
            seq,
            seq,
            EventKind::Message,
            vec![42; 128],
        );
        repo.append(&appended).unwrap();
        assert_eq!(
            repo.apply_profile_maintenance(old).unwrap().state,
            MaintenanceState::Stale
        );
        let fresh = repo.begin_profile_maintenance().unwrap();
        assert_eq!(fresh.job, old.job);
        let progress = repo
            .step_profile_maintenance(
                fresh,
                PageBudget {
                    rows: 1,
                    bytes: 512,
                },
            )
            .unwrap();
        assert_eq!(progress.state, MaintenanceState::Ready);
        assert_eq!((progress.examined, progress.charged_bytes), (0, 0));
        assert_eq!(
            (progress.candidates, progress.candidate_bytes),
            (complete.candidates, complete.candidate_bytes)
        );
        let after = repo.observe_profile_work().unwrap();
        let before_job = before.job.unwrap();
        let after_job = after.job.unwrap();
        assert_eq!(after_job.revision, after.source_revision);
        assert!(after_job.complete);
        assert_eq!(after_job.cursor_tag, before_job.cursor_tag);
        assert!(after.requests_pending);
        assert_eq!(repo.apply_profile_maintenance(fresh).unwrap().dropped, 0);
        assert_eq!(repo.count(&ordinary.conversation_id).unwrap(), 14);
        old = fresh;
    }
    assert_eq!(repo.body_read_counts(), bodies);
}

fn profile_snapshot_fixture(
    repo: &EventRepository,
    author: &DeviceIdentity,
    byte_negative: bool,
) -> (Vec<Event>, ProfileMaintenanceToken) {
    let profiles: Vec<_> = (1..=12)
        .map(|seq| event(author, seq, vec![], seq, EventKind::Profile))
        .collect();
    for profile in &profiles {
        repo.append(profile).unwrap();
    }
    if byte_negative {
        repo.append(&Event::new(
            author,
            ConversationId::new([9; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![0; 64 * 1024],
        ))
        .unwrap();
    }
    repo.request_profile_maintenance(&profiles[0].conversation_id)
        .unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    (profiles, token)
}

#[test]
fn negative_profile_snapshot_below_count_gate_survives_ordinary_append() {
    let dir = tempfile::tempdir().unwrap();
    let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    for seq in 1..=9 {
        repo.append(&event(&author, seq, vec![], seq, EventKind::Profile))
            .unwrap();
    }
    repo.request_profile_maintenance(&ConversationId::new([1; 32]))
        .unwrap();
    let old = repo.begin_profile_maintenance().unwrap();
    let complete = finish(&repo, old);
    assert_eq!(complete.candidates, 7);
    assert!(complete.candidate_bytes >= repo.estimated_bytes().unwrap().div_ceil(8));
    repo.append(&event(&author, 10, vec![], 10, EventKind::Message))
        .unwrap();
    assert_eq!(
        repo.apply_profile_maintenance(old).unwrap().state,
        MaintenanceState::Stale
    );
    let fresh = repo.begin_profile_maintenance().unwrap();
    let progress = repo
        .step_profile_maintenance(fresh, PageBudget::default())
        .unwrap();
    assert_eq!(progress.state, MaintenanceState::Ready);
    assert_eq!(progress.examined, 0);
    assert_eq!(progress.candidates, 7);
    assert_eq!(repo.apply_profile_maintenance(fresh).unwrap().dropped, 0);
}

#[test]
fn negative_profile_snapshot_invalidates_on_any_profile_parent() {
    for parent_index in [0, 11] {
        let dir = tempfile::tempdir().unwrap();
        let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let author = DeviceIdentity::generate();
        let (profiles, old) = profile_snapshot_fixture(&repo, &author, true);
        assert_eq!(finish(&repo, old).candidates, 10);
        repo.append(&event(
            &author,
            13,
            vec![profiles[parent_index].id],
            13,
            EventKind::Message,
        ))
        .unwrap();
        assert_eq!(
            repo.apply_profile_maintenance(old).unwrap().state,
            MaintenanceState::Stale
        );
        let stale = repo.observe_profile_work().unwrap();
        assert!(stale.job.unwrap().revision < stale.source_revision);
        let fresh = repo.begin_profile_maintenance().unwrap();
        assert_eq!(finish(&repo, fresh).candidates, 9);
        assert_eq!(repo.apply_profile_maintenance(fresh).unwrap().dropped, 0);
        repo.drop_conversation(&ConversationId::new([9; 32]))
            .unwrap();
        let actionable = repo.begin_profile_maintenance().unwrap();
        finish(&repo, actionable);
        assert_eq!(
            repo.apply_profile_maintenance(actionable).unwrap().dropped,
            9
        );
        assert!(repo.has(&profiles[parent_index].id).unwrap());
        // Referencing a protected leaf promotes the previous candidate into
        // the newest-two unreferenced leaves; it must survive the rebuilt job.
        for index in if parent_index == 11 {
            [9, 10]
        } else {
            [10, 11]
        } {
            assert!(repo.has(&profiles[index].id).unwrap());
        }
    }
}

#[test]
fn negative_profile_snapshot_carry_excludes_invalidating_boundaries() {
    for boundary in ["profile", "request", "partial", "stale", "positive"] {
        let dir = tempfile::tempdir().unwrap();
        let repo = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let author = DeviceIdentity::generate();
        let (profiles, old) = profile_snapshot_fixture(&repo, &author, boundary != "positive");
        if boundary != "partial" {
            finish(&repo, old);
        } else {
            repo.step_profile_maintenance(
                old,
                PageBudget {
                    rows: 2,
                    bytes: 1024,
                },
            )
            .unwrap();
        }
        let before = repo.observe_profile_work().unwrap();
        match boundary {
            "profile" | "stale" => {
                repo.append(&event(&author, 13, vec![], 13, EventKind::Profile))
                    .unwrap();
            }
            "request" => {
                repo.request_profile_maintenance(&ConversationId::new([9; 32]))
                    .unwrap();
            }
            _ => {}
        }
        if boundary != "profile" {
            repo.append(&event(&author, 14, vec![], 14, EventKind::Message))
                .unwrap();
        }
        let after = repo.observe_profile_work().unwrap();
        assert_eq!(
            after.job.as_ref().unwrap().revision,
            before.job.unwrap().revision,
            "{boundary}"
        );
        assert!(
            after.job.unwrap().revision < after.source_revision,
            "{boundary}"
        );
        assert_eq!(
            repo.apply_profile_maintenance(old).unwrap().state,
            MaintenanceState::Stale
        );
        assert!(repo.has(&profiles[0].id).unwrap());
    }
}

#[test]
fn negative_profile_snapshot_carry_rolls_back_with_late_append_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let (_, token) = profile_snapshot_fixture(&repo, &author, true);
    finish(&repo, token);
    let before = repo.observe_profile_work().unwrap();
    let charge = repo.estimated_bytes().unwrap();
    db.transaction(|c| {
        // This domain write follows the carry UPDATE. Require the job to have
        // advanced already so the injected failure exercises actual rollback.
        c.execute_batch("CREATE TEMP TRIGGER fail_after_profile_carry BEFORE UPDATE OF next_commit ON event_domains WHEN EXISTS(SELECT 1 FROM event_profile_jobs j WHERE j.domain=NEW.domain AND j.source_revision=NEW.source_revision) BEGIN SELECT RAISE(ABORT,'late append failure after carry'); END;")?;
        Ok(())
    }).unwrap();
    let appended = event(&author, 13, vec![], 13, EventKind::Message);
    assert!(repo.append(&appended).is_err());
    assert_eq!(repo.observe_profile_work().unwrap(), before);
    assert_eq!(repo.estimated_bytes().unwrap(), charge);
    assert!(!repo.has(&appended.id).unwrap());
    assert_eq!(
        repo.step_profile_maintenance(token, PageBudget::default())
            .unwrap()
            .examined,
        0
    );
    db.transaction(|c| {
        c.execute_batch("DROP TRIGGER fail_after_profile_carry")?;
        Ok(())
    })
    .unwrap();
    repo.append(&appended).unwrap();
    let fresh = repo.begin_profile_maintenance().unwrap();
    assert_eq!(
        repo.step_profile_maintenance(fresh, PageBudget::default())
            .unwrap()
            .examined,
        0
    );
}

#[test]
fn negative_profile_snapshot_reopens_and_preserves_exact_token_guards() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let (_, old) = profile_snapshot_fixture(&repo, &author, true);
    finish(&repo, old);
    repo.append(&event(&author, 13, vec![], 13, EventKind::Message))
        .unwrap();
    let fresh = repo.begin_profile_maintenance().unwrap();
    let before = repo.observe_profile_work().unwrap();
    drop(repo);
    drop(db);
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    assert_eq!(repo.observe_profile_work().unwrap(), before);
    assert_eq!(
        repo.step_profile_maintenance(fresh, PageBudget::default())
            .unwrap()
            .examined,
        0
    );
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let other_store = EventRepository::new(database(&other_dir), EventDomainId([1; 16])).unwrap();
    for wrong in [
        old,
        ProfileMaintenanceToken {
            domain: other.domain,
            ..fresh
        },
        ProfileMaintenanceToken {
            store_uuid: other_store.store_uuid,
            ..fresh
        },
        ProfileMaintenanceToken {
            job: [0; 16],
            ..fresh
        },
    ] {
        assert_eq!(
            repo.step_profile_maintenance(wrong, PageBudget::default())
                .unwrap()
                .state,
            MaintenanceState::Stale
        );
        assert_eq!(
            repo.apply_profile_maintenance(wrong).unwrap().state,
            MaintenanceState::Stale
        );
    }
    repo.drop_conversation(&ConversationId::new([9; 32]))
        .unwrap();
    assert_eq!(
        repo.apply_profile_maintenance(fresh).unwrap().state,
        MaintenanceState::Stale
    );
    let rebuilt = repo.begin_profile_maintenance().unwrap();
    assert_eq!(finish(&repo, rebuilt).candidates, 10);
    assert_eq!(repo.apply_profile_maintenance(rebuilt).unwrap().dropped, 10);
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
    repo.request_profile_maintenance(&conv).unwrap();
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
    repo.request_profile_maintenance(&conv).unwrap();
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
        let mut expected: Vec<_> = legacy.events(&conv).unwrap().iter().map(|e| e.id).collect();
        expected.sort();
        assert_eq!(ids, expected);
        assert_eq!(repo.heads(&conv).unwrap(), legacy.heads(&conv).unwrap());
        let (digest, count) = legacy.fingerprint_and_len(&conv).unwrap();
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

// Keep the fixture and assertions at the composition boundary: real eligible
// Profile publication against already-published canonical history generations.
#[test]
fn profile_compaction_defers_ready_order_reclamation_128() {
    profile_compaction_defers_ready_order_reclamation(128);
}

#[test]
fn profile_compaction_defers_ready_order_reclamation_256() {
    profile_compaction_defers_ready_order_reclamation(256);
}

fn profile_compaction_defers_ready_order_reclamation(total: u64) {
    use crate::eventlog::repository::order::{
        CausalOrder, OrderReadState, OrderReadiness, OrderWorkBudget,
    };
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let mut repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    let conv = ConversationId::new([1; 32]);
    let kinds = [CausalOrder::Logical, CausalOrder::WallClock];
    let budget = OrderWorkBudget {
        vertices: 3,
        edges: 2,
        bytes: 384,
    };
    let page_budget = PageBudget {
        rows: 1,
        bytes: 4096,
    };
    let mut events: Vec<Event> = Vec::new();
    let profiles = total / 4;
    for seq in 1..=total {
        let kind = if seq <= profiles {
            EventKind::Profile
        } else {
            EventKind::Message
        };
        let parents = if seq > profiles + 1 {
            vec![events.last().unwrap().id]
        } else {
            vec![]
        };
        // Deliberately disagreeing clocks require both exact canonical orders.
        let e = Event::new(
            &author,
            conv,
            seq,
            parents,
            total - seq,
            seq,
            kind,
            vec![42; 128],
        );
        repo.append(&e).unwrap();
        events.push(e);
    }
    let finish_order = |repo: &EventRepository, kind| {
        for _ in 0..10000 {
            let step = repo.order_step(&conv, kind, budget).unwrap();
            assert!(step.seeded + step.ranked + step.cleaned <= budget.vertices);
            assert!(step.edges <= budget.edges);
            assert!(step.charged_bytes <= budget.bytes);
            if step.readiness == OrderReadiness::Ready {
                return;
            }
        }
        panic!("bounded canonical order failed to converge");
    };
    let mut old = Vec::new();
    for kind in kinds {
        finish_order(&repo, kind);
        let generation = repo.order_generation(&conv, kind).unwrap().unwrap();
        assert_eq!(generation.count, total);
        let OrderReadState::Ready(page) = repo
            .causal_headers_page(&conv, kind, None, page_budget)
            .unwrap()
        else {
            panic!("initial order pending")
        };
        old.push((kind, generation, page.next.unwrap()));
    }
    let derived_rows = |db: &Arc<EncryptedDatabase>, job: &[u8; 16]| {
        db.read(|c| {
            c.query_row("SELECT (SELECT count(*) FROM event_order_rows WHERE job=?1)+(SELECT count(*) FROM event_order_work WHERE job=?1)", [job.as_slice()], |r| r.get::<_, i64>(0)).map_err(LogError::from)
        }).unwrap()
    };
    for (_, generation, _) in &old {
        assert_eq!(derived_rows(&db, &generation.job), (2 * total) as i64);
    }
    repo.request_profile_maintenance(&conv).unwrap();
    let token = repo.begin_profile_maintenance().unwrap();
    assert_eq!(finish(&repo, token).candidates, profiles - 2);
    // A failing source mutation must roll back logical invalidation too.
    db.transaction(|c| {
        c.execute_batch("CREATE TEMP TRIGGER fail_order_invalidation BEFORE DELETE ON retained_events BEGIN SELECT RAISE(ABORT,'injected Profile publication failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(repo.apply_profile_maintenance(token).is_err());
    for (kind, generation, cursor) in &old {
        assert_eq!(
            repo.order_generation(&conv, *kind).unwrap(),
            Some(*generation)
        );
        assert!(matches!(
            repo.causal_headers_page(&conv, *kind, Some(cursor), page_budget)
                .unwrap(),
            OrderReadState::Ready(_)
        ));
        assert_eq!(derived_rows(&db, &generation.job), (2 * total) as i64);
    }
    db.transaction(|c| {
        c.execute_batch("DROP TRIGGER fail_order_invalidation")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        repo.apply_profile_maintenance(token).unwrap().dropped as u64,
        profiles - 2
    );
    for (kind, generation, cursor) in &old {
        assert_eq!(repo.order_generation(&conv, *kind).unwrap(), None);
        assert!(matches!(
            repo.causal_headers_page(&conv, *kind, None, page_budget)
                .unwrap(),
            OrderReadState::Pending
        ));
        assert!(matches!(
            repo.causal_headers_page(&conv, *kind, Some(cursor), page_budget),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(
            repo.causal_headers_after_rank(&conv, *kind, generation.job, 0, page_budget),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(
            derived_rows(&db, &generation.job),
            (2 * total) as i64,
            "Profile publication must invalidate immediately without synchronously draining history-sized rank/work rows"
        );
        db.read(|c| {
            let phase: i64 = c.query_row(
                "SELECT phase FROM event_order_jobs WHERE job=?1",
                [generation.job.as_slice()],
                |r| r.get(0),
            )?;
            assert_eq!(phase, 4, "the preserved generation must be obsolete");
            Ok(())
        })
        .unwrap();
    }
    // Interrupt after one real reclamation transaction. Appends while obsolete
    // must neither republish its ranks nor create another abandoned generation.
    let first = repo
        .order_step(&conv, CausalOrder::Logical, budget)
        .unwrap();
    assert_eq!(
        (
            first.cleaned,
            first.seeded,
            first.ranked,
            first.edges,
            first.charged_bytes
        ),
        (3, 0, 0, 0, 384)
    );
    assert_eq!(derived_rows(&db, &old[0].1.job), (2 * total - 3) as i64);
    let late = Event::new(
        &author,
        conv,
        total + 1,
        vec![events.last().unwrap().id],
        0,
        0,
        EventKind::Message,
        vec![42; 128],
    );
    repo.append(&late).unwrap();
    events.push(late);
    drop(repo);
    drop(db);
    let db = database(&dir);
    repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    assert_eq!(derived_rows(&db, &old[0].1.job), (2 * total - 3) as i64);
    for (kind, generation, cursor) in &old {
        assert_eq!(repo.order_generation(&conv, *kind).unwrap(), None);
        assert!(matches!(
            repo.causal_headers_page(&conv, *kind, Some(cursor), page_budget),
            Err(LogError::StaleCursor)
        ));
        let mut cleaned = if *kind == CausalOrder::Logical { 3 } else { 0 };
        for _ in 0..10000 {
            let before = derived_rows(&db, &generation.job);
            let step = repo.order_step(&conv, *kind, budget).unwrap();
            let after = derived_rows(&db, &generation.job);
            assert!(step.seeded + step.ranked + step.cleaned <= budget.vertices);
            assert!(step.edges <= budget.edges);
            assert!(step.charged_bytes <= budget.bytes);
            assert!(
                (before - after) as usize <= step.cleaned,
                "every removed derived row must be charged"
            );
            cleaned += step.cleaned;
            if step.readiness == OrderReadiness::Ready {
                break;
            }
        }
        assert_eq!(
            cleaned,
            2 * total as usize + 1,
            "both row populations and final obsolete job deletion are charged exactly once"
        );
        assert_eq!(derived_rows(&db, &generation.job), 0);
        let current = repo.order_generation(&conv, *kind).unwrap().unwrap();
        assert_ne!(current.job, generation.job);
        let retained: Vec<_> = events.iter().filter(|e| repo.has(&e.id).unwrap()).collect();
        let expected =
            crate::eventlog::order::ordered_ids(&retained, &[], *kind == CausalOrder::Logical);
        let mut actual = Vec::new();
        let mut cursor = None;
        loop {
            let OrderReadState::Ready(page) = repo
                .causal_headers_page(&conv, *kind, cursor.as_ref(), page_budget)
                .unwrap()
            else {
                panic!("replacement order pending")
            };
            actual.extend(page.items.into_iter().map(|h| h.id));
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            actual, expected,
            "replacement must match the independent Kahn oracle"
        );
        assert_eq!(actual.len() as u64, total - (profiles - 2) + 1);
    }
    assert_eq!(
        repo.body_decodes.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn committed_kind_observation_baselines_then_bounds_pages_without_writes_or_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let author = DeviceIdentity::generate();
    repo.append(&event(&author, 1, vec![], 1, EventKind::Profile))
        .unwrap();
    let baseline = repo.observe_committed_kinds(None).unwrap();
    assert_eq!(baseline.0, 0);
    assert_eq!(
        baseline.1, [0; 20],
        "baseline must not enumerate historical headers"
    );
    for n in 2..=67 {
        repo.append(&event(
            &author,
            n,
            vec![],
            n,
            if n == 67 {
                EventKind::Profile
            } else {
                EventKind::Message
            },
        ))
        .unwrap();
    }
    let changes = db.read(|c| Ok(c.total_changes())).unwrap();
    let commits = db.successful_write_commits();
    let bodies = repo.body_read_counts();
    let page = repo.observe_committed_kinds(Some(baseline.0)).unwrap();
    assert_eq!(page.1[0], 64);
    assert_eq!(page.1[8], 0);
    assert!(page.2);
    let tail = repo.observe_committed_kinds(Some(page.0)).unwrap();
    assert_eq!(tail.1[0], 1);
    assert_eq!(tail.1[8], 1);
    assert!(!tail.2);
    let reset = repo.observe_committed_kinds(Some(999)).unwrap();
    assert!(reset.3);
    assert_eq!(reset.1, [0; 20]);
    assert_eq!(db.read(|c| Ok(c.total_changes())).unwrap(), changes);
    assert_eq!(db.successful_write_commits(), commits);
    assert_eq!(repo.body_read_counts(), bodies);
}
