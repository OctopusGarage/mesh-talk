use super::*;
use crate::{
    eventlog::ConversationId,
    identity::device::DeviceIdentity,
    postoffice::{
        FederatedMailbox,
        storage::role::{IndexedRelayState, RelayAdmission},
    },
    storage::profile::{ProfileStorageLease, context::ProfileStorage},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn profile(root: &tempfile::TempDir, identity: &DeviceIdentity) -> Arc<ProfileStorage> {
    ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &root.path().join("relay.log"),
        "pw",
        100,
    )
    .unwrap()
}
fn entry(key: u32, first: u64) -> FederatedMailbox {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(b"MTF3");
    bytes[4..8].copy_from_slice(&key.to_be_bytes());
    FederatedMailbox {
        id: ConversationId::new(bytes),
        digest: [7; 32],
        first_seen_secs: first,
    }
}
fn charges(profile: &ProfileStorage) -> (i64, i64, i64) {
    profile.database().read(|c| Ok(c.query_row("SELECT session_rows,staged_rows,staged_bytes FROM relay_federation_runtime WHERE singleton=1", [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)).unwrap()
}

/// Count real SQLite compilation at the selected storage boundary.
struct SqlCompiles<'a> {
    database: &'a EncryptedDatabase,
    count: Option<Box<CompileCounter>>,
}
struct CompileCounter {
    count: std::sync::atomic::AtomicUsize,
    guards: bool,
}
impl<'a> SqlCompiles<'a> {
    fn install(database: &'a EncryptedDatabase) -> Self {
        Self::install_for(database, false)
    }
    fn install_for(database: &'a EncryptedDatabase, guards: bool) -> Self {
        let count = Box::new(CompileCounter {
            count: std::sync::atomic::AtomicUsize::new(0),
            guards,
        });
        database
            .read(|c| {
                // SAFETY: the connection is locked and the boxed counter remains
                // alive until Drop unregisters the compilation callback.
                let result = unsafe {
                    rusqlite::ffi::sqlite3_set_authorizer(
                        c.handle(),
                        Some(staging_compile),
                        (&*count as *const CompileCounter).cast_mut().cast(),
                    )
                };
                assert_eq!(result, rusqlite::ffi::SQLITE_OK);
                Ok(())
            })
            .unwrap();
        Self {
            database,
            count: Some(count),
        }
    }
    fn take(&self) -> usize {
        self.count
            .as_ref()
            .unwrap()
            .count
            .swap(0, Ordering::Relaxed)
    }
}
impl Drop for SqlCompiles<'_> {
    fn drop(&mut self) {
        let removed = self.database.read(|c| {
            // SAFETY: removal is serialized before the callback data is freed.
            let result = unsafe {
                rusqlite::ffi::sqlite3_set_authorizer(c.handle(), None, std::ptr::null_mut())
            };
            assert_eq!(result, rusqlite::ffi::SQLITE_OK);
            Ok(())
        });
        if removed.is_err() {
            // A faulted connection cannot be entered to unregister its callback.
            Box::leak(self.count.take().unwrap());
        }
    }
}
unsafe extern "C" fn staging_compile(
    userdata: *mut std::ffi::c_void,
    action: std::ffi::c_int,
    table: *const std::ffi::c_char,
    column: *const std::ffi::c_char,
    _database: *const std::ffi::c_char,
    _view: *const std::ffi::c_char,
) -> std::ffi::c_int {
    if matches!(
        action,
        rusqlite::ffi::SQLITE_READ | rusqlite::ffi::SQLITE_INSERT | rusqlite::ffi::SQLITE_DELETE
    ) && !table.is_null()
    {
        // SAFETY: SQLite provides a live NUL-terminated table name; install and
        // Drop keep the boxed counter alive for the complete registration.
        let (table, count) = unsafe {
            (
                std::ffi::CStr::from_ptr(table).to_bytes(),
                &*userdata.cast::<CompileCounter>(),
            )
        };
        let selected = if count.guards {
            // SQLCipher may recompile bounded cursor SELECTs on LIMIT rebinding.
            // Observe fixed authority/header reads rather than cursor-only
            // projections; session staging has its separate page-cost regression.
            let column = if column.is_null() {
                &[][..]
            } else {
                // SAFETY: SQLite supplies a live column name for READ.
                unsafe { std::ffi::CStr::from_ptr(column).to_bytes() }
            };
            action == rusqlite::ffi::SQLITE_READ
                && (matches!(
                    table,
                    b"relay_runtime"
                        | b"event_conversations"
                        | b"event_domains"
                        | b"event_retirement_jobs"
                        | b"relay_digest_preparation"
                ) || (table == b"relay_mailboxes"
                    && matches!(column, b"first_seen" | b"touched" | b"pending"))
                    || (table == b"retained_events"
                        && !matches!(
                            column,
                            b"commit_ordinal" | b"id" | b"domain" | b"conversation"
                        ))
                    || (table == b"relay_federation_sessions" && column == b"session_epoch"))
        } else {
            matches!(
                table,
                b"relay_federation_entries" | b"relay_federation_local_entries"
            )
        };
        if selected {
            count.count.fetch_add(1, Ordering::Relaxed);
        }
    }
    rusqlite::ffi::SQLITE_OK
}

#[test]
fn federation_row_work_compiles_constant_per_bounded_page() {
    let measure = |size: u32| {
        let root = tempfile::tempdir().unwrap();
        let profile = profile(&root, &DeviceIdentity::generate());
        let owner =
            SessionOwner::begin(profile.clone(), Direction::Outbound, 100, Instant::now()).unwrap();
        let lease = owner.lease();
        let entries: Vec<_> = (0..size)
            .map(|key| {
                let mut entry = entry(key, 90 + u64::from(key % 9));
                entry.digest = [key as u8; 32];
                entry
            })
            .collect();
        let compiles = SqlCompiles::install(profile.database());
        lease.receive_page(&entries, true, 100).unwrap();
        let received = compiles.take();
        profile
            .database()
            .transaction(|c| lease.stage_local_page_on(c, &entries, true, 100))
            .unwrap();
        let selected = compiles.take();
        assert_eq!(
            charges(&profile),
            (
                1,
                2 * i64::from(size),
                SESSION_BYTES + 2 * i64::from(size) * ENTRY_BYTES
            )
        );
        profile
            .database()
            .read(|c| {
                for table in ["relay_federation_entries", "relay_federation_local_entries"] {
                    let mut statement = c.prepare(&format!(
                        "SELECT conversation,digest,first_seen FROM {table} WHERE session_id=?1 ORDER BY conversation"
                    ))?;
                    let actual = statement
                        .query_map([lease.session_id], |r| {
                            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, Vec<u8>>(2)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    let expected: Vec<_> = entries.iter().map(|e| (e.id.as_bytes().to_vec(), e.digest.to_vec(), e.first_seen_secs.to_be_bytes().to_vec())).collect();
                    assert_eq!(actual, expected, "{table} preserves the exact selected inventory");
                }
                Ok(())
            })
            .unwrap();
        profile
            .database()
            .transaction(|c| lease.begin_mismatch_on(c, None))
            .unwrap();
        compiles.take();
        let mismatch = profile
            .database()
            .transaction(|c| lease.mismatch_step_on(c, 512, 16))
            .unwrap();
        let compared = compiles.take();
        assert!(mismatch.done && mismatch.ids.is_empty());
        assert_eq!(mismatch.examined, 2 * (size as usize + 1));
        drop(owner);
        assert!(cleanup_step(&profile, 100).unwrap());
        let cleaned = compiles.take();
        assert_eq!(charges(&profile), (0, 0, 0));
        [received, selected, compared, cleaned]
    };
    let one = measure(1);
    let many = measure(128);
    assert!(
        many.iter().zip(one).all(|(many, one)| *many <= one),
        "staging, selection, comparison and cleanup must compile per bounded operation, not per row: one={one:?}, many={many:?}"
    );
}

#[test]
fn warmed_session_guards_and_network_reads_reuse_bounded_compilation() {
    use crate::eventlog::sync::network::{ExportPageBudget, IdPageBudget};
    use crate::postoffice::storage::role::inventory::preparation::DigestWorker;
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = profile(&root, &identity);
    let role =
        IndexedRelayState::from_profile(profile.clone(), RelayAdmission::LegacyCompatible).unwrap();
    let events: Vec<_> = (1..=3).map(|key| event(&identity, key)).collect();
    for event in &events {
        role.record(event, 100, None).unwrap();
    }
    role.request_preparation_at(100).unwrap();
    let mut worker = DigestWorker::default();
    for _ in 0..100 {
        if role.preparation_ready_at(100).unwrap() {
            break;
        }
        worker.step_at(&role, 100).unwrap();
    }
    assert!(role.preparation_ready_at(100).unwrap());
    // Installing an authorizer invalidates existing VMs; install before warming.
    let compiles = SqlCompiles::install_for(profile.database(), true);
    let measure = || {
        let mut counts = Vec::new();
        for round in 0..6 {
            let owner =
                SessionOwner::begin(profile.clone(), Direction::Outbound, 100, Instant::now())
                    .unwrap();
            let lease = owner.lease();
            lease.receive_page(&[], true, 100).unwrap();
            compiles.take();
            assert!(!role.next_work_at(100).unwrap().has_work());
            assert!(!role.digest_work_pending().unwrap());
            assert!(role.preparation_ready_at(100).unwrap());
            let inventory = role.inventory_view_at(100).unwrap();
            let page = role
                .selected_inventory_page_at(&inventory, &lease, None, 512, 100)
                .unwrap();
            assert_eq!(page.entries.len(), events.len());
            role.finish_inventory_frame_at(&inventory, &lease, page.entries.len(), true, 100)
                .unwrap();
            for offset in 0..events.len() {
                let event = &events[(round + offset) % events.len()];
                let view = role
                    .network_view_with_session_at(&event.conversation_id, 100, Some(&lease))
                    .unwrap();
                assert_eq!(role.network_metadata_at(&view, 100).unwrap().count, 1);
                let have = role
                    .network_have_at(
                        &view,
                        None,
                        IdPageBudget {
                            ids: 8,
                            examined: 8,
                        },
                        100,
                    )
                    .unwrap();
                assert_eq!(have.items, vec![event.id]);
                assert!(have.next.is_none());
                // Exercise the bounded header selector without hydrating a body.
                assert!(matches!(
                    role.network_export_at(
                        &view,
                        &std::collections::HashSet::new(),
                        None,
                        ExportPageBudget {
                            examined: 8,
                            events: 8,
                            encoded_bytes: 1,
                            allocated_bytes: 1
                        },
                        100
                    ),
                    Err(LogError::PageBudgetTooSmall { .. })
                ));
                role.network_validate_at(&view, 100).unwrap();
            }
            counts.push(compiles.take());
            drop(owner);
            while cleanup_step(&profile, 100).unwrap() {}
        }
        counts
    };
    let counts = measure();
    let retained = profile
        .database()
        .read(|c| {
            let mut count = 0;
            let mut statement = std::ptr::null_mut();
            loop {
                // SAFETY: enumerate live SQLite-owned VMs while holding the connection
                // lock; the pointers are never executed, mutated or freed here.
                statement = unsafe { rusqlite::ffi::sqlite3_next_stmt(c.handle(), statement) };
                if statement.is_null() {
                    break;
                }
                count += 1;
            }
            Ok(count)
        })
        .unwrap();
    eprintln!("warm read authorizations={counts:?}; retained statement VMs={retained}");
    assert!(
        retained <= 64,
        "warm statement retention stays bounded: {retained}"
    );
    assert!(counts[0] > 0, "the observer must see cold SQL compilation");
    assert!(
        counts[1..].iter().all(|count| *count == 0),
        "warm current-authority/network queries must not compile again across conversations and sessions: {counts:?}"
    );
    // Reproduce the former eviction pressure with the same queries and binds.
    profile
        .database()
        .read(|c| {
            c.set_prepared_statement_cache_capacity(16);
            Ok(())
        })
        .unwrap();
    let constrained = measure();
    eprintln!("16-entry read authorizations={constrained:?}");
    assert!(
        constrained[1..].iter().all(|count| *count > 0),
        "the workload must exercise the working set that displaced the old 16-entry cache: {constrained:?}"
    );
}

#[test]
fn warmed_session_guards_observe_live_authority_mutations() {
    use crate::eventlog::sync::network::IdPageBudget;
    for mutation in [
        "readiness",
        "cutoff",
        "session",
        "epoch",
        "capability",
        "cancel",
    ] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = profile(&root, &identity);
        let role =
            IndexedRelayState::from_profile(profile.clone(), RelayAdmission::LegacyCompatible)
                .unwrap();
        let event = event(&identity, 1);
        role.record(&event, 100, None).unwrap();
        let mut owner = Some(
            SessionOwner::begin(profile.clone(), Direction::Outbound, 100, Instant::now()).unwrap(),
        );
        let lease = owner.as_ref().unwrap().lease();
        lease.receive_page(&[], true, 100).unwrap();
        let view = role
            .network_view_with_session_at(&event.conversation_id, 100, Some(&lease))
            .unwrap();
        for _ in 0..2 {
            assert_eq!(role.network_metadata_at(&view, 100).unwrap().count, 1);
            role.network_validate_at(&view, 100).unwrap();
            assert_eq!(
                role.network_have_at(
                    &view,
                    None,
                    IdPageBudget {
                        ids: 8,
                        examined: 8
                    },
                    100
                )
                .unwrap()
                .items,
                vec![event.id]
            );
        }
        if mutation == "cancel" {
            drop(owner.take());
        } else {
            profile.database().transaction(|c| {
                match mutation {
                    "readiness" => { c.execute("UPDATE relay_runtime SET ready=0 WHERE singleton=1", [])?; }
                    "cutoff" => { c.execute("UPDATE relay_runtime SET expired_cutoff=?1 WHERE singleton=1", [100u64.to_be_bytes().as_slice()])?; }
                    "session" => { c.execute("UPDATE relay_federation_sessions SET session_epoch=session_epoch+1 WHERE session_id=?1", [lease.session_id])?; }
                    "epoch" => { c.execute("UPDATE event_conversations SET destructive_epoch=destructive_epoch+1 WHERE domain=?1 AND conversation=?2", params![RELAY_EVENTS.0.as_slice(), event.conversation_id.as_bytes().as_slice()])?; }
                    "capability" => { c.execute("UPDATE event_conversations SET v4_count=1 WHERE domain=?1 AND conversation=?2", params![RELAY_EVENTS.0.as_slice(), event.conversation_id.as_bytes().as_slice()])?; }
                    _ => unreachable!(),
                }
                Ok(())
            }).unwrap();
        }
        assert!(
            role.network_metadata_at(&view, 100).is_err(),
            "{mutation}: warmed metadata must recheck authority"
        );
        assert!(
            role.network_validate_at(&view, 100).is_err(),
            "{mutation}: warmed validation must recheck authority"
        );
        assert!(
            role.network_have_at(
                &view,
                None,
                IdPageBudget {
                    ids: 8,
                    examined: 8
                },
                100
            )
            .is_err(),
            "{mutation}: warmed Have must not disclose IDs"
        );
    }
}

#[test]
fn owner_drop_cancels_all_cloned_leases_but_preserves_stale_charges() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    let clone = lease.clone();
    lease.receive_page(&[entry(1, 90)], true, 100).unwrap();
    let before = charges(&p);
    drop(owner);
    assert!(clone.origin(&entry(1, 90).id, 100).is_err());
    assert_eq!(
        charges(&p),
        before,
        "cancellation cannot free durable charged staging"
    );
    assert!(cleanup_step(&p, 131).unwrap());
    assert_eq!(charges(&p), (0, 0, 0));
}

#[test]
fn staging_requires_terminal_and_deduplicates_only_unique_charge() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    let first: Vec<_> = (0..512).map(|i| entry(i, 95)).collect();
    lease.receive_page(&first, false, 100).unwrap();
    assert!(
        lease.origin(&first[0].id, 100).is_err(),
        "incomplete inventory cannot authorize origin"
    );
    lease
        .receive_page(&[entry(0, 90), entry(512, 95)], true, 100)
        .unwrap();
    assert_eq!(charges(&p), (1, 513, 1024 + 513 * 192));
    assert_eq!(lease.origin(&first[0].id, 100).unwrap(), Some(90));
    assert!(
        lease.receive_page(&[], true, 100).is_err(),
        "terminal inventory is immutable"
    );
    let role = IndexedRelayState::from_profile(p, RelayAdmission::LegacyCompatible).unwrap();
    assert!(
        role.mailbox_metadata_at(&first[0].id, 100)
            .unwrap()
            .is_none(),
        "unknown inventory never starts custody"
    );
}

#[test]
fn monotonic_deadline_cannot_be_extended_by_wall_clock_rollback() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(
        p,
        Direction::Outbound,
        100,
        Instant::now() - Duration::from_secs(31),
    )
    .unwrap();
    assert!(owner.lease().receive_page(&[], true, 1).is_err());
}

#[test]
fn new_profile_owner_invalidates_old_epoch_without_releasing_old_charge() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let p = profile(&root, &identity);
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    owner
        .lease()
        .receive_page(&[entry(1, 90)], true, 100)
        .unwrap();
    let before = charges(&p);
    let old_epoch = p.federation_owner_epoch().unwrap();
    drop(owner);
    drop(p);
    let p = profile(&root, &identity);
    assert_eq!(p.federation_owner_epoch().unwrap(), old_epoch + 1);
    assert_eq!(charges(&p), before);
    assert!(cleanup_step(&p, 100).unwrap());
    assert_eq!(charges(&p), (0, 0, 0));
}

#[test]
fn global_resource_reservations_fail_before_partial_page_mutation() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    p.database().transaction(|c|{ c.execute("UPDATE relay_federation_runtime SET staged_rows=262144,staged_bytes=67108800 WHERE singleton=1", [])?; Ok(()) }).unwrap();
    let before = charges(&p);
    assert!(matches!(
        lease.receive_page(&[entry(1, 90)], true, 100),
        Err(LogError::RelayCapacityUnavailable)
    ));
    assert_eq!(charges(&p), before);
    p.database()
        .read(|c| {
            let pages: i64 = c.query_row(
                "SELECT receive_pages FROM relay_federation_sessions",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(pages, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn dropped_owner_cleanup_makes_progress_after_wall_clock_rollback() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    owner
        .lease()
        .receive_page(&[entry(1, 90)], true, 100)
        .unwrap();
    drop(owner);
    assert!(
        cleanup_step(&p, 1).unwrap(),
        "owner cancellation must not wait for UTC to catch up"
    );
    assert_eq!(charges(&p), (0, 0, 0));
}

fn event(identity: &DeviceIdentity, key: u32) -> crate::eventlog::Event {
    crate::eventlog::Event::new(
        identity,
        entry(key, 90).id,
        1,
        vec![],
        1,
        1,
        crate::eventlog::EventKind::Message,
        vec![42],
    )
}
#[test]
fn session_origin_and_record_share_authority_and_the_acceptance_transaction() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let p = profile(&root, &identity);
    let role =
        IndexedRelayState::from_profile(p.clone(), RelayAdmission::LegacyCompatible).unwrap();
    let e = event(&identity, 1);
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    lease.receive_page(&[entry(1, 90)], true, 100).unwrap();
    assert_eq!(
        role.record_with_session_at(&e, 100, &lease).unwrap(),
        crate::eventlog::AppendOutcome::Appended
    );
    assert_eq!(
        role.mailbox_metadata_at(&e.conversation_id, 100)
            .unwrap()
            .unwrap()
            .first_seen,
        90
    );
    let charge = role.physical_charge().unwrap();
    drop(owner);
    assert!(
        role.record_with_session_at(&event(&identity, 2), 100, &lease)
            .is_err()
    );
    assert_eq!(role.physical_charge().unwrap(), charge);
    assert!(
        role.mailbox_metadata_at(&entry(2, 90).id, 100)
            .unwrap()
            .is_none()
    );
}
#[test]
fn known_origin_correction_is_bounded_metadata_and_unknown_stays_unknown() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let p = profile(&root, &identity);
    let role =
        IndexedRelayState::from_profile(p.clone(), RelayAdmission::LegacyCompatible).unwrap();
    let e = event(&identity, 1);
    role.record(&e, 100, None).unwrap();
    let before = role
        .mailbox_metadata_at(&e.conversation_id, 100)
        .unwrap()
        .unwrap();
    let owner = SessionOwner::begin(p, Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    lease
        .receive_page(&[entry(1, 90), entry(2, 80)], true, 100)
        .unwrap();
    let page = role.correct_known_origins_at(&lease, None, 1, 100).unwrap();
    assert_eq!(page.examined, 1);
    assert!(page.after.is_some());
    let page = role
        .correct_known_origins_at(&lease, page.after.as_ref(), 1, 100)
        .unwrap();
    assert_eq!(page.examined, 1);
    let after = role
        .mailbox_metadata_at(&e.conversation_id, 100)
        .unwrap()
        .unwrap();
    assert_eq!(after.first_seen, 90);
    assert_eq!(after.touched, before.touched);
    assert_eq!(after.retained, before.retained);
    assert!(
        role.mailbox_metadata_at(&entry(2, 80).id, 100)
            .unwrap()
            .is_none()
    );
}

#[test]
fn full_hint_queue_falls_back_to_monotonic_authority_without_waiting_for_utc() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    owner
        .lease()
        .receive_page(&[entry(1, 90)], true, 100)
        .unwrap();
    for i in 0..512 {
        p.federation_cancellations().push((1000 + i, 1));
    }
    drop(owner);
    for _ in 0..514 {
        if !cleanup_step(&p, 1).unwrap() {
            break;
        }
    }
    assert_eq!(
        charges(&p),
        (0, 0, 0),
        "full hints cannot lose current-process cancellation authority"
    );
}
#[test]
fn late_owner_drop_after_reclamation_is_an_idempotent_missing_row_hint() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    owner
        .lease()
        .receive_page(&[entry(1, 90)], true, 100)
        .unwrap();
    assert!(cleanup_step(&p, 131).unwrap());
    assert_eq!(charges(&p), (0, 0, 0));
    drop(owner);
    assert!(cleanup_step(&p, 1).unwrap());
    assert_eq!(charges(&p), (0, 0, 0));
    let changes = p.database().read(|c| Ok(c.total_changes())).unwrap();
    assert!(!cleanup_step(&p, 1).unwrap());
    assert_eq!(
        p.database().read(|c| Ok(c.total_changes())).unwrap(),
        changes
    );
}

#[test]
fn registry_commit_failure_returns_no_owner_and_releases_reserved_slot() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let p = profile(&root, &identity);
    crate::postoffice::storage::role_tests::CUT.with(|v| v.set(Some("commit")));
    let failed = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now());
    crate::postoffice::storage::role_tests::CUT.with(|v| v.set(None));
    assert!(failed.is_err());
    assert!(
        p.federation_cancellations()
            .registry
            .lock()
            .unwrap()
            .is_empty()
    );
    assert!(p.federation_cancellations().peek().is_none());
    assert!(
        SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).is_err(),
        "faulted DB handle cannot admit more sessions"
    );
    drop(p);
    let p = profile(&root, &identity);
    assert_eq!(charges(&p), (0, 0, 0));
    let owner = SessionOwner::begin(p, Direction::Inbound, 100, Instant::now()).unwrap();
    owner.lease().receive_page(&[], true, 100).unwrap();
}
#[test]
fn registry_finds_expired_session_behind_healthy_session_with_utc_rollback() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let healthy = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let expired = SessionOwner::begin(
        p.clone(),
        Direction::Inbound,
        100,
        Instant::now() - Duration::from_secs(31),
    )
    .unwrap();
    assert!(cleanup_step(&p, 1).unwrap());
    assert_eq!(charges(&p), (1, 0, 1024));
    healthy.lease().receive_page(&[], true, 100).unwrap();
    drop(expired);
    assert!(cleanup_step(&p, 1).unwrap());
    assert_eq!(charges(&p), (1, 0, 1024));
}

#[test]
fn live_direction_limits_and_abandoned_session_rows_are_independent() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let mut live = Vec::new();
    for _ in 0..256 {
        live.push(SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap());
    }
    for _ in 0..8 {
        live.push(
            SessionOwner::begin(p.clone(), Direction::Outbound, 100, Instant::now()).unwrap(),
        );
    }
    for direction in [Direction::Inbound, Direction::Outbound] {
        assert!(matches!(
            SessionOwner::begin(p.clone(), direction, 100, Instant::now()),
            Err(LogError::RelayCapacityUnavailable)
        ));
    }
    for owner in &live {
        owner.cancel().unwrap();
    }
    for _ in 264..512 {
        let owner =
            SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
        owner.cancel().unwrap();
        live.push(owner);
    }
    assert_eq!(charges(&p), (512, 0, 512 * 1024));
    assert!(matches!(
        SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()),
        Err(LogError::RelayCapacityUnavailable)
    ));
    assert!(cleanup_step(&p, 1).unwrap());
    assert_eq!(charges(&p), (511, 0, 511 * 1024));
    let next = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    assert_eq!(charges(&p), (512, 0, 512 * 1024));
    drop(next);
}
#[test]
fn every_received_duplicate_counts_toward_page_and_entry_limits() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    let page = vec![entry(1, 90); 512];
    for _ in 0..255 {
        lease.receive_page(&page, false, 100).unwrap();
    }
    assert!(
        lease.receive_page(&page, false, 100).is_err(),
        "the final allowed page must be terminal"
    );
    assert!(lease.origin(&entry(1, 90).id, 100).is_err());
    lease.receive_page(&page, true, 100).unwrap();
    assert!(lease.receive_page(&[], true, 100).is_err());
    assert_eq!(charges(&p), (1, 1, 1216));
    p.database()
        .read(|c| {
            let counts: (i64, i64) = c.query_row(
                "SELECT receive_pages,receive_entries FROM relay_federation_sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(counts, (256, 131072));
            Ok(())
        })
        .unwrap();
}
#[test]
fn invalid_or_conflicting_page_rolls_back_all_entries_and_reservations() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    let before = charges(&p);
    let mut conflicting = entry(1, 90);
    conflicting.digest = [8; 32];
    let mut wrong_id = entry(2, 90);
    wrong_id.id = ConversationId::new([0; 32]);
    for page in [
        vec![entry(1, 90), conflicting.clone()],
        vec![entry(1, 90), wrong_id],
        vec![entry(1, 90), entry(2, 401)],
        vec![entry(1, 90); 513],
    ] {
        assert!(lease.receive_page(&page, true, 100).is_err());
        assert_eq!(charges(&p), before);
    }
    lease.receive_page(&[entry(1, 90)], false, 100).unwrap();
    let before = charges(&p);
    assert!(
        lease
            .receive_page(&[entry(2, 90), conflicting], true, 100)
            .is_err()
    );
    assert_eq!(charges(&p), before);
    assert!(lease.origin(&entry(1, 90).id, 100).is_err());
}
#[test]
fn staged_origin_age_is_revalidated_at_actual_event_ingest() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let p = profile(&root, &identity);
    let role =
        IndexedRelayState::from_profile(p.clone(), RelayAdmission::LegacyCompatible).unwrap();
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    lease.receive_page(&[entry(1, 90)], true, 100).unwrap();
    assert!(
        role.record_with_session_at(&event(&identity, 1), 90 + MAX_RELAY_AGE.as_secs(), &lease)
            .is_err()
    );
    assert_eq!(role.physical_charge().unwrap().event_count, 0);
    assert_eq!(charges(&p), (1, 1, 1216));
}
#[test]
fn explicit_cancel_wakes_worker_before_sql_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let mut wake = p.federation_cancellations().subscribe_changes();
    wake.borrow_and_update();
    owner.cancel().unwrap();
    assert!(
        wake.has_changed().unwrap(),
        "cancellation must wake a sleeping worker"
    );
    assert!(cleanup_step(&p, 1).unwrap());
    assert_eq!(charges(&p), (0, 0, 0));
}
#[test]
fn warm_open_rejects_malformed_scalar_federation_authority_without_scanning() {
    for mutation in [
        "PRAGMA ignore_check_constraints=ON;UPDATE relay_federation_runtime SET owner_epoch=-1;PRAGMA ignore_check_constraints=OFF",
        "UPDATE relay_federation_runtime SET session_rows=1",
        "DELETE FROM relay_digest_preparation",
        "PRAGMA ignore_check_constraints=ON;UPDATE relay_digest_preparation SET queued=-1;PRAGMA ignore_check_constraints=OFF",
        "PRAGMA ignore_check_constraints=ON;UPDATE relay_runtime SET inventory_visibility_epoch=-1;PRAGMA ignore_check_constraints=OFF",
    ] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let p = profile(&root, &identity);
        p.database()
            .transaction(|c| {
                c.execute_batch(mutation)?;
                Ok(())
            })
            .unwrap();
        drop(p);
        assert!(
            ProfileStorage::open_relay(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &identity.public(),
                &root.path().join("relay.log"),
                "pw",
                100
            )
            .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn session_cleanup_releases_only_a_bounded_page_and_preserves_remaining_charge() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Inbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    for page in 0..2 {
        let entries: Vec<_> = (0..512).map(|i| entry(page * 512 + i, 90)).collect();
        lease.receive_page(&entries, false, 100).unwrap();
    }
    lease.receive_page(&[entry(1024, 90)], true, 100).unwrap();
    drop(owner);
    for remaining in [513, 1] {
        assert!(cleanup_step(&p, 1).unwrap());
        assert_eq!(charges(&p), (1, remaining, 1024 + remaining * 192));
    }
    assert!(cleanup_step(&p, 1).unwrap());
    assert_eq!(charges(&p), (0, 0, 0));
    assert!(lease.origin(&entry(1, 90).id, 100).is_err());
}
#[test]
fn warm_open_rejects_missing_federation_table_or_index() {
    for mutation in [
        "DROP TABLE relay_federation_entries",
        "DROP INDEX relay_federation_sessions_owner",
        "DROP TABLE relay_federation_runtime",
        "DROP TABLE relay_federation_local_entries",
        "DROP TABLE relay_digest_preparation",
        "DROP TABLE relay_digest_demands",
        "DROP TABLE relay_federation_digests",
        "DROP INDEX retained_events_domain_commits",
    ] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let p = profile(&root, &identity);
        p.database()
            .transaction(|c| {
                c.execute_batch(mutation)?;
                Ok(())
            })
            .unwrap();
        drop(p);
        assert!(
            ProfileStorage::open_relay(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &identity.public(),
                &root.path().join("relay.log"),
                "pw",
                100
            )
            .is_err(),
            "{mutation}"
        );
    }
}
#[test]
fn local_inventory_snapshot_has_owned_encrypted_schema() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let exists=p.database().read(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='relay_federation_local_entries')",[],|r|r.get::<_,bool>(0))?)).unwrap();
    assert!(
        exists,
        "sent local digests need charged encrypted finite snapshot authority"
    );
}
#[test]
fn both_snapshot_sides_share_charges_and_one_cleanup_slice_budget() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Outbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    let remote = (0..300).map(|n| entry(n, 90)).collect::<Vec<_>>();
    let local = (300..600).map(|n| entry(n, 90)).collect::<Vec<_>>();
    lease.receive_page(&remote, true, 100).unwrap();
    p.database()
        .transaction(|c| lease.stage_local_page_on(c, &local, true, 100))
        .unwrap();
    assert_eq!(charges(&p), (1, 600, SESSION_BYTES + 600 * ENTRY_BYTES));
    assert!(
        lease.origin(&local[0].id, 100).unwrap().is_none(),
        "local selection is never remote origin authority"
    );
    owner.cancel().unwrap();
    assert!(
        p.database()
            .transaction(|c| lease.stage_local_page_on(c, &[], true, 100))
            .is_err()
    );
    assert!(cleanup_step(&p, 100).unwrap());
    assert_eq!(charges(&p), (1, 88, SESSION_BYTES + 88 * ENTRY_BYTES));
    let counts = p
        .database()
        .read(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM relay_federation_entries", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                c.query_row(
                    "SELECT count(*) FROM relay_federation_local_entries",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!(counts, (0, 88));
    assert!(cleanup_step(&p, 100).unwrap());
    assert_eq!(charges(&p), (0, 0, 0));
}
#[test]
fn local_snapshot_capacity_failure_reserves_before_insert_and_preserves_remote() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Outbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    lease.receive_page(&[entry(1, 90)], true, 100).unwrap();
    p.database().transaction(|c|{c.execute("UPDATE relay_federation_runtime SET staged_rows=?1,staged_bytes=?2 WHERE singleton=1",params![MAX_ROWS-1,SESSION_BYTES+(MAX_ROWS-1)*ENTRY_BYTES])?;Ok(())}).unwrap();
    let before = charges(&p);
    assert!(matches!(
        p.database().transaction(|c| lease.stage_local_page_on(
            c,
            &[entry(2, 90), entry(3, 90)],
            true,
            100
        )),
        Err(LogError::RelayCapacityUnavailable)
    ));
    assert_eq!(charges(&p), before);
    p.database().read(|c|{let row:(i64,i64,i64)=c.query_row("SELECT local_complete,local_pages,row_charge FROM relay_federation_sessions WHERE session_id=?1",[lease.session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;assert_eq!(row,(0,0,1));let count:i64=c.query_row("SELECT count(*) FROM relay_federation_local_entries",[],|r|r.get(0))?;assert_eq!(count,0);Ok(())}).unwrap();
    assert_eq!(lease.origin(&entry(1, 90).id, 100).unwrap(), Some(90));
}
#[test]
fn scalar_mismatch_rotates_wraps_once_and_empty_continuation_is_not_done() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Outbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    let remote = (0..40).map(|n| entry(n, 90)).collect::<Vec<_>>();
    lease.receive_page(&remote, true, 100).unwrap();
    assert!(
        p.database()
            .transaction(|c| lease.begin_mismatch_on(c, None))
            .is_err(),
        "both terminal sides are mandatory"
    );
    p.database()
        .transaction(|c| lease.stage_local_page_on(c, &[], true, 100))
        .unwrap();
    p.database()
        .transaction(|c| lease.begin_mismatch_on(c, Some(&remote[19].id)))
        .unwrap();
    let mut ids = Vec::new();
    let mut empty_continuation = false;
    let mut done = false;
    for _ in 0..100 {
        let page = p
            .database()
            .transaction(|c| lease.mismatch_step_on(c, 2, 16))
            .unwrap();
        assert!(page.examined <= 2 && page.ids.len() <= 16);
        if page.ids.is_empty() && !page.done {
            empty_continuation = true
        }
        ids.extend(page.ids);
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done && empty_continuation);
    let expected = (20..40)
        .chain(0..20)
        .map(|n| entry(n, 90).id)
        .collect::<Vec<_>>();
    assert_eq!(ids, expected);
}
#[test]
fn local_raw_selection_continuations_do_not_consume_wire_page_budget() {
    let root = tempfile::tempdir().unwrap();
    let p = profile(&root, &DeviceIdentity::generate());
    let owner = SessionOwner::begin(p.clone(), Direction::Outbound, 100, Instant::now()).unwrap();
    let lease = owner.lease();
    for _ in 0..300 {
        p.database()
            .transaction(|c| lease.stage_local_batch_on(c, &[], 100))
            .unwrap();
    }
    p.database()
        .transaction(|c| lease.stage_local_batch_on(c, &[entry(1, 90)], 100))
        .unwrap();
    assert!(
        p.database()
            .transaction(|c| lease.finish_local_frame_on(c, 0, true))
            .is_err(),
        "terminal cannot omit selected entries"
    );
    p.database()
        .transaction(|c| lease.finish_local_frame_on(c, 1, true))
        .unwrap();
    let pages = p
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT local_pages FROM relay_federation_sessions WHERE session_id=?1",
                [lease.session_id],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(pages, 1);
}
