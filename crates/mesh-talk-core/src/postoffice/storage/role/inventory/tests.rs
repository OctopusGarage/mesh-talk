use super::preparation::{DigestWorker, PreparationProgress};
use super::*;
use crate::{
    eventlog::EventKind, identity::device::DeviceIdentity, storage::profile::ProfileStorageLease,
};
fn fixture() -> (tempfile::TempDir, DeviceIdentity, IndexedRelayState) {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &root.path().join("relay.log"),
        "pw",
        100,
    )
    .unwrap();
    let state = IndexedRelayState::from_profile(profile, RelayAdmission::LegacyCompatible).unwrap();
    (root, identity, state)
}
fn conversation(n: u32) -> ConversationId {
    let mut key = [0; 32];
    key[..4].copy_from_slice(b"MTF3");
    key[28..].copy_from_slice(&n.to_be_bytes());
    ConversationId::new(key)
}
fn event(identity: &DeviceIdentity, conv: ConversationId, seq: u64) -> Event {
    Event::new(
        identity,
        conv,
        seq,
        vec![],
        seq,
        seq,
        EventKind::Message,
        vec![42],
    )
}
fn finish(state: &IndexedRelayState) -> (usize, usize) {
    let mut worker = DigestWorker::default();
    let mut examined = 0;
    let mut hashed = 0;
    for _ in 0..10000 {
        let p = worker.step_at(state, 100).unwrap();
        examined += p.examined;
        hashed += p.hashed;
        if state.preparation_ready_at(100).unwrap() {
            return (examined, hashed);
        }
    }
    panic!("preparation did not finish")
}
// Observe actual SQLite compilation without enabling a production hooks feature.
struct MetadataCompiles<'a> {
    database: &'a crate::storage::database::EncryptedDatabase,
    count: Option<Box<std::sync::atomic::AtomicUsize>>,
}
impl<'a> MetadataCompiles<'a> {
    fn install(database: &'a crate::storage::database::EncryptedDatabase) -> Self {
        let count = Box::new(std::sync::atomic::AtomicUsize::new(0));
        database
            .read(|c| {
                // SAFETY: the owner holds its connection mutex; the boxed atomic's
                // address is stable until Drop unregisters this callback.
                let rc = unsafe {
                    rusqlite::ffi::sqlite3_set_authorizer(
                        c.handle(),
                        Some(metadata_compile),
                        (&*count as *const std::sync::atomic::AtomicUsize)
                            .cast_mut()
                            .cast(),
                    )
                };
                assert_eq!(rc, rusqlite::ffi::SQLITE_OK);
                Ok(())
            })
            .unwrap();
        Self {
            database,
            count: Some(count),
        }
    }
    fn reset(&self) {
        self.count
            .as_ref()
            .unwrap()
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }
    fn count(&self) -> usize {
        self.count
            .as_ref()
            .unwrap()
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl Drop for MetadataCompiles<'_> {
    fn drop(&mut self) {
        let unregistered = self.database.read(|c| {
            // SAFETY: the callback is removed under the owner's connection lock
            // before its boxed userdata is released.
            let rc = unsafe {
                rusqlite::ffi::sqlite3_set_authorizer(c.handle(), None, std::ptr::null_mut())
            };
            assert_eq!(rc, rusqlite::ffi::SQLITE_OK);
            Ok(())
        });
        if unregistered.is_err() {
            // A faulted owner cannot be entered even for cleanup. Keep userdata
            // alive until the connection closes instead of leaving a dangling
            // authorizer pointer during a failing test's unwind.
            Box::leak(self.count.take().unwrap());
        }
    }
}
unsafe extern "C" fn metadata_compile(
    userdata: *mut std::ffi::c_void,
    action: std::ffi::c_int,
    table: *const std::ffi::c_char,
    column: *const std::ffi::c_char,
    _database: *const std::ffi::c_char,
    _view: *const std::ffi::c_char,
) -> std::ffi::c_int {
    if action == rusqlite::ffi::SQLITE_READ && !table.is_null() && !column.is_null() {
        // SAFETY: SQLite supplies live NUL-terminated strings for SQLITE_READ;
        // install keeps this atomic userdata alive for the callback's lifetime.
        let (table, column, count) = unsafe {
            (
                std::ffi::CStr::from_ptr(table).to_bytes(),
                std::ffi::CStr::from_ptr(column).to_bytes(),
                &*userdata.cast::<std::sync::atomic::AtomicUsize>(),
            )
        };
        if matches!(
            table,
            b"event_conversations" | b"event_retirement_jobs" | b"relay_federation_digests"
        ) || (table == b"relay_mailboxes" && column == b"first_seen")
        {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    rusqlite::ffi::SQLITE_OK
}
#[test]
fn warmed_inventory_metadata_compilation_is_constant_across_mailboxes() {
    let (_root, identity, state) = fixture();
    for n in 1..=32 {
        state
            .record(&event(&identity, conversation(n), 1), 100, None)
            .unwrap();
    }
    state.request_preparation_at(100).unwrap();
    finish(&state);
    let view = state.inventory_view_at(100).unwrap();
    let database = state.profile.database();
    // Install before warmup: changing SQLite's authorizer itself expires VMs.
    let compiles = MetadataCompiles::install(database);
    // Page operations run outside the connection lock held by installation.
    let warm = state.inventory_page_at(&view, None, 512, 100).unwrap();
    assert_eq!(warm.entries.len(), 32);
    compiles.reset();
    let repeated = state.inventory_page_at(&view, None, 512, 100).unwrap();
    assert_eq!(
        repeated
            .entries
            .iter()
            .map(|e| (e.id, e.digest, e.first_seen_secs))
            .collect::<Vec<_>>(),
        warm.entries
            .iter()
            .map(|e| (e.id, e.digest, e.first_seen_secs))
            .collect::<Vec<_>>()
    );
    assert!(
        compiles.count() <= 4,
        "warmed metadata recompiled per mailbox: {} read authorizations",
        compiles.count()
    );
    let retained_statements = |c: &Connection| {
        let mut count = 0;
        let mut statement = std::ptr::null_mut();
        loop {
            // SAFETY: enumerate SQLite-owned statements under the connection's
            // owner lock; no statement is executed or freed through this pointer.
            statement = unsafe { rusqlite::ffi::sqlite3_next_stmt(c.handle(), statement) };
            if statement.is_null() {
                break;
            }
            count += 1;
        }
        count
    };
    let cached = database.read(|c| Ok(retained_statements(c))).unwrap();
    assert!(
        (8..=16).contains(&cached),
        "hot metadata VM cache must be bounded: {cached}"
    );
    let saturated = database
        .read(|c| {
            for value in 0..32i64 {
                // Controlled constants intentionally create distinct cache keys.
                // Each scoped statement drops before the next one is prepared.
                let mut statement = c.prepare_cached(&format!("SELECT {value}"))?;
                assert_eq!(statement.query_row([], |r| r.get::<_, i64>(0))?, value);
            }
            Ok(retained_statements(c))
        })
        .unwrap();
    assert_eq!(
        saturated, 16,
        "distinct statements must evict older cached VMs"
    );
}
#[test]
fn exact_federation_and_count_prefixed_wire_digests_and_idle_zero_work() {
    let (_root, identity, state) = fixture();
    let conv = conversation(1);
    let mut ids = vec![];
    for seq in 1..=3 {
        let e = event(&identity, conv, seq);
        state.record(&e, 100, None).unwrap();
        ids.push(*e.id.as_bytes());
    }
    state.request_preparation_at(100).unwrap();
    assert_eq!(finish(&state).1, 3);
    ids.sort();
    let mut expected = Sha256::new();
    let mut wire = Sha256::new();
    wire.update(3u64.to_le_bytes());
    for id in ids {
        expected.update(id);
        wire.update(id)
    }
    let view = state.inventory_view_at(100).unwrap();
    let page = state.inventory_page_at(&view, None, 512, 100).unwrap();
    assert_eq!(page.entries.len(), 1);
    let digest: [u8; 32] = expected.finalize().into();
    assert_eq!(page.entries[0].digest, digest);
    let binding = state
        .profile
        .database()
        .read(|c| state.digest_binding_on(c, &conv, None))
        .unwrap()
        .unwrap();
    let cache = state
        .profile
        .database()
        .read(|c| state.cached_digest_on(c, &conv, binding))
        .unwrap()
        .unwrap();
    let expected_wire: [u8; 32] = wire.finalize().into();
    assert_eq!(cache.wire, expected_wire);
    assert_ne!(cache.wire, cache.federation);
    let before = state
        .profile
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    let mut worker = DigestWorker::default();
    for _ in 0..3 {
        let p = worker.step_at(&state, 100).unwrap();
        assert_eq!(
            (p.examined, p.hashed, p.published, p.ready, p.pending),
            (0, 0, false, false, false)
        );
    }
    assert_eq!(
        state
            .profile
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap(),
        before
    );
}
#[test]
fn selected_digest_is_append_stable_but_not_yet_selected_append_retries() {
    let (_root, identity, state) = fixture();
    let conv = conversation(1);
    state.record(&event(&identity, conv, 1), 100, None).unwrap();
    state.request_preparation_at(100).unwrap();
    finish(&state);
    let view = state.inventory_view_at(100).unwrap();
    let selected = state.inventory_page_at(&view, None, 512, 100).unwrap();
    state.record(&event(&identity, conv, 2), 100, None).unwrap();
    state.validate_inventory_at(&view, 100).unwrap();
    assert_eq!(selected.entries.len(), 1);
    assert!(matches!(
        state.inventory_page_at(&view, None, 512, 100),
        Err(LogError::StaleCursor)
    ));
    finish(&state);
    let fresh = state.inventory_view_at(100).unwrap();
    assert_ne!(
        state
            .inventory_page_at(&fresh, None, 512, 100)
            .unwrap()
            .entries[0]
            .digest,
        selected.entries[0].digest
    );
}
#[test]
fn raw_empty_inventory_continuation_and_cross_view_cursor_rejection() {
    let (_root, identity, state) = fixture();
    state
        .record(
            &event(&identity, ConversationId::new([1; 32]), 1),
            100,
            None,
        )
        .unwrap();
    state
        .record(&event(&identity, conversation(1), 1), 100, None)
        .unwrap();
    state.request_preparation_at(100).unwrap();
    finish(&state);
    let view = state.inventory_view_at(100).unwrap();
    let empty = state.inventory_page_at(&view, None, 1, 100).unwrap();
    assert_eq!(empty.examined, 1);
    assert!(empty.entries.is_empty());
    assert!(empty.after.is_some());
    assert_eq!(
        state
            .inventory_page_at(&view, empty.after.as_ref(), 1, 100)
            .unwrap()
            .entries
            .len(),
        1
    );
    let other = state.inventory_view_at(100).unwrap();
    assert!(matches!(
        state.inventory_page_at(&other, empty.after.as_ref(), 1, 100),
        Err(LogError::StaleCursor)
    ));
}
#[test]
fn origin_and_expiry_invalidate_inventory_but_touch_does_not() {
    let (_root, identity, state) = fixture();
    let e = event(&identity, conversation(1), 1);
    state.record(&e, 100, None).unwrap();
    state.request_preparation_at(100).unwrap();
    finish(&state);
    let view = state.inventory_view_at(100).unwrap();
    state.record(&e, 100, None).unwrap();
    state.validate_inventory_at(&view, 100).unwrap();
    state.record(&e, 100, Some(90)).unwrap();
    assert!(matches!(
        state.validate_inventory_at(&view, 100),
        Err(LogError::StaleCursor)
    ));
    let view = state.inventory_view_at(100).unwrap();
    assert!(matches!(
        state.validate_inventory_at(&view, 90 + crate::postoffice::MAX_RELAY_AGE.as_secs()),
        Err(LogError::StaleCursor)
    ));
    assert!(
        matches!(
            state.validate_inventory_at(&view, 100),
            Err(LogError::StaleCursor)
        ),
        "rollback cannot revive expired view"
    );
}
#[test]
fn malformed_or_ahead_cache_never_becomes_empty_success() {
    for sql in [
        "UPDATE relay_federation_digests SET revision=revision+1",
        "PRAGMA ignore_check_constraints=ON; UPDATE relay_federation_digests SET digest=x'00'; PRAGMA ignore_check_constraints=OFF",
    ] {
        let (_root, identity, state) = fixture();
        state
            .record(&event(&identity, conversation(1), 1), 100, None)
            .unwrap();
        state.request_preparation_at(100).unwrap();
        finish(&state);
        state
            .profile
            .database()
            .transaction(|c| {
                c.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
        let view = state.inventory_view_at(100).unwrap();
        assert!(state.inventory_page_at(&view, None, 512, 100).is_err());
    }
}
#[test]
fn ordinary_metadata_demand_hashes_without_starting_global_discovery() {
    let (_root, identity, state) = fixture();
    let conv = ConversationId::new([1; 32]);
    state.record(&event(&identity, conv, 1), 100, None).unwrap();
    let view = state.network_view_at(&conv, 100).unwrap();
    assert!(
        state
            .network_metadata_at(&view, 100)
            .unwrap()
            .fingerprint
            .is_none()
    );
    let mut worker = DigestWorker::default();
    let p = worker.step_at(&state, 100).unwrap();
    assert_eq!((p.examined, p.hashed, p.published), (0, 1, true));
    let fresh = state.network_view_at(&conv, 100).unwrap();
    assert!(
        state
            .network_metadata_at(&fresh, 100)
            .unwrap()
            .fingerprint
            .is_some()
    );
    let phase = state
        .profile
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT phase FROM relay_digest_preparation WHERE singleton=1",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(phase, 0);
}
#[test]
fn changed_active_hash_rotates_behind_other_demanded_mailboxes() {
    let (_root, identity, state) = fixture();
    let first = conversation(1);
    let second = conversation(2);
    for seq in 1..=513 {
        state
            .record(&event(&identity, first, seq), 100, None)
            .unwrap();
    }
    state
        .record(&event(&identity, second, 1), 100, None)
        .unwrap();
    state.request_preparation_at(100).unwrap();
    let mut worker = DigestWorker::default();
    assert_eq!(worker.step_at(&state, 100).unwrap().hashed, 512);
    state
        .record(&event(&identity, first, 514), 100, None)
        .unwrap();
    let p = worker.step_at(&state, 100).unwrap();
    assert_eq!(
        (p.hashed, p.published),
        (1, true),
        "busy first mailbox must not starve a healthy second mailbox"
    );
    finish(&state);
}
#[test]
fn cold_hundred_thousand_mailboxes_discovery_is_linear_across_full_queue_and_restart() {
    let (_root, identity, state) = fixture();
    let template = event(&identity, conversation(0), 1);
    state.record(&template, 100, None).unwrap();
    // Synthetic metadata fixtures exercise the producer, not signed-body replay.
    // The separate digest fixture above proves actual exact IDs/hash semantics.
    state.profile.database().transaction(|c|{
        let mut arrivals=c.prepare("INSERT INTO relay_mailboxes SELECT domain,?1,first_seen,touched,pending FROM relay_mailboxes WHERE domain=?2 AND conversation=?3")?;
        let mut metadata=c.prepare("INSERT INTO event_conversations SELECT domain,?1,lifecycle,destructive_epoch,revision,event_count,stored_bytes,estimated_bytes,max_lamport,v2_count,v3_count,v4_count,fingerprint_revision,fingerprint FROM event_conversations WHERE domain=?2 AND conversation=?3")?;
        let mut retained=c.prepare("INSERT INTO retained_events SELECT domain,?1,?2,author,seq,lamport,wall_clock,kind,parent_count,encoded_bytes,ciphertext_bytes,signature_bytes,stored_bytes,?3,body FROM retained_events WHERE domain=?4 AND id=?5")?;
        for n in 1..100000u32 {
            let conv=conversation(n);let mut event_id=[0u8;32];event_id[28..].copy_from_slice(&n.to_be_bytes());
            arrivals.execute(params![conv.as_bytes().as_slice(),RELAY_ARRIVALS.as_slice(),template.conversation_id.as_bytes().as_slice()])?;
            metadata.execute(params![conv.as_bytes().as_slice(),RELAY_EVENTS.0.as_slice(),template.conversation_id.as_bytes().as_slice()])?;
            retained.execute(params![event_id.as_slice(),conv.as_bytes().as_slice(),i64::from(n),RELAY_EVENTS.0.as_slice(),template.id.as_bytes().as_slice()])?;
        }
        c.execute("UPDATE event_domains SET next_commit=100000,event_count=100000,estimated_bytes=estimated_bytes*100000 WHERE domain=?1",[RELAY_EVENTS.0.as_slice()])?;Ok(())
    }).unwrap();
    state.request_preparation_at(100).unwrap();
    let mut total = 0;
    let mut pauses = 0;
    for turn in 0..1000 {
        let p = state
            .policy_transaction(100, |c, cutoff| {
                let mut p = PreparationProgress::default();
                state.produce_on(c, cutoff, &mut p)?;
                Ok(p)
            })
            .unwrap();
        total += p.examined;
        assert!(p.examined <= 512);
        if turn % 3 == 2 {
            let before = state
                .profile
                .database()
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT scan_after,queued FROM relay_digest_preparation WHERE singleton=1",
                        [],
                        |r| Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, i64>(1)?)),
                    )?)
                })
                .unwrap();
            if before.1 == 1024 {
                pauses += 1;
                let blocked = state
                    .policy_transaction(100, |c, cutoff| {
                        let mut p = PreparationProgress::default();
                        state.produce_on(c, cutoff, &mut p)?;
                        Ok(p)
                    })
                    .unwrap();
                assert_eq!(blocked.examined, 0);
                // Simulate worker completion, then reconstruct producer facade;
                // its only discovery state is durable SQL, not connection RAM.
                state.profile.database().transaction(|c|{
                    c.execute("INSERT INTO relay_federation_digests SELECT d.conversation,e.revision,e.destructive_epoch,zeroblob(32),zeroblob(32) FROM relay_digest_demands d JOIN event_conversations e ON e.domain=?1 AND e.conversation=d.conversation ON CONFLICT(conversation) DO NOTHING",[RELAY_EVENTS.0.as_slice()])?;
                    c.execute("DELETE FROM relay_digest_demands",[])?;c.execute("UPDATE relay_digest_preparation SET queued=0 WHERE singleton=1",[])?;Ok(())
                }).unwrap();
                let restarted = IndexedRelayState::from_profile(
                    state.profile.clone(),
                    RelayAdmission::LegacyCompatible,
                )
                .unwrap();
                let persisted = restarted
                    .profile
                    .database()
                    .read(|c| {
                        Ok(c.query_row(
                            "SELECT scan_after FROM relay_digest_preparation WHERE singleton=1",
                            [],
                            |r| r.get::<_, Option<Vec<u8>>>(0),
                        )?)
                    })
                    .unwrap();
                assert_eq!(persisted, before.0);
            }
        }
        let phase = state
            .profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT phase FROM relay_digest_preparation WHERE singleton=1",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        if phase == 2 {
            break;
        }
    }
    assert!(pauses > 10);
    assert_eq!(
        total, 100000,
        "cold discovery must examine each mailbox once"
    );
    // One ordinary append is followed from the global commit tail, with zero
    // cold-prefix discovery, even if other scopes have been logically deleted.
    state
        .record(&event(&identity, conversation(0), 2), 100, None)
        .unwrap();
    state
        .profile
        .database()
        .transaction(|c| {
            c.execute("DELETE FROM relay_digest_demands", [])?;
            c.execute(
                "UPDATE relay_digest_preparation SET queued=0 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let tail = state
        .policy_transaction(100, |c, cutoff| {
            let mut p = PreparationProgress::default();
            state.produce_on(c, cutoff, &mut p)?;
            Ok(p)
        })
        .unwrap();
    assert_eq!(tail.examined, 1);
    let idle = state
        .policy_transaction(100, |c, cutoff| {
            let mut p = PreparationProgress::default();
            state.produce_on(c, cutoff, &mut p)?;
            Ok(p)
        })
        .unwrap();
    assert_eq!(idle.examined, 0);
}
#[test]
fn digest_publications_coalesce_one_preparation_ready_wake() {
    let (_root, identity, state) = fixture();
    for n in 0..3 {
        state
            .record(&event(&identity, conversation(n), 1), 100, None)
            .unwrap();
    }
    state.request_preparation_at(100).unwrap();
    let changed = state.subscribe_changes();
    let before = *changed.borrow();
    let mut worker = DigestWorker::default();
    let first = worker.step_at(&state, 100).unwrap();
    assert!(first.published && !first.ready);
    assert_eq!(
        *changed.borrow(),
        before,
        "per-cache completion must not trigger full exchange retries"
    );
    assert!(!worker.step_at(&state, 100).unwrap().ready);
    assert_eq!(*changed.borrow(), before);
    assert!(worker.step_at(&state, 100).unwrap().ready);
    assert_eq!(*changed.borrow(), before + 1);
}
#[test]
fn stable_large_job_keeps_sha_prefix_until_complete_with_other_jobs_queued() {
    let (_root, identity, state) = fixture();
    let first = conversation(1);
    for seq in 1..=1025 {
        state
            .record(&event(&identity, first, seq), 100, None)
            .unwrap();
    }
    state
        .record(&event(&identity, conversation(2), 1), 100, None)
        .unwrap();
    state.request_preparation_at(100).unwrap();
    let mut worker = DigestWorker::default();
    let a = worker.step_at(&state, 100).unwrap();
    let b = worker.step_at(&state, 100).unwrap();
    let c = worker.step_at(&state, 100).unwrap();
    assert_eq!((a.hashed, b.hashed, c.hashed), (512, 512, 1));
    assert!(!a.published && !b.published && c.published);
    let small = worker.step_at(&state, 100).unwrap();
    assert_eq!(
        (small.hashed, small.published, small.ready),
        (1, true, true)
    );
}
#[test]
fn finite_mismatch_uses_staged_sent_digest_after_current_cache_is_overwritten() {
    use crate::postoffice::storage::federation::{Direction, SessionOwner};
    use std::time::Instant;
    let (_root, identity, state) = fixture();
    let conv = conversation(1);
    state.record(&event(&identity, conv, 1), 100, None).unwrap();
    state.request_preparation_at(100).unwrap();
    finish(&state);
    let owner = SessionOwner::begin(
        state.profile.clone(),
        Direction::Outbound,
        100,
        Instant::now(),
    )
    .unwrap();
    let lease = owner.lease();
    let view = state.inventory_view_at(100).unwrap();
    let sent = state
        .selected_inventory_page_at(&view, &lease, None, 512, 100)
        .unwrap();
    assert!(sent.after.is_none());
    state
        .finish_inventory_frame_at(&view, &lease, sent.entries.len(), true, 100)
        .unwrap();
    lease.receive_page(&sent.entries, true, 100).unwrap();
    state.record(&event(&identity, conv, 2), 100, None).unwrap();
    finish(&state);
    let current = state.inventory_view_at(100).unwrap();
    assert_ne!(
        state
            .inventory_page_at(&current, None, 512, 100)
            .unwrap()
            .entries[0]
            .digest,
        sent.entries[0].digest
    );
    state
        .begin_inventory_mismatch_at(&view, &lease, None, 100)
        .unwrap();
    let mismatch = state
        .inventory_mismatch_step_at(&view, &lease, 512, 16, 100)
        .unwrap();
    assert!(
        mismatch.done && mismatch.ids.is_empty(),
        "equality describes the finite sent set, not current custody"
    );
    state
        .record(&event(&identity, conv, 1), 100, Some(90))
        .unwrap();
    assert!(matches!(
        state.inventory_mismatch_step_at(&view, &lease, 512, 16, 100),
        Err(LogError::StaleCursor)
    ));
}
#[test]
fn selected_local_snapshot_real_commit_fault_rolls_back_charges_and_rows() {
    use crate::postoffice::storage::federation::{Direction, SessionOwner};
    use std::time::Instant;
    let (root, identity, state) = fixture();
    state
        .record(&event(&identity, conversation(1), 1), 100, None)
        .unwrap();
    state.request_preparation_at(100).unwrap();
    finish(&state);
    let owner = SessionOwner::begin(
        state.profile.clone(),
        Direction::Outbound,
        100,
        Instant::now(),
    )
    .unwrap();
    let lease = owner.lease();
    let view = state.inventory_view_at(100).unwrap();
    lease.receive_page(&[], true, 100).unwrap();
    super::super::super::role_tests::CUT.with(|v| v.set(Some("commit")));
    assert!(
        state
            .selected_inventory_page_at(&view, &lease, None, 512, 100)
            .is_err()
    );
    super::super::super::role_tests::CUT.with(|v| v.set(None));
    drop(view);
    drop(lease);
    drop(owner);
    drop(state);
    let p = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &root.path().join("relay.log"),
        "pw",
        100,
    )
    .unwrap();
    p.database().read(|c|{let row:(i64,i64,i64)=c.query_row("SELECT session_rows,staged_rows,staged_bytes FROM relay_federation_runtime WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;assert_eq!(row,(1,0,1024));let locals:i64=c.query_row("SELECT count(*) FROM relay_federation_local_entries",[],|r|r.get(0))?;assert_eq!(locals,0);Ok(())}).unwrap();
}
#[test]
fn many_nonfederated_raw_slices_coalesce_one_finite_wire_frame() {
    use crate::postoffice::storage::federation::{Direction, SessionOwner, cleanup_step};
    use std::time::Instant;
    let (_root, identity, state) = fixture();
    for n in 0..300u32 {
        let mut conv = [0u8; 32];
        conv[28..].copy_from_slice(&n.to_be_bytes());
        state
            .record(&event(&identity, ConversationId::new(conv), 1), 100, None)
            .unwrap();
    }
    state
        .record(&event(&identity, conversation(1), 1), 100, None)
        .unwrap();
    state.request_preparation_at(100).unwrap();
    finish(&state);
    let owner = SessionOwner::begin(
        state.profile.clone(),
        Direction::Outbound,
        100,
        Instant::now(),
    )
    .unwrap();
    let lease = owner.lease();
    let view = state.inventory_view_at(100).unwrap();
    let mut after = None;
    let mut examined = 0;
    let mut emitted = 0;
    loop {
        let page = state
            .selected_inventory_page_at(&view, &lease, after.as_ref(), 1, 100)
            .unwrap();
        examined += page.examined;
        emitted += page.entries.len();
        if page.after.is_none() {
            break;
        }
        assert_eq!(emitted, 0);
        after = page.after;
    }
    assert_eq!((examined, emitted), (301, 1));
    state
        .finish_inventory_frame_at(&view, &lease, 1, true, 100)
        .unwrap();
    lease.receive_page(&[], true, 100).unwrap();
    state
        .profile
        .database()
        .read(|c| {
            let pages: i64 = c.query_row(
                "SELECT local_pages FROM relay_federation_sessions",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(pages, 1);
            let rows: i64 = c.query_row(
                "SELECT staged_rows FROM relay_federation_runtime WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(rows, 1);
            Ok(())
        })
        .unwrap();
    owner.cancel().unwrap();
    assert!(cleanup_step(&state.profile, 100).unwrap());
    state
        .profile
        .database()
        .read(|c| {
            let rows: i64 = c.query_row(
                "SELECT staged_rows FROM relay_federation_runtime WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(rows, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn gapped_commit_tail_reaches_finite_cutoff_then_observes_a_later_append() {
    let (_root, identity, state) = fixture();
    let conv = conversation(1);
    state.record(&event(&identity, conv, 1), 100, None).unwrap();
    state.request_preparation_at(100).unwrap();
    finish(&state);
    // A GC-shaped ordinal gap has no retained rows. Its exclusive next_commit
    // still binds the finite scan; a later append must receive that ordinal.
    state
        .profile
        .database()
        .transaction(|c| {
            c.execute(
                "UPDATE event_domains SET next_commit=1000 WHERE domain=?1",
                [RELAY_EVENTS.0.as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    let mut worker = DigestWorker::default();
    let empty = worker.step_at(&state, 100).unwrap();
    assert_eq!((empty.examined, empty.hashed), (0, 0));
    assert!(state.preparation_ready_at(100).unwrap());
    state.record(&event(&identity, conv, 2), 100, None).unwrap();
    let appended = worker.step_at(&state, 100).unwrap();
    assert_eq!((appended.examined, appended.hashed), (1, 2));
    assert!(appended.published);
    assert!(state.preparation_ready_at(100).unwrap());
    let idle = worker.step_at(&state, 100).unwrap();
    assert_eq!((idle.examined, idle.hashed, idle.pending), (0, 0, false));
}

#[test]
fn visibility_epoch_overflow_preserves_authentication_and_rolls_back_origin() {
    let (_root, identity, state) = fixture();
    let e = event(&identity, conversation(1), 1);
    state.record(&e, 100, None).unwrap();
    state
        .profile
        .database()
        .transaction(|c| {
            c.execute(
                "UPDATE relay_runtime SET inventory_visibility_epoch=9223372036854775807",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let mut forged = e.clone();
    forged.sig[0] ^= 1;
    assert!(matches!(
        state.record(&forged, 100, Some(90)),
        Err(LogError::BadSignature)
    ));
    assert!(matches!(
        state.record(&e, 100, Some(90)),
        Err(LogError::RepositoryCounterExhausted)
    ));
    assert_eq!(
        state
            .mailbox_metadata(&e.conversation_id)
            .unwrap()
            .unwrap()
            .first_seen,
        100
    );
    assert_eq!(state.physical_charge().unwrap().event_count, 1);
}
