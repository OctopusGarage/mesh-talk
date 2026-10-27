use super::*;
use crate::eventlog::repository::tests::{database, event};
use crate::identity::device::DeviceIdentity;
fn repo() -> (tempfile::TempDir, EventRepository) {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    (dir, repository)
}
fn ids_budget() -> IdPageBudget {
    IdPageBudget {
        ids: 1,
        examined: 1,
    }
}
fn export_budget() -> ExportPageBudget {
    ExportPageBudget {
        examined: 1,
        events: 4,
        encoded_bytes: 65467,
        allocated_bytes: 4 * 1024 * 1024,
    }
}
#[test]
fn network_snapshot_finishes_despite_appends_after_every_page() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let root = event(&signer, 1, vec![], 10, EventKind::Message);
    repository.append(&root).unwrap();
    let second = event(&signer, 2, vec![root.id], 1, EventKind::Message);
    repository.append(&second).unwrap();
    let view = repository.open_view(&root.conversation_id).unwrap();
    assert_eq!(
        NetworkSyncStore::metadata(&repository, &view)
            .unwrap()
            .count,
        2
    );
    let mut after = None;
    let mut ids = vec![];
    for seq in 3..=5 {
        let page = repository
            .have_page(&view, after.as_ref(), ids_budget())
            .unwrap();
        ids.extend(page.items);
        let extra = event(&signer, seq, vec![second.id], seq, EventKind::Message);
        repository.append(&extra).unwrap();
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert!(after.is_none());
    assert_eq!(ids, [root.id, second.id]);
    assert_eq!(
        NetworkSyncStore::metadata(&repository, &view)
            .unwrap()
            .count,
        2
    );
    let fresh = repository.open_view(&root.conversation_id).unwrap();
    assert_eq!(
        NetworkSyncStore::metadata(&repository, &fresh)
            .unwrap()
            .count,
        4
    );
}
#[test]
fn known_prefix_reads_no_headers_or_bodies_and_empty_continuation_advances() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let root = event(&signer, 1, vec![], 10, EventKind::Message);
    let child = event(&signer, 2, vec![root.id], 1, EventKind::Message);
    repository.append(&root).unwrap();
    repository.append(&child).unwrap();
    let view = repository.open_view(&root.conversation_id).unwrap();
    let known = HashSet::from([root.id]);
    let page = repository
        .export_page(&view, &known, None, export_budget())
        .unwrap();
    assert!(page.events.is_empty());
    assert!(page.next.is_some());
    assert_eq!(page.examined, 1);
    assert_eq!(
        repository
            .network_header_fetches
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        repository
            .body_fetches
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let page = repository
        .export_page(&view, &known, page.next.as_ref(), export_budget())
        .unwrap();
    assert_eq!(page.events, [child]);
    assert!(page.next.is_none());
    assert_eq!(
        repository
            .network_header_fetches
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        repository
            .body_fetches
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
#[test]
fn drop_recreate_and_wrong_context_invalidate_network_view() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let root = event(&signer, 1, vec![], 1, EventKind::Message);
    repository.append(&root).unwrap();
    let view = repository.open_view(&root.conversation_id).unwrap();
    let other = EventRepository::new(database(&_dir), EventDomainId([2; 16])).unwrap();
    assert!(matches!(
        other.have_page(&view, None, ids_budget()),
        Err(LogError::StaleCursor)
    ));
    repository.drop_conversation(&root.conversation_id).unwrap();
    repository.append(&root).unwrap();
    assert!(matches!(
        repository.have_page(&view, None, ids_budget()),
        Err(LogError::StaleCursor)
    ));
}
#[test]
fn network_metadata_only_peeks_current_fingerprint() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let root = event(&signer, 1, vec![], 1, EventKind::Message);
    repository.append(&root).unwrap();
    let view = repository.open_view(&root.conversation_id).unwrap();
    assert!(
        NetworkSyncStore::metadata(&repository, &view)
            .unwrap()
            .fingerprint
            .is_none()
    );
    assert_eq!(
        repository
            .fingerprint_ids
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let (expected, _) = repository
        .fingerprint_and_len(&root.conversation_id)
        .unwrap();
    let cached = repository.open_view(&root.conversation_id).unwrap();
    assert_eq!(
        NetworkSyncStore::metadata(&repository, &cached)
            .unwrap()
            .fingerprint,
        Some(expected)
    );
    let second = event(&signer, 2, vec![root.id], 2, EventKind::Message);
    repository.append(&second).unwrap();
    assert_eq!(
        NetworkSyncStore::metadata(&repository, &cached)
            .unwrap()
            .fingerprint,
        Some(expected)
    );
    let fresh = repository.open_view(&root.conversation_id).unwrap();
    assert!(
        NetworkSyncStore::metadata(&repository, &fresh)
            .unwrap()
            .fingerprint
            .is_none()
    );
}
#[test]
fn commit_export_is_topological_even_when_canonical_root_priority_disagrees() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let a = event(&signer, 1, vec![], 100, EventKind::Message);
    let b = event(&signer, 2, vec![], 2, EventKind::Message);
    let child = event(&signer, 3, vec![a.id, b.id], 0, EventKind::Message);
    for event in [&a, &b, &child] {
        repository.append(event).unwrap();
    }
    let mut oracle = crate::eventlog::EventLog::default();
    for event in [&a, &b, &child] {
        oracle.append(event.clone()).unwrap();
    }
    assert_eq!(
        crate::eventlog::sync::SyncStore::events_excluding(
            &oracle,
            &a.conversation_id,
            &HashSet::new()
        )
        .unwrap()[0]
            .id,
        b.id
    );
    for known in [
        HashSet::new(),
        HashSet::from([a.id]),
        HashSet::from([a.id, b.id]),
    ] {
        let view = repository.open_view(&a.conversation_id).unwrap();
        let mut after = None;
        let mut accepted = known.clone();
        let mut exported = vec![];
        loop {
            let page = repository
                .export_page(&view, &known, after.as_ref(), export_budget())
                .unwrap();
            for event in page.events {
                assert!(event.parents.iter().all(|id| accepted.contains(id)));
                accepted.insert(event.id);
                exported.push(event.id);
            }
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        assert_eq!(
            exported,
            [a.id, b.id, child.id]
                .into_iter()
                .filter(|id| !known.contains(id))
                .collect::<Vec<_>>()
        );
    }
}
#[test]
fn network_cursor_binding_and_budget_preflight_precede_body_fetch() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let first = event(&signer, 1, vec![], 1, EventKind::Message);
    let second = event(&signer, 2, vec![], 2, EventKind::Message);
    repository.append(&first).unwrap();
    repository.append(&second).unwrap();
    let view = repository.open_view(&first.conversation_id).unwrap();
    let cursor = repository
        .have_page(&view, None, ids_budget())
        .unwrap()
        .next
        .unwrap();
    let empty = repository.open_view(&ConversationId::new([9; 32])).unwrap();
    assert!(matches!(
        repository.have_page(&empty, Some(&cursor), ids_budget()),
        Err(LogError::StaleCursor)
    ));
    let another_dir = tempfile::tempdir().unwrap();
    let another = EventRepository::new(database(&another_dir), EventDomainId([1; 16])).unwrap();
    assert!(matches!(
        another.have_page(&view, Some(&cursor), ids_budget()),
        Err(LogError::StaleCursor)
    ));
    for budget in [
        ExportPageBudget {
            encoded_bytes: 1,
            ..export_budget()
        },
        ExportPageBudget {
            allocated_bytes: 1,
            ..export_budget()
        },
    ] {
        assert!(matches!(
            repository.export_page(&view, &HashSet::new(), None, budget),
            Err(LogError::PageBudgetTooSmall { .. })
        ));
    }
    assert_eq!(
        repository
            .body_fetches
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let mut budget = export_budget();
    budget.events = 1;
    budget.examined = 8;
    let page = repository
        .export_page(&view, &HashSet::new(), None, budget)
        .unwrap();
    assert_eq!(page.events, [first]);
    assert!(page.next.is_some());
    assert_eq!(
        repository
            .body_fetches
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
#[test]
fn physical_profile_leaf_removal_invalidates_open_network_view() {
    use crate::eventlog::repository::maintenance::MaintenanceState;
    use crate::eventlog::repository::pages::PageBudget;
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let conv = ConversationId::new([1; 32]);
    for seq in 1..=12 {
        repository
            .append(&event(&signer, seq, vec![], seq, EventKind::Profile))
            .unwrap();
    }
    let view = repository.open_view(&conv).unwrap();
    repository.request_profile_maintenance(&conv).unwrap();
    let token = repository.begin_profile_maintenance().unwrap();
    loop {
        if repository
            .step_profile_maintenance(
                token,
                PageBudget {
                    rows: 2,
                    bytes: 1024,
                },
            )
            .unwrap()
            .state
            == MaintenanceState::Ready
        {
            break;
        }
    }
    assert!(repository.apply_profile_maintenance(token).unwrap().dropped > 0);
    assert!(matches!(
        repository.validate_view(&view),
        Err(LogError::StaleCursor)
    ));
}
#[test]
fn destructive_epoch_exhaustion_rolls_back_deletion() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let root = event(&signer, 1, vec![], 1, EventKind::Message);
    repository.append(&root).unwrap();
    repository
        .database
        .transaction(|c| {
            c.execute(
                "UPDATE event_conversations SET destructive_epoch=?1",
                [i64::MAX],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        repository.drop_conversation(&root.conversation_id),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert!(repository.has(&root.id).unwrap());
}
#[test]
fn covering_commit_selector_work_and_active_page_stay_bounded_at_10k_and_100k() {
    use rusqlite::StatementStatus;
    for count in [10_000i64, 100_000] {
        let (_dir, repository) = repo();
        let signer = DeviceIdentity::generate();
        let root = event(&signer, 0, vec![], 0, EventKind::Message);
        repository.append(&root).unwrap();
        // Trusted index fixture: synthetic rows are deliberately never decoded.
        // This measures the actual selector without 100k redundant crypto operations.
        let mut known = HashSet::from([root.id]);
        repository.database.transaction(|c|{
            let mut stmt=c.prepare("INSERT INTO retained_events SELECT domain,?1,conversation,author,?2,lamport,wall_clock,kind,parent_count,encoded_bytes,ciphertext_bytes,signature_bytes,stored_bytes,body,?3 FROM retained_events WHERE domain=?4 AND id=?5")?;
            for ordinal in 1..count {
                let mut id=[0;32];id[..8].copy_from_slice(&ordinal.to_be_bytes());
                stmt.execute(params![id.as_slice(),(ordinal as u64).to_be_bytes().as_slice(),ordinal,repository.domain.0.as_slice(),root.id.as_bytes().as_slice()])?;known.insert(EventId::new(id));
            }
            c.execute("UPDATE event_conversations SET event_count=?1",[count])?;c.execute("UPDATE event_domains SET next_commit=?1",[count])?;Ok(())
        }).unwrap();
        let view = repository.open_view(&root.conversation_id).unwrap();
        let after = count / 2;
        let cursor = ExportCursor {
            binding: view.binding,
            after,
            id: [0; 32],
        };
        let page = repository
            .export_page(
                &view,
                &known,
                Some(&cursor),
                ExportPageBudget {
                    examined: 64,
                    ..export_budget()
                },
            )
            .unwrap();
        assert_eq!(page.examined, 64);
        assert!(page.events.is_empty());
        assert!(page.next.is_some());
        assert_eq!(
            repository
                .network_header_fetches
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(
            repository
                .body_fetches
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        repository.database.read(|c|{
            let parameters=params![repository.domain.0.as_slice(),root.conversation_id.as_bytes().as_slice(),after,count,after,[0u8;32].as_slice(),65];
            let plan={let mut stmt=c.prepare(&format!("EXPLAIN QUERY PLAN {SELECT_COMMIT_IDS}"))?;let rows=stmt.query_map(parameters,|r|r.get::<_,String>(3))?;rows.collect::<Result<Vec<_>,_>>()?};
            assert!(plan.iter().any(|line|line.contains("COVERING INDEX retained_events_commit")),"{plan:?}");
            assert!(!plan.iter().any(|line|line.contains("TEMP")),"{plan:?}");
            let mut stmt=c.prepare(SELECT_COMMIT_IDS)?;{let mut rows=stmt.query(parameters)?;while rows.next()?.is_some(){}}
            let steps=stmt.get_status(StatementStatus::VmStep);assert!(steps<2500,"count={count}, steps={steps}");assert_eq!(stmt.get_status(StatementStatus::Sort),0);
            eprintln!("network count={count} plan={plan:?} vm_steps={steps}; selected={} headers=0 bodies=0",page.events.len());Ok(())
        }).unwrap();
    }
}
#[test]
fn reordered_legacy_import_exports_causally_and_view_retains_profile_lease() {
    use crate::storage::{
        profile::{ProfileStorageLease, context::ProfileStorage},
        record_log::EncryptedRecordLog,
    };
    let dir = tempfile::tempdir().unwrap();
    let signer = DeviceIdentity::generate();
    let path = dir.path().join("custom.events");
    let a = event(&signer, 1, vec![], 100, EventKind::Message);
    let b = event(&signer, 2, vec![], 2, EventKind::Message);
    let child = event(&signer, 3, vec![a.id, b.id], 0, EventKind::Message);
    let (mut legacy, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
    for event in [&child, &a, &b, &child] {
        legacy.append_durable(event).unwrap();
    }
    drop(legacy);
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(dir.path()).unwrap(),
        &signer.public(),
        &path,
        "pw",
        12,
    )
    .unwrap();
    let repository = profile.events().unwrap();
    let view = repository.open_view(&a.conversation_id).unwrap();
    let mut after = None;
    let mut known = HashSet::new();
    let mut ids = vec![];
    loop {
        let page = repository
            .export_page(&view, &HashSet::new(), after.as_ref(), export_budget())
            .unwrap();
        for event in page.events {
            assert!(event.parents.iter().all(|id| known.contains(id)));
            known.insert(event.id);
            ids.push(event.id);
        }
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(ids, [a.id, b.id, child.id]);
    drop(repository);
    drop(profile);
    assert!(ProfileStorageLease::acquire(dir.path()).is_err());
    drop(view);
    assert!(ProfileStorageLease::acquire(dir.path()).is_ok());
}
#[test]
fn export_cutoff_excludes_appended_messages_and_finishes() {
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let root = event(&signer, 1, vec![], 10, EventKind::Message);
    let child = event(&signer, 2, vec![root.id], 0, EventKind::Message);
    for event in [&root, &child] {
        repository.append(event).unwrap();
    }
    let view = repository.open_view(&root.conversation_id).unwrap();
    let mut after = None;
    let mut selected = vec![];
    for seq in 3..=5 {
        let page = repository
            .export_page(&view, &HashSet::new(), after.as_ref(), export_budget())
            .unwrap();
        selected.extend(page.events.into_iter().map(|e| e.id));
        after = page.next;
        repository
            .append(&event(
                &signer,
                seq,
                vec![child.id],
                seq,
                EventKind::Message,
            ))
            .unwrap();
        if after.is_none() {
            break;
        }
    }
    assert!(after.is_none());
    assert_eq!(selected, [root.id, child.id]);
    assert!(
        !NetworkSyncStore::metadata(&repository, &view)
            .unwrap()
            .channel_v4
    );
    let fresh = repository.open_view(&root.conversation_id).unwrap();
    assert_eq!(
        NetworkSyncStore::metadata(&repository, &fresh)
            .unwrap()
            .count,
        4
    );
}

#[test]
fn first_capability_append_invalidates_captured_network_view() {
    for kind in [
        EventKind::ChannelReplica,
        EventKind::ChannelHistoryPolicy,
        EventKind::ChannelAccountAuthority,
    ] {
        let (_dir, repository) = repo();
        let signer = DeviceIdentity::generate();
        let root = event(&signer, 1, vec![], 1, EventKind::Message);
        repository.append(&root).unwrap();
        repository
            .fingerprint_and_len(&root.conversation_id)
            .unwrap();
        let view = repository.open_view(&root.conversation_id).unwrap();
        assert!(
            NetworkSyncStore::metadata(&repository, &view)
                .unwrap()
                .fingerprint
                .is_some()
        );
        repository
            .append(&event(&signer, 2, vec![root.id], 2, kind))
            .unwrap();
        assert!(
            matches!(repository.validate_view(&view), Err(LogError::StaleCursor)),
            "{kind:?}: current protocol requirement must invalidate old-view handoff"
        );
        assert!(matches!(
            NetworkSyncStore::metadata(&repository, &view),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(
            repository.have_page(&view, None, ids_budget()),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(
            repository.export_page(&view, &HashSet::new(), None, export_budget()),
            Err(LogError::StaleCursor)
        ));
        let fresh = repository.open_view(&root.conversation_id).unwrap();
        assert_eq!(
            NetworkSyncStore::metadata(&repository, &fresh)
                .unwrap()
                .count,
            2
        );
    }
    let (_dir, repository) = repo();
    let signer = DeviceIdentity::generate();
    let first = event(&signer, 1, vec![], 1, EventKind::ChannelAccountAuthority);
    repository.append(&first).unwrap();
    let view = repository.open_view(&first.conversation_id).unwrap();
    repository
        .append(&event(
            &signer,
            2,
            vec![first.id],
            2,
            EventKind::ChannelReplica,
        ))
        .unwrap();
    assert!(
        repository.validate_view(&view).is_ok(),
        "v4 already supports lower protocol controls"
    );
}
