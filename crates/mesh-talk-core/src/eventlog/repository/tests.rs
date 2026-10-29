use super::pages::PageBudget;
use super::*;
use crate::{
    eventlog::{ConversationId, EventKind},
    identity::device::DeviceIdentity,
};
pub(super) fn event(
    identity: &DeviceIdentity,
    seq: u64,
    parents: Vec<EventId>,
    clock: u64,
    kind: EventKind,
) -> Event {
    Event::new(
        identity,
        ConversationId::new([1; 32]),
        seq,
        parents,
        clock,
        clock,
        kind,
        vec![42; 128],
    )
}
pub(super) fn database(dir: &tempfile::TempDir) -> Arc<EncryptedDatabase> {
    Arc::new(
        EncryptedDatabase::open(&dir.path().join("profile.db"), "password", b"profile").unwrap(),
    )
}
#[test]
fn retains_exact_event_across_reopen_without_cross_domain_leak() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    let e = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    assert_eq!(repository.append(&e).unwrap(), AppendOutcome::Appended);
    assert_eq!(repository.get(&e.id).unwrap(), Some(e.clone()));
    assert!(other.get(&e.id).unwrap().is_none());
    assert_eq!(other.append(&e).unwrap(), AppendOutcome::Appended);
    drop(repository);
    drop(other);
    let reopened = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    assert_eq!(reopened.get(&e.id).unwrap(), Some(e.clone()));
    assert_eq!(reopened.append(&e).unwrap(), AppendOutcome::Duplicate);
}
#[test]
fn invalid_duplicate_and_missing_parent_do_not_mutate() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let e = event(&identity, 1, vec![], 1, EventKind::Message);
    repository.append(&e).unwrap();
    let mut invalid = e.clone();
    invalid.sig[0] ^= 1;
    assert!(matches!(
        repository.append(&invalid),
        Err(LogError::BadSignature)
    ));
    let missing = event(
        &identity,
        2,
        vec![EventId::new([7; 32])],
        2,
        EventKind::Message,
    );
    assert!(matches!(
        repository.append(&missing),
        Err(LogError::MissingParents(_))
    ));
    assert!(repository.get(&missing.id).unwrap().is_none());
}
#[test]
fn component_schema_rejects_future_version_without_adopting_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    db.transaction(|tx| {
        tx.execute_batch(schema::SCHEMA.split(';').next().unwrap())?;
        tx.execute("INSERT INTO event_repository_schema VALUES(1,99)", [])?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        EventRepository::new(db.clone(), EventDomainId([1; 16])),
        Err(LogError::IncompatibleEventSchema)
    ));
    db.read(|c| {
        let count: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='event_domains'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    })
    .unwrap();
}

#[test]
fn metadata_matches_pure_log_for_profile_branch_merge_and_maximum_clocks() {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let root = event(&a, 1, vec![], 7, EventKind::Message);
    let profile = event(&a, 2, vec![root.id], 2, EventKind::Profile);
    let branch = event(&b, 1, vec![root.id], 8, EventKind::ChannelReplica);
    let merge = event(
        &a,
        3,
        vec![profile.id, branch.id],
        1,
        EventKind::ChannelHistoryPolicy,
    );
    let max = event(
        &a,
        u64::MAX,
        vec![merge.id],
        u64::MAX,
        EventKind::ChannelAccountAuthority,
    );
    let mut oracle = super::super::EventLog::default();
    for e in [root, profile, branch, merge, max] {
        oracle.append(e.clone()).unwrap();
        repository.append(&e).unwrap();
        assert_eq!(
            repository.count(&e.conversation_id).unwrap(),
            oracle.events(&e.conversation_id).len() as u64
        );
        assert_eq!(
            repository.heads(&e.conversation_id).unwrap(),
            oracle.heads(&e.conversation_id)
        );
        assert_eq!(
            repository
                .authored_event(&e.conversation_id, &e.author, e.seq)
                .unwrap(),
            Some(e.clone())
        );
        assert_eq!(
            repository
                .author_max(&e.conversation_id, &e.author)
                .unwrap(),
            Some(e.seq)
        );
        assert_eq!(
            repository
                .metadata(&e.conversation_id)
                .unwrap()
                .unwrap()
                .max_lamport,
            oracle
                .events(&e.conversation_id)
                .iter()
                .map(|e| e.lamport)
                .max()
                .unwrap()
        );
    }
    let conv = ConversationId::new([1; 32]);
    let author = Author::from_ed25519(a.public().ed25519_pub);
    assert!(matches!(
        repository.next_sequence(&conv, &author),
        Err(LogError::SequenceExhausted)
    ));
    assert!(matches!(
        repository.prepare(&conv),
        Err(LogError::ClockExhausted)
    ));
    let metadata = repository.metadata(&conv).unwrap().unwrap();
    assert!(metadata.channel_v2 && metadata.channel_v3 && metadata.channel_v4);
}
#[test]
fn atomic_new_channel_batch_rolls_back_missing_parent_and_equivocation() {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let root = event(&a, 1, vec![], 1, EventKind::Message);
    let missing = event(&a, 2, vec![EventId::new([99; 32])], 2, EventKind::Message);
    assert!(matches!(
        repository.append_channel_batch_atomic(&[root.clone(), missing]),
        Err(LogError::MissingParents(_))
    ));
    assert!(!repository.has(&root.id).unwrap());
    assert!(
        repository
            .metadata(&root.conversation_id)
            .unwrap()
            .is_none()
    );
    let fork = event(&a, 1, vec![root.id], 2, EventKind::Message);
    assert!(matches!(
        repository.append_channel_batch_atomic(&[root.clone(), fork]),
        Err(LogError::AuthorEquivocation { .. })
    ));
    let child = event(&a, 2, vec![root.id], 2, EventKind::Message);
    repository
        .append_channel_batch_atomic(&[root.clone(), root.clone(), child.clone()])
        .unwrap();
    assert_eq!(repository.count(&root.conversation_id).unwrap(), 2);
    assert_eq!(
        repository.heads(&root.conversation_id).unwrap(),
        vec![child.id]
    );
    assert!(repository.append_channel_batch_atomic(&[root]).is_err());
}
#[test]
fn fingerprint_is_exact_cached_persistently_and_reads_no_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let events: Vec<_> = (1..=128)
        .rev()
        .map(|seq| event(&a, seq, vec![], seq % 3, EventKind::Message))
        .collect();
    repository.append_channel_batch_atomic(&events).unwrap();
    let conv = events[0].conversation_id;
    let expected = super::super::sync::fingerprint(events.iter().map(|e| e.id).collect());
    assert_eq!(
        repository.fingerprint_and_len(&conv).unwrap(),
        (expected, 128)
    );
    assert_eq!(
        repository
            .fingerprint_ids
            .load(std::sync::atomic::Ordering::Relaxed),
        128
    );
    repository.append(&events[0]).unwrap();
    assert_eq!(
        repository.fingerprint_and_len(&conv).unwrap(),
        (expected, 128)
    );
    assert_eq!(
        repository
            .fingerprint_ids
            .load(std::sync::atomic::Ordering::Relaxed),
        128
    );
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let unknown = ConversationId::new([8; 32]);
    assert_eq!(
        repository.fingerprint_and_len(&unknown).unwrap(),
        (super::super::sync::fingerprint(vec![]), 0)
    );
    assert!(repository.metadata(&unknown).unwrap().is_none());
    drop(repository);
    drop(db);
    let reopened = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    assert_eq!(
        reopened.fingerprint_and_len(&conv).unwrap(),
        (expected, 128)
    );
    assert_eq!(
        reopened
            .fingerprint_ids
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        reopened
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
#[test]
fn keyset_pages_bound_rows_bytes_and_decode_only_selected_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let events: Vec<_> = (1..=128)
        .map(|seq| event(&a, seq, vec![], seq % 3, EventKind::Message))
        .collect();
    repository.append_channel_batch_atomic(&events).unwrap();
    let conv = events[0].conversation_id;
    let mut expected = events.clone();
    expected.sort_by_key(|e| (e.lamport, e.id));
    let mut cursor = None;
    let mut got = Vec::new();
    loop {
        let page = repository
            .headers_page(
                &conv,
                cursor.as_ref(),
                PageBudget {
                    rows: 7,
                    bytes: 7 * std::mem::size_of::<EventHeader>(),
                },
            )
            .unwrap();
        assert!(page.items.len() <= 7);
        assert!(page.items.capacity() * std::mem::size_of::<EventHeader>() <= page.charged_bytes);
        assert_eq!(
            page.charged_bytes,
            page.items.len() * std::mem::size_of::<EventHeader>()
        );
        got.extend(page.items.iter().map(|h| h.id));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(got, expected.iter().map(|e| e.id).collect::<Vec<_>>());
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let page = repository
        .events_page(
            &conv,
            None,
            PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap();
    assert_eq!(page.items, expected[..1]);
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    let required = repository.events_page(&conv, None, PageBudget { rows: 1, bytes: 1 });
    assert!(matches!(required,Err(LogError::PageBudgetTooSmall{required}) if required>1));
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert!(matches!(
        repository.headers_page(
            &conv,
            None,
            PageBudget {
                rows: 0,
                bytes: 1024
            }
        ),
        Err(LogError::InvalidPageBudget)
    ));
    let ids = repository
        .ids_page(&conv, None, PageBudget { rows: 2, bytes: 64 })
        .unwrap();
    assert_eq!(ids.items.len(), 2);
    assert_eq!(ids.charged_bytes, 64);
    let heads = repository
        .heads_page(&conv, None, PageBudget { rows: 2, bytes: 64 })
        .unwrap();
    assert_eq!(heads.items, ids.items);
    let conversations = repository
        .conversations_page(None, PageBudget::default())
        .unwrap();
    assert_eq!(conversations.items.len(), 1);
    assert_eq!(conversations.items[0].count, 128);
}
#[test]
fn cursors_bind_store_domain_conversation_order_and_revision() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let root = event(&a, 1, vec![], 1, EventKind::Message);
    let child = event(&a, 2, vec![root.id], 2, EventKind::Message);
    repository
        .append_channel_batch_atomic(&[root.clone(), child.clone()])
        .unwrap();
    let page = repository
        .headers_page(
            &root.conversation_id,
            None,
            PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap();
    let cursor = page.next.unwrap();
    assert!(matches!(
        repository.ids_page(&root.conversation_id, Some(&cursor), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        repository.headers_page(
            &ConversationId::new([9; 32]),
            Some(&cursor),
            PageBudget::default()
        ),
        Err(LogError::StaleCursor)
    ));
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    assert!(matches!(
        other.headers_page(&root.conversation_id, Some(&cursor), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
    let dir2 = tempfile::tempdir().unwrap();
    let other_store = EventRepository::new(database(&dir2), EventDomainId([1; 16])).unwrap();
    assert!(matches!(
        other_store.headers_page(&root.conversation_id, Some(&cursor), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
    repository.append(&child).unwrap(); // A true duplicate must not stale the cursor.
    assert_eq!(
        repository
            .headers_page(&root.conversation_id, Some(&cursor), PageBudget::default())
            .unwrap()
            .items
            .len(),
        1
    );
    repository
        .append(&event(&a, 3, vec![child.id], 3, EventKind::Profile))
        .unwrap();
    assert!(matches!(
        repository.headers_page(&root.conversation_id, Some(&cursor), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
}
#[test]
fn parent_edges_are_separately_budgeted_and_cross_conversation_parents_fail() {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let roots: Vec<_> = (1..=64)
        .map(|seq| event(&a, seq, vec![], seq, EventKind::Message))
        .collect();
    repository.append_channel_batch_atomic(&roots).unwrap();
    let merge = event(
        &a,
        65,
        roots.iter().map(|e| e.id).collect(),
        65,
        EventKind::Profile,
    );
    repository.append(&merge).unwrap();
    assert_eq!(
        repository.header(&merge.id).unwrap().unwrap().parent_count,
        64
    );
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let page = repository
            .parent_ids_page(
                &merge.id,
                cursor.as_ref(),
                PageBudget { rows: 3, bytes: 96 },
            )
            .unwrap()
            .unwrap();
        ids.extend(page.items);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids, merge.parents);
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    // The allocation preflight must continue to accept complete many-parent
    // signed bodies, rather than imposing a new protocol parent cap.
    assert_eq!(repository.get(&merge.id).unwrap(), Some(merge.clone()));
    let other = Event::new(
        &a,
        ConversationId::new([2; 32]),
        1,
        vec![roots[0].id],
        1,
        1,
        EventKind::Message,
        vec![],
    );
    assert!(matches!(
        repository.append(&other),
        Err(LogError::MissingParents(_))
    ));
}

#[test]
fn missing_authoritative_schema_or_index_fails_reopen_without_repair() {
    for object in ["TABLE event_heads", "INDEX retained_events_ids"] {
        let dir = tempfile::tempdir().unwrap();
        let db = database(&dir);
        let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
        let root = event(
            &DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repository.append(&root).unwrap();
        db.read(|c| {
            c.execute_batch(&format!("DROP {object}"))?;
            Ok(())
        })
        .unwrap();
        drop(repository);
        assert!(
            EventRepository::new(db.clone(), EventDomainId([1; 16])).is_err(),
            "must reject missing {object}"
        );
        db.read(|c| {
            let count: i64 =
                c.query_row("SELECT count(*) FROM retained_events", [], |r| r.get(0))?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
    }
}
#[test]
fn indexed_selectors_have_covering_id_plan_and_no_temporary_sort() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        for sql in [
            "EXPLAIN QUERY PLAN SELECT id FROM retained_events WHERE domain=?1 AND conversation=?2 ORDER BY id",
            "EXPLAIN QUERY PLAN SELECT id FROM retained_events WHERE domain=?1 AND conversation=?2 AND id>?3 ORDER BY id LIMIT 8",
        ] {
            let mut stmt=c.prepare(sql)?;let mut rows=if sql.contains("?3"){stmt.query(params![[1u8;16].as_slice(),[1u8;32].as_slice(),[0u8;32].as_slice()])?}else{stmt.query(params![[1u8;16].as_slice(),[1u8;32].as_slice()])?};
            let mut details=String::new();while let Some(row)=rows.next()? {details.push_str(&row.get::<_,String>(3)?);}
            assert!(details.contains("COVERING INDEX retained_events_ids"),"{details}");assert!(!details.contains("TEMP B-TREE"),"{details}");
        }
        Ok(())
    }).unwrap();
}
#[test]
fn malformed_accessed_data_is_error_and_unselected_corrupt_body_is_not_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let root = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repository.append(&root).unwrap();
    let mut bytes = bincode::serialize(&root).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    db.read(|c| {
        c.execute("UPDATE retained_events SET body=?1", [bytes])?;
        Ok(())
    })
    .unwrap();
    assert!(repository.header(&root.id).unwrap().is_some());
    assert_eq!(
        repository
            .fingerprint_and_len(&root.conversation_id)
            .unwrap()
            .1,
        1
    );
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(repository.get(&root.id).is_err());
    db.read(|c| {
        c.execute_batch(
            "PRAGMA ignore_check_constraints=ON; UPDATE retained_events SET seq=x'00'",
        )?;
        Ok(())
    })
    .unwrap();
    assert!(repository.header(&root.id).is_err());
    db.read(|c| {
        c.execute_batch("UPDATE event_conversations SET revision=-1")?;
        Ok(())
    })
    .unwrap();
    assert!(repository.count(&root.conversation_id).is_err());
}
#[test]
fn transaction_failure_and_counter_exhaustion_preserve_all_admission_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let root = event(&a, 1, vec![], 1, EventKind::Message);
    repository.append(&root).unwrap();
    let initial = repository.metadata(&root.conversation_id).unwrap();
    let hash = repository
        .fingerprint_and_len(&root.conversation_id)
        .unwrap();
    let child = event(&a, 2, vec![root.id], 2, EventKind::ChannelReplica);
    db.read(|c| {c.execute_batch("CREATE TRIGGER fail_admission BEFORE UPDATE ON event_domains BEGIN SELECT RAISE(ABORT,'injected failure'); END")?;Ok(())}).unwrap();
    assert!(repository.append(&child).is_err());
    assert!(!repository.has(&child.id).unwrap());
    assert_eq!(repository.metadata(&root.conversation_id).unwrap(), initial);
    assert_eq!(
        repository.heads(&root.conversation_id).unwrap(),
        vec![root.id]
    );
    assert_eq!(
        repository
            .author_max(&root.conversation_id, &root.author)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        repository
            .fingerprint_and_len(&root.conversation_id)
            .unwrap(),
        hash
    );
    db.read(|c| {
        c.execute_batch(
            "DROP TRIGGER fail_admission; UPDATE event_domains SET next_commit=9223372036854775807",
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        repository.append(&child),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert!(!repository.has(&child.id).unwrap());
    db.read(|c| {c.execute_batch("UPDATE event_domains SET next_commit=1; UPDATE event_conversations SET revision=9223372036854775807")?;Ok(())}).unwrap();
    assert!(matches!(
        repository.append(&child),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert!(!repository.has(&child.id).unwrap());
}
#[test]
fn unsigned_clock_range_and_one_oversize_progress_retry_preserve_signed_events() {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let mut events: Vec<_> = [u64::MAX, 0, i64::MAX as u64, (i64::MAX as u64) + 1]
        .into_iter()
        .enumerate()
        .map(|(n, clock)| event(&a, n as u64, vec![], clock, EventKind::Message))
        .collect();
    events[1] = Event::new(
        &a,
        events[1].conversation_id,
        events[1].seq,
        vec![],
        0,
        0,
        EventKind::Message,
        vec![1; 16384],
    );
    repository.append_channel_batch_atomic(&events).unwrap();
    let conv = events[0].conversation_id;
    let page = repository
        .headers_page(&conv, None, PageBudget::default())
        .unwrap();
    assert_eq!(
        page.items.iter().map(|e| e.lamport).collect::<Vec<_>>(),
        vec![0, i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX]
    );
    let Err(LogError::PageBudgetTooSmall { required }) = repository.events_page(
        &conv,
        None,
        PageBudget {
            rows: 1,
            bytes: 1024,
        },
    ) else {
        panic!("oversized first event must not be fetched")
    };
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let page = repository
        .events_page(
            &conv,
            None,
            PageBudget {
                rows: 1,
                bytes: required,
            },
        )
        .unwrap();
    assert_eq!(page.items, vec![events[1].clone()]);
    assert_eq!(page.charged_bytes, required);
}

#[test]
fn other_domain_parent_and_invalid_content_leave_no_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let root = event(&a, 1, vec![], 1, EventKind::Message);
    repository.append(&root).unwrap();
    let child = event(&a, 2, vec![root.id], 2, EventKind::Message);
    assert!(
        matches!(other.append(&child),Err(LogError::MissingParents(ids)) if ids==vec![root.id])
    );
    assert!(other.metadata(&child.conversation_id).unwrap().is_none());
    let mut corrupt = child.clone();
    corrupt.ciphertext.push(1);
    assert!(matches!(
        repository.append(&corrupt),
        Err(LogError::CorruptId)
    ));
    let mut noncanonical = child;
    noncanonical.parents.push(root.id);
    assert!(matches!(
        repository.append(&noncanonical),
        Err(LogError::NonCanonical)
    ));
    assert_eq!(repository.count(&root.conversation_id).unwrap(), 1);
}
#[test]
fn byte_limited_body_prefix_and_zero_budgets_never_fetch_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let events: Vec<_> = (1..=8)
        .map(|seq| event(&a, seq, vec![], seq, EventKind::Message))
        .collect();
    repository.append_channel_batch_atomic(&events).unwrap();
    let conv = events[0].conversation_id;
    let one = repository
        .header(&events[0].id)
        .unwrap()
        .unwrap()
        .stored_bytes
        * 3
        + std::mem::size_of::<Event>();
    let page = repository
        .events_page(
            &conv,
            None,
            PageBudget {
                rows: 8,
                bytes: one * 2,
            },
        )
        .unwrap();
    assert_eq!(page.items, events[..2]);
    assert_eq!(page.charged_bytes, one * 2);
    assert!(page.next.is_some());
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    for budget in [
        PageBudget { rows: 0, bytes: 1 },
        PageBudget { rows: 1, bytes: 0 },
        PageBudget {
            rows: usize::MAX,
            bytes: 1,
        },
    ] {
        assert!(matches!(
            repository.events_page(&conv, None, budget),
            Err(LogError::InvalidPageBudget)
        ));
        assert!(matches!(
            repository.parent_ids_page(&events[0].id, None, budget),
            Err(LogError::InvalidPageBudget)
        ));
    }
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    let id_page = repository
        .ids_page(&conv, None, PageBudget { rows: 3, bytes: 96 })
        .unwrap();
    assert_eq!(id_page.items.capacity() * 32, id_page.charged_bytes);
}

#[test]
fn stored_body_rejects_trailing_encoding_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let root = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repository.append(&root).unwrap();
    let mut bytes = bincode::serialize(&root).unwrap();
    bytes.extend_from_slice(b"trailing garbage");
    db.read(|c| {
        c.execute(
            "UPDATE retained_events SET body=?1,stored_bytes=?2",
            params![bytes, bytes.len() as i64],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(repository.get(&root.id).is_err());
}

#[test]
fn known_version_rejects_changed_types_nullability_keys_constraints_and_foreign_keys() {
    for (original, changed) in [
        (
            "singleton INTEGER PRIMARY KEY CHECK(singleton=1)",
            "singleton INTEGER PRIMARY KEY",
        ),
        ("version INTEGER NOT NULL", "version TEXT NOT NULL"),
        ("max_seq BLOB NOT NULL", "max_seq INTEGER NOT NULL"),
        ("max_seq BLOB NOT NULL", "max_seq BLOB"),
        (
            "PRIMARY KEY(domain,conversation,author)",
            "PRIMARY KEY(domain,conversation,author DESC)",
        ),
        (
            "PRIMARY KEY(domain,conversation,author)",
            "PRIMARY KEY(domain,conversation,author COLLATE NOCASE)",
        ),
        (", UNIQUE(domain,conversation,author,seq)", ""),
        (", UNIQUE(domain,commit_ordinal)", ""),
        (
            "CHECK(typeof(max_seq)='blob' AND length(max_seq)=8)",
            "CHECK(length(max_seq)>0)",
        ),
        (
            ",\n FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation)\n",
            "\n",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = database(&dir);
        let ddl = schema::SCHEMA.replace(original, changed);
        assert_ne!(ddl, schema::SCHEMA, "fixture must change {original}");
        db.transaction(|tx| {
            tx.execute_batch(&ddl)?;
            Ok(())
        })
        .unwrap();
        let before: Vec<String> = db
            .read(|c| {
                let mut stmt =
                    c.prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")?;
                Ok(stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?)
            })
            .unwrap();
        assert!(
            matches!(
                EventRepository::new(db.clone(), EventDomainId([1; 16])),
                Err(LogError::IncompatibleEventSchema)
            ),
            "must reject changed authority: {original}"
        );
        db.read(|c| {
            let mut stmt =
                c.prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")?;
            let after: Vec<String> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            assert_eq!(after, before);
            let domains: i64 =
                c.query_row("SELECT count(*) FROM event_domains", [], |r| r.get(0))?;
            assert_eq!(domains, 0);
            Ok(())
        })
        .unwrap();
    }
}
#[test]
fn bounded_version_vector_discovers_all_authors_gaps_and_full_range_maxima() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let other = EventRepository::new(db, EventDomainId([2; 16])).unwrap();
    let mut oracle = super::super::EventLog::default();
    let mut events = Vec::new();
    for sequences in [&[1, 7, 3][..], &[0, u64::MAX, 9][..], &[8, 4][..]] {
        let author = DeviceIdentity::generate();
        for &seq in sequences {
            events.push(event(&author, seq, vec![], seq, EventKind::Message));
        }
    }
    for e in &events {
        oracle.append(e.clone()).unwrap();
    }
    repository.append_channel_batch_atomic(&events).unwrap();
    let conv = events[0].conversation_id;
    let mut cursor = None;
    let mut maxima = std::collections::HashMap::new();
    loop {
        let page = repository
            .author_maxima_page(
                &conv,
                cursor.as_ref(),
                PageBudget {
                    rows: 1,
                    bytes: std::mem::size_of::<super::pages::AuthorMaximum>(),
                },
            )
            .unwrap();
        assert!(page.items.len() <= 1);
        for row in page.items {
            maxima.insert(row.author, row.max_seq);
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(maxima, oracle.version_vector(&conv));
    assert_eq!(maxima.len(), 3);
    assert!(
        other
            .author_maxima_page(&conv, None, PageBudget::default())
            .unwrap()
            .items
            .is_empty()
    );
    let page = repository
        .author_maxima_page(
            &conv,
            None,
            PageBudget {
                rows: 1,
                bytes: 128,
            },
        )
        .unwrap();
    let cursor = page.next.unwrap();
    assert!(matches!(
        repository.ids_page(&conv, Some(&cursor), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        repository.author_maxima_page(&conv, None, PageBudget { rows: 1, bytes: 1 }),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
#[test]
fn another_conversation_append_preserves_conversation_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let repository = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let root = event(&a, 1, vec![], 1, EventKind::Message);
    let child = event(&a, 2, vec![root.id], 2, EventKind::Message);
    repository
        .append_channel_batch_atomic(&[root.clone(), child.clone()])
        .unwrap();
    let page = repository
        .headers_page(
            &root.conversation_id,
            None,
            PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap();
    let cursor = page.next.unwrap();
    let other = Event::new(
        &a,
        ConversationId::new([2; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![],
    );
    repository.append(&other).unwrap();
    assert_eq!(
        repository
            .headers_page(&root.conversation_id, Some(&cursor), PageBudget::default())
            .unwrap()
            .items[0]
            .id,
        child.id
    );
}
#[test]
fn oversized_metadata_arithmetic_errors_before_selecting_body() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let root = event(
        &DeviceIdentity::generate(),
        1,
        vec![],
        1,
        EventKind::Message,
    );
    repository.append(&root).unwrap();
    db.read(|c| {c.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE retained_events SET stored_bytes=9223372036854775807")?;Ok(())}).unwrap();
    assert!(matches!(
        repository.events_page(
            &root.conversation_id,
            None,
            PageBudget {
                rows: 1,
                bytes: usize::MAX
            }
        ),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn empty_channel_batch_is_noop_for_existing_and_unknown_conversations() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    repository.append_channel_batch_atomic(&[]).unwrap();
    let a = DeviceIdentity::generate();
    let root = event(&a, 1, vec![], 1, EventKind::Message);
    repository.append(&root).unwrap();
    let before = repository.metadata(&root.conversation_id).unwrap();
    let hash = repository
        .fingerprint_and_len(&root.conversation_id)
        .unwrap();
    repository.append_channel_batch_atomic(&[]).unwrap();
    assert_eq!(repository.metadata(&root.conversation_id).unwrap(), before);
    assert_eq!(
        repository
            .fingerprint_and_len(&root.conversation_id)
            .unwrap(),
        hash
    );
    db.read(|c| {
        let next: i64 = c.query_row("SELECT next_commit FROM event_domains", [], |r| r.get(0))?;
        assert_eq!(next, 1);
        Ok(())
    })
    .unwrap();
}
#[test]
fn false_small_body_metadata_is_rejected_before_blob_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let a = DeviceIdentity::generate();
    let root = Event::new(
        &a,
        ConversationId::new([1; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![1; 16384],
    );
    repository.append(&root).unwrap();
    db.read(|c| {
        c.execute_batch(
            "PRAGMA ignore_check_constraints=ON; UPDATE retained_events SET stored_bytes=1",
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        repository
            .events_page(
                &root.conversation_id,
                None,
                PageBudget {
                    rows: 1,
                    bytes: 1024
                }
            )
            .is_err()
    );
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn known_version_preserves_ddl_token_boundaries_when_validating_column_authority() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let ddl = schema::SCHEMA.replace("max_seq BLOB NOT NULL", "max_seq BLOBNOTNULL");
    db.transaction(|tx| {
        tx.execute_batch(&ddl)?;
        Ok(())
    })
    .unwrap();
    db.read(|c| {
        let (kind,required):(String,i64)=c.query_row("SELECT type,\"notnull\" FROM pragma_table_info('event_author_max') WHERE name='max_seq'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!((kind,required),("BLOBNOTNULL".into(),0));
        Ok(())
    }).unwrap();
    assert!(matches!(
        EventRepository::new(db.clone(), EventDomainId([1; 16])),
        Err(LogError::IncompatibleEventSchema)
    ));
    db.read(|c| {
        let domains: i64 = c.query_row("SELECT count(*) FROM event_domains", [], |r| r.get(0))?;
        assert_eq!(domains, 0);
        Ok(())
    })
    .unwrap();
}

fn assert_vector_length_rejected_before_event_decode(field: &str) {
    for matching_header in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = database(&dir);
        let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
        let root = event(
            &DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repository.append(&root).unwrap();
        // Derive each length-prefix position from the existing fixint types,
        // rather than baking Event's current field byte offsets into a fixture.
        let parent_offset =
            bincode::serialized_size(&(root.id, root.conversation_id, root.author, root.seq))
                .unwrap();
        let cipher_offset = bincode::serialized_size(&(
            root.id,
            root.conversation_id,
            root.author,
            root.seq,
            &root.parents,
            root.lamport,
            root.wall_clock,
            root.kind,
        ))
        .unwrap();
        let signature_offset = bincode::serialized_size(&(
            root.id,
            root.conversation_id,
            root.author,
            root.seq,
            &root.parents,
            root.lamport,
            root.wall_clock,
            root.kind,
            &root.ciphertext,
        ))
        .unwrap();
        let (offset, column) = match field {
            "parents" => (parent_offset, "parent_count"),
            "ciphertext" => (cipher_offset, "ciphertext_bytes"),
            "signature" => (signature_offset, "signature_bytes"),
            _ => unreachable!(),
        };
        let claimed = if matching_header { 4096_u64 } else { u64::MAX };
        let mut bytes = bincode::serialize(&root).unwrap();
        let prefix = bincode::serialize(&claimed).unwrap();
        let offset = offset as usize;
        bytes[offset..offset + prefix.len()].copy_from_slice(&prefix);
        db.read(|c| {
            c.execute("UPDATE retained_events SET body=?1", [bytes])?;
            if matching_header {
                c.execute(
                    &format!("UPDATE retained_events SET {column}=?1"),
                    [claimed as i64],
                )?;
            }
            Ok(())
        })
        .unwrap();
        assert!(
            repository
                .events_page(
                    &root.conversation_id,
                    None,
                    PageBudget {
                        rows: 1,
                        bytes: 4096
                    }
                )
                .is_err()
        );
        assert_eq!(
            repository
                .body_decodes
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "{field}: corrupt length must fail before derived Event deserialization; matching_header={matching_header}"
        );
    }
}
#[test]
fn corrupt_parent_length_is_rejected_before_length_driven_allocation() {
    assert_vector_length_rejected_before_event_decode("parents");
}
#[test]
fn corrupt_ciphertext_length_is_rejected_before_length_driven_allocation() {
    assert_vector_length_rejected_before_event_decode("ciphertext");
}
#[test]
fn corrupt_signature_length_is_rejected_before_length_driven_allocation() {
    assert_vector_length_rejected_before_event_decode("signature");
}

#[test]
fn causal_order_work_is_persisted_in_encrypted_ordinary_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let _repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        let count:i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('event_order_jobs','event_order_work','event_order_rows','event_order_current')", [], |r|r.get(0))?;
        assert_eq!(count,4,"exact causal order needs persistent resumable encrypted work");
        Ok(())
    }).unwrap();
}

#[test]
fn maintenance_estimates_are_persisted_separately_from_stored_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        let columns: i64 = c.query_row("SELECT count(*) FROM pragma_table_info('event_domains') WHERE name IN ('estimated_bytes','source_revision')", [], |r|r.get(0))?;
        assert_eq!(columns,2,"maintenance requires durable estimated bytes and mutation revision");
        Ok(())
    }).unwrap();
    let identity = DeviceIdentity::generate();
    let root = event(&identity, 1, vec![], 1, EventKind::Message);
    let child = event(&identity, 2, vec![root.id], 2, EventKind::Profile);
    repository.append(&root).unwrap();
    repository.append(&child).unwrap();
    repository.append(&child).unwrap();
    db.read(|c| {
        let totals:(i64,i64,i64)=c.query_row("SELECT d.estimated_bytes,c.estimated_bytes,d.source_revision FROM event_domains d JOIN event_conversations c USING(domain) WHERE domain=?1", [[1u8;16].as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(totals,(800,800,2));
        Ok(())
    }).unwrap();
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn maintenance_drop_is_scoped_atomic_and_recreate_cannot_reuse_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let other = EventRepository::new(db.clone(), EventDomainId([2; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let first = event(&identity, 1, vec![], 1, EventKind::Message);
    let second = event(&identity, 2, vec![first.id], 2, EventKind::Message);
    repository.append(&first).unwrap();
    repository.append(&second).unwrap();
    other.append(&first).unwrap();
    let cursor = repository
        .headers_page(
            &first.conversation_id,
            None,
            PageBudget {
                rows: 1,
                bytes: 4096,
            },
        )
        .unwrap()
        .next
        .unwrap();
    assert_eq!(
        repository
            .drop_conversation(&first.conversation_id)
            .unwrap(),
        2
    );
    assert_eq!(repository.count(&first.conversation_id).unwrap(), 0);
    assert_eq!(other.count(&first.conversation_id).unwrap(), 1);
    assert!(matches!(
        repository.headers_page(&first.conversation_id, Some(&cursor), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
    repository.append(&first).unwrap();
    repository.append(&second).unwrap();
    assert!(matches!(
        repository.headers_page(&first.conversation_id, Some(&cursor), PageBudget::default()),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(
        repository
            .body_decodes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn physical_domain_charge_tracks_append_duplicate_and_drop() {
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
    repo.append(&e).unwrap();
    let charge = || {
        db.read(|c| {
            Ok(c.query_row(
                "SELECT event_count,estimated_bytes FROM event_domains WHERE domain=?1",
                [[1u8; 16].as_slice()],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )?)
        })
    };
    assert_eq!(charge().unwrap(), (1, 384));
    repo.drop_conversation(&e.conversation_id).unwrap();
    assert_eq!(charge().unwrap(), (0, 0));
}

#[test]
fn retirement_schema_has_explicit_lifecycle_and_durable_job_authority() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let _repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    db.read(|c| {
        let lifecycle: i64 = c.query_row(
            "SELECT count(*) FROM pragma_table_info('event_conversations') WHERE name='lifecycle'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            lifecycle, 1,
            "retirement must have an explicit indexed lifecycle"
        );
        let jobs: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='event_retirement_jobs'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(jobs, 1, "retirement progress must survive reopen");
        Ok(())
    })
    .unwrap();
}

#[test]
fn retirement_logical_visibility_precedes_physical_reclamation() {
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
    repo.database.transaction(|c| {
        c.execute("UPDATE event_conversations SET lifecycle=1,destructive_epoch=1,revision=2 WHERE domain=?1 AND conversation=?2", params![repo.domain.0.as_slice(),e.conversation_id.as_bytes().as_slice()])?;
        c.execute("INSERT INTO event_retirement_jobs(domain,conversation,store_uuid,epoch,revision) VALUES(?1,?2,?3,1,2)",params![repo.domain.0.as_slice(),e.conversation_id.as_bytes().as_slice(),repo.store_uuid.as_slice()])?;
        Ok(())
    }).unwrap();
    assert!(
        repo.get(&e.id).is_err(),
        "hidden bodies must be unavailable before physical deletion"
    );
    assert!(
        repo.append(&e).is_err(),
        "hidden duplicate must not qualify"
    );
    assert!(repo.metadata(&e.conversation_id).is_err());
}

#[test]
fn validated_growth_gate_blocks_without_mutation_and_never_runs_for_duplicates_or_invalid_events() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(&dir);
    let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
    let identity = DeviceIdentity::generate();
    let e = event(&identity, 1, vec![], 1, EventKind::Message);
    let charge = db.read(|c| repo.domain_charge_on(c)).unwrap();
    let result = db
        .transaction(|c| {
            repo.append_with_gate_on(c, &e, |cost| {
                assert_eq!(cost, 384);
                Ok(AppendGate::Block("local resource"))
            })
        })
        .unwrap();
    assert!(matches!(result, GatedAppend::Blocked("local resource")));
    assert_eq!(db.read(|c| repo.domain_charge_on(c)).unwrap(), charge);
    assert!(repo.get(&e.id).unwrap().is_none());
    repo.append(&e).unwrap();
    let result = db
        .transaction(|c| {
            repo.append_with_gate_on(c, &e, |_| -> Result<AppendGate<()>, LogError> {
                panic!("duplicate must not reserve growth")
            })
        })
        .unwrap();
    assert!(matches!(
        result,
        GatedAppend::Accepted(AppendOutcome::Duplicate)
    ));
    let mut forged = e.clone();
    forged.sig[0] ^= 1;
    assert!(matches!(
        db.transaction(|c| repo.append_with_gate_on(
            c,
            &forged,
            |_| -> Result<AppendGate<()>, LogError> {
                panic!("invalid duplicate must not call growth gate")
            }
        )),
        Err(LogError::BadSignature)
    ));
    let missing = event(
        &identity,
        2,
        vec![EventId::new([9; 32])],
        2,
        EventKind::Message,
    );
    assert!(matches!(
        db.transaction(|c| repo.append_with_gate_on(
            c,
            &missing,
            |_| -> Result<AppendGate<()>, LogError> {
                panic!("missing parent must not call growth gate")
            }
        )),
        Err(LogError::MissingParents(_))
    ));
    let equiv = event(&identity, 1, vec![], 2, EventKind::Message);
    assert!(matches!(
        db.transaction(|c| repo.append_with_gate_on(
            c,
            &equiv,
            |_| -> Result<AppendGate<()>, LogError> {
                panic!("equivocation must not call growth gate")
            }
        )),
        Err(LogError::AuthorEquivocation { .. })
    ));
}

#[test]
fn runtime_metadata_received_work_retries_fairly_and_reopens_without_body_reads() {
    let dir = tempfile::tempdir().unwrap();
    let database = database(&dir);
    database.transaction(|c|{c.execute_batch("CREATE TABLE transcript_received_evidence(id BLOB PRIMARY KEY,conversation BLOB NOT NULL) WITHOUT ROWID;")?;Ok(())}).unwrap();
    let domain = EventDomainId([9; 16]);
    let repo = EventRepository::new(database.clone(), domain).unwrap();
    let local = DeviceIdentity::generate();
    let peer = DeviceIdentity::generate();
    let mut events = Vec::new();
    for n in 0..257 {
        let item = event(&peer, n + 1, vec![], n, EventKind::Message);
        repo.append(&item).unwrap();
        events.push(item);
    }
    let conv = events[0].conversation_id;
    repo.initialize_receiving(Author::from_ed25519(local.public().ed25519_pub))
        .unwrap();
    let first = repo
        .pending_receive_ids(conv, 128, 8 * 1024 * 1024)
        .unwrap();
    assert_eq!(first.len(), 128);
    let second = repo
        .pending_receive_ids(conv, 128, 8 * 1024 * 1024)
        .unwrap();
    assert_eq!(second.len(), 128);
    assert!(!second.iter().any(|id| first.contains(id)));
    let third = repo
        .pending_receive_ids(conv, 128, 8 * 1024 * 1024)
        .unwrap();
    assert_eq!(
        third[0], events[256].id,
        "an old unknown key cannot starve a later processable event"
    );
    database
        .transaction(|c| {
            c.execute(
                "INSERT INTO transcript_received_evidence VALUES(?1,?2)",
                params![
                    events[256].id.as_bytes().as_slice(),
                    conv.as_bytes().as_slice()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(repo.body_read_counts(), (0, 0));
    drop(repo);
    let reopened = EventRepository::new(database.clone(), domain).unwrap();
    reopened
        .initialize_receiving(Author::from_ed25519(local.public().ed25519_pub))
        .unwrap();
    for _ in 0..4 {
        assert!(
            !reopened
                .pending_receive_ids(conv, 128, 8 * 1024 * 1024)
                .unwrap()
                .contains(&events[256].id)
        );
    }
    assert_eq!(reopened.body_read_counts(), (0, 0));
    assert!(matches!(
        reopened.pending_receive_ids(conv, usize::MAX, 1),
        Err(LogError::InvalidPageBudget)
    ));
    assert_eq!(reopened.body_read_counts(), (0, 0));
}

#[test]
fn runtime_metadata_receive_bootstrap_resumes_and_keeps_control_work_separate() {
    let dir = tempfile::tempdir().unwrap();
    let database = database(&dir);
    database.transaction(|c|{c.execute_batch("CREATE TABLE transcript_received_evidence(id BLOB PRIMARY KEY,conversation BLOB NOT NULL) WITHOUT ROWID;")?;Ok(())}).unwrap();
    let domain = EventDomainId([9; 16]);
    let repo = EventRepository::new(database.clone(), domain).unwrap();
    let local = DeviceIdentity::generate();
    let peer = DeviceIdentity::generate();
    let author = Author::from_ed25519(local.public().ed25519_pub);
    repo.initialize_receiving(author).unwrap();
    for n in 0..257 {
        repo.append(&event(&peer, n + 1, vec![], n, EventKind::Message))
            .unwrap();
    }
    let control = event(&peer, 258, vec![], 258, EventKind::Delete);
    repo.append(&control).unwrap();
    assert!(repo.discover_receives().unwrap());
    // A crash after a committed migration page must resume the unfinished scan.
    database
        .transaction(|c| {
            c.execute(
                "UPDATE participant_receive_scan SET initialized=0 WHERE domain=?1",
                [domain.0.as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    drop(repo);
    let repo = EventRepository::new(database.clone(), domain).unwrap();
    repo.initialize_receiving(author).unwrap();
    let (after, through) = repo.bootstrap_control_position().unwrap();
    assert_eq!(after, -1);
    assert!(through >= 257);
    assert_eq!(
        repo.pending_control_ids(control.conversation_id, 1, 4096)
            .unwrap(),
        vec![control.id]
    );
    assert_eq!(
        repo.pending_receive_ids(control.conversation_id, 128, 8 * 1024 * 1024)
            .unwrap()
            .len(),
        128
    );
    repo.advance_control_bootstrap(through).unwrap();
    repo.complete_control(control.id).unwrap();
    let late = event(&peer, 259, vec![], 259, EventKind::Delete);
    repo.append(&late).unwrap();
    drop(repo);
    let repo = EventRepository::new(database, domain).unwrap();
    repo.initialize_receiving(author).unwrap();
    assert_eq!(
        repo.bootstrap_control_position().unwrap(),
        (through, through)
    );
    assert_eq!(
        repo.pending_control_ids(control.conversation_id, 1, 4096)
            .unwrap(),
        vec![late.id]
    );
    assert_eq!(repo.body_read_counts(), (0, 0));
}

#[test]
fn runtime_metadata_oversized_receive_cannot_starve_later_valid_event() {
    let dir = tempfile::tempdir().unwrap();
    let database = database(&dir);
    database.transaction(|c|{c.execute_batch("CREATE TABLE transcript_received_evidence(id BLOB PRIMARY KEY,conversation BLOB NOT NULL) WITHOUT ROWID;")?;Ok(())}).unwrap();
    let repo = EventRepository::new(database, EventDomainId([9; 16])).unwrap();
    let local = DeviceIdentity::generate();
    let peer = DeviceIdentity::generate();
    let oversized = Event::new(
        &peer,
        ConversationId::new([1; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![1; 262_145],
    );
    let later = event(&peer, 2, vec![], 2, EventKind::Message);
    repo.append(&oversized).unwrap();
    repo.append(&later).unwrap();
    let mut parents = Vec::new();
    for n in 0..1025 {
        let parent = event(&local, n + 1, vec![], n, EventKind::Message);
        repo.append(&parent).unwrap();
        parents.push(parent.id);
    }
    let wide = event(&peer, 3, parents, 1027, EventKind::Message);
    repo.append(&wide).unwrap();
    repo.initialize_receiving(Author::from_ed25519(local.public().ed25519_pub))
        .unwrap();
    assert_eq!(
        repo.pending_receive_ids(later.conversation_id, 3, 4096)
            .unwrap(),
        vec![later.id]
    );
    let retry = repo
        .pending_receive_ids(later.conversation_id, 3, 8 * 1024 * 1024)
        .unwrap();
    assert!(
        retry.contains(&oversized.id),
        "a byte-budget defer must preserve later eligibility"
    );
    assert!(
        retry.contains(&wide.id),
        "valid events with 1025 parents remain eligible"
    );
    assert!(matches!(
        repo.pending_receive_ids(later.conversation_id, 1, 1),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert_eq!(
        repo.pending_receive_ids(later.conversation_id, 3, 8 * 1024 * 1024)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(repo.body_read_counts(), (0, 0));
}
