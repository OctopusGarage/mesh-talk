use super::*;
use crate::{
    eventlog::{ConversationId, Event, EventKind, LogError},
    identity::device::DeviceIdentity,
    storage::record_log::EncryptedRecordLog,
};

#[test]
fn wrong_legacy_password_preserves_source_and_allows_correct_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("arbitrary.events");
    let device = DeviceIdentity::generate();
    let event = Event::new(
        &device,
        ConversationId::new([3; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![7; 16],
    );
    let (mut legacy, _) = EncryptedRecordLog::<Event>::open(&path, "correct", b"MTLOG1").unwrap();
    legacy.append_durable(&event).unwrap();
    drop(legacy);
    let before = std::fs::read(&path).unwrap();
    let lease = ProfileStorageLease::acquire(dir.path()).unwrap();
    assert!(
        super::context::ProfileStorage::open_relay(lease, &device.public(), &path, "wrong", 12)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("profile.db").exists());
    let lease = ProfileStorageLease::acquire(dir.path()).unwrap();
    let storage =
        super::context::ProfileStorage::open_relay(lease, &device.public(), &path, "correct", 12)
            .unwrap();
    assert_eq!(
        storage.events().unwrap().get(&event.id).unwrap(),
        Some(event)
    );
}

#[test]
fn repository_retains_profile_lease_after_outer_context_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let device = DeviceIdentity::generate();
    let lease = ProfileStorageLease::acquire(dir.path()).unwrap();
    let storage = super::context::ProfileStorage::open_relay(
        lease,
        &device.public(),
        &dir.path().join("relay.events"),
        "pw",
        12,
    )
    .unwrap();
    let repository = storage.events().unwrap();
    drop(storage);
    assert!(ProfileStorageLease::acquire(dir.path()).is_err());
    drop(repository);
    assert!(ProfileStorageLease::acquire(dir.path()).is_ok());
}

#[test]
fn reordered_duplicate_full_u64_history_matches_real_legacy_reader() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom.events");
    let device = DeviceIdentity::generate();
    let parent = Event::new(
        &device,
        ConversationId::new([3; 32]),
        u64::MAX - 1,
        vec![],
        u64::MAX - 1,
        1,
        EventKind::Message,
        vec![],
    );
    let child = Event::new(
        &device,
        parent.conversation_id,
        u64::MAX,
        vec![parent.id],
        u64::MAX,
        2,
        EventKind::Message,
        vec![],
    );
    let (mut legacy, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
    for e in [&child, &parent, &child] {
        legacy.append_durable(e).unwrap();
    }
    drop(legacy);
    let old = crate::eventlog::PersistentEventLog::open(&path, "pw").unwrap();
    drop(old);
    let storage = super::context::ProfileStorage::open_relay(
        ProfileStorageLease::acquire(dir.path()).unwrap(),
        &device.public(),
        &path,
        "pw",
        12,
    )
    .unwrap();
    let events = storage.events().unwrap();
    assert_eq!(events.get(&parent.id).unwrap(), Some(parent));
    assert_eq!(events.get(&child.id).unwrap(), Some(child));
    drop(events);
    drop(storage);
    assert!(crate::eventlog::PersistentEventLog::open(&path, "pw").is_err());
}

#[test]
fn header_only_and_torn_first_record_preserve_legacy_empty_semantics() {
    use std::io::Write;
    for torn in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.events");
        let device = DeviceIdentity::generate();
        drop(EncryptedRecordLog::<Event>::open(&path, "prior password", b"MTLOG1").unwrap());
        if torn {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(&[0, 0, 1, 0, 4, 5])
                .unwrap();
        }
        let storage = super::context::ProfileStorage::open_relay(
            ProfileStorageLease::acquire(dir.path()).unwrap(),
            &device.public(),
            &path,
            "supplied",
            12,
        )
        .unwrap();
        assert!(!std::fs::read(&path).unwrap().starts_with(b"MTLOG1"));
        drop(storage);
    }
}

#[test]
fn invalid_sources_never_initialize_a_final_database() {
    for invalid in [
        vec![],
        b"MTLOG1".to_vec(),
        b"unknown source and salt".to_vec(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        std::fs::write(&path, &invalid).unwrap();
        let device = DeviceIdentity::generate();
        assert!(
            super::context::ProfileStorage::open_relay(
                ProfileStorageLease::acquire(dir.path()).unwrap(),
                &device.public(),
                &path,
                "pw",
                12
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
        assert!(!dir.path().join("profile.db").exists());
    }
}

fn fixture(root: &std::path::Path) -> (DeviceIdentity, std::path::PathBuf, Event) {
    let device = DeviceIdentity::generate();
    let path = root.join("custom.events");
    let event = Event::new(
        &device,
        ConversationId::new([3; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![7; 16],
    );
    let (mut writer, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
    writer.append_durable(&event).unwrap();
    drop(writer);
    (device, path, event)
}
fn open(
    root: &std::path::Path,
    device: &DeviceIdentity,
    path: &std::path::Path,
) -> Result<std::sync::Arc<super::context::ProfileStorage>, crate::eventlog::LogError> {
    super::context::ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root).unwrap(),
        &device.public(),
        path,
        "pw",
        12,
    )
}
#[test]
fn published_cleanup_reclaims_duplicate_staging_and_normal_reopen_does_no_legacy_io() {
    use super::publication::tests::{DECODES, HASHES};
    let dir = tempfile::tempdir().unwrap();
    let (device, path, event) = fixture(dir.path());
    let profile = open(dir.path(), &device, &path).unwrap();
    assert!(!profile.cleanup_pending().unwrap());
    assert_eq!(
        profile
            .database()
            .read(|c| Ok(
                c.query_row("SELECT count(*) FROM relay_import_events", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .unwrap(),
        0
    );
    drop(profile);
    HASHES.with(|v| v.set(0));
    DECODES.with(|v| v.set(0));
    let profile = open(dir.path(), &device, &path).unwrap();
    assert_eq!(HASHES.with(|v| v.get()), 0);
    assert_eq!(DECODES.with(|v| v.get()), 0);
    assert_eq!(
        profile.events().unwrap().get(&event.id).unwrap(),
        Some(event)
    );
}
#[test]
fn published_reopen_rejects_missing_arrival_schema_without_reinitializing() {
    let dir = tempfile::tempdir().unwrap();
    let (device, path, _) = fixture(dir.path());
    let profile = open(dir.path(), &device, &path).unwrap();
    profile
        .database()
        .transaction(|tx| {
            tx.execute("DROP TABLE relay_mailboxes", [])?;
            Ok(())
        })
        .unwrap();
    drop(profile);
    assert!(open(dir.path(), &device, &path).is_err());
}
#[test]
fn every_publication_cut_reopens_from_authenticated_state() {
    use super::publication::tests::CUT;
    for point in [
        "archive",
        "closed-candidate",
        "prepared",
        "event-stub",
        "arrival-stub",
        "frozen",
        "event-batch",
        "events-staged",
        "events-ranked",
        "kahn-page",
        "kahn-batch",
        "verified",
        "admission-row",
        "publication-transaction",
        "published",
        "reclaimed",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (device, path, event) = fixture(dir.path());
        // Add a child to exercise persisted Kahn edge draining.
        let child = Event::new(
            &device,
            event.conversation_id,
            2,
            vec![event.id],
            2,
            2,
            EventKind::Message,
            vec![],
        );
        let (mut writer, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
        writer.append_durable(&child).unwrap();
        drop(writer);
        CUT.with(|v| v.replace(Some(point)));
        assert!(
            open(dir.path(), &device, &path).is_err(),
            "cut {point} did not interrupt"
        );
        CUT.with(|v| v.replace(None));
        let profile = open(dir.path(), &device, &path)
            .unwrap_or_else(|e| panic!("recovery after {point}: {e}"));
        let repository = profile.events().unwrap();
        assert_eq!(repository.get(&event.id).unwrap(), Some(event));
        assert_eq!(repository.get(&child.id).unwrap(), Some(child));
        assert_eq!(
            profile
                .database()
                .read(|c| Ok(c.query_row(
                    "SELECT count(*) FROM relay_sources WHERE phase=4",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            2
        );
    }
}
#[test]
fn cleanup_intent_retries_after_checked_checkpoint_or_unlink_interruption() {
    use super::publication::tests::CUT;
    for point in ["checkpoint", "cleanup"] {
        let dir = tempfile::tempdir().unwrap();
        let (device, path, event) = fixture(dir.path());
        CUT.with(|v| v.replace(Some(point)));
        let profile = open(dir.path(), &device, &path).unwrap();
        assert!(profile.cleanup_pending().unwrap());
        drop(profile);
        let profile = open(dir.path(), &device, &path).unwrap();
        assert!(!profile.cleanup_pending().unwrap());
        assert_eq!(
            profile.events().unwrap().get(&event.id).unwrap(),
            Some(event)
        );
    }
}
#[test]
fn restored_legacy_source_missing_or_altered_barrier_fails_closed() {
    for change in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let (device, path, _) = fixture(dir.path());
        let original = std::fs::read(&path).unwrap();
        drop(open(dir.path(), &device, &path).unwrap());
        match change {
            0 => std::fs::write(&path, &original).unwrap(),
            1 => std::fs::remove_file(&path).unwrap(),
            _ => {
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[10] ^= 1;
                std::fs::write(&path, bytes).unwrap();
            }
        }
        assert!(open(dir.path(), &device, &path).is_err());
        if change == 0 {
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }
}
#[test]
fn closed_profile_move_preserves_device_binding_and_relative_layout() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("before");
    std::fs::create_dir(&first).unwrap();
    let (device, path, event) = fixture(&first);
    drop(open(&first, &device, &path).unwrap());
    let moved = root.path().join("after");
    std::fs::rename(&first, &moved).unwrap();
    let profile = open(&moved, &device, &moved.join("custom.events")).unwrap();
    assert_eq!(
        profile.events().unwrap().get(&event.id).unwrap(),
        Some(event)
    );
}
#[test]
fn owner_role_and_layout_changes_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (device, path, _) = fixture(dir.path());
    drop(open(dir.path(), &device, &path).unwrap());
    assert!(open(dir.path(), &DeviceIdentity::generate(), &path).is_err());
    assert!(open(dir.path(), &device, &dir.path().join("another.events")).is_err());
    let profile = open(dir.path(), &device, &path).unwrap();
    profile.database().transaction(|tx|{tx.execute("DROP TABLE relay_publication",[])?;tx.execute_batch("CREATE TABLE relay_publication(singleton INTEGER PRIMARY KEY,version INTEGER,role TEXT);INSERT INTO relay_publication VALUES(1,1,'participant')")?;Ok(())}).unwrap();
    drop(profile);
    assert!(open(dir.path(), &device, &path).is_err());
}
#[test]
fn missing_parent_equivocation_and_bad_signature_never_publish() {
    for case in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let (device, path, event) = fixture(dir.path());
        let mut invalid = Event::new(
            &device,
            event.conversation_id,
            if case == 1 { 1 } else { 2 },
            if case == 0 {
                vec![crate::eventlog::EventId::new([9; 32])]
            } else {
                vec![]
            },
            2,
            2,
            EventKind::Message,
            vec![9],
        );
        if case == 2 {
            invalid.sig[0] ^= 1;
        }
        let (mut writer, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
        writer.append_durable(&invalid).unwrap();
        drop(writer);
        assert!(open(dir.path(), &device, &path).is_err());
        assert!(open(dir.path(), &device, &path).is_err());
    }
}
#[cfg(unix)]
#[test]
fn aliases_non_utf8_paths_and_unsafe_leaves_follow_profile_ownership() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("actual");
    std::fs::create_dir(&dir).unwrap();
    let alias = root.path().join("alias");
    symlink(&dir, &alias).unwrap();
    let device = DeviceIdentity::generate(); // APFS rejects non-UTF8 names at the filesystem boundary. Exercise raw
    // byte locators on Unix filesystems that can actually create such entries.
    let probe = dir.join(std::ffi::OsString::from_vec(b"probe-\xff".to_vec()));
    let filename = if std::fs::write(&probe, []).is_ok() {
        std::fs::remove_file(probe).unwrap();
        std::ffi::OsString::from_vec(b"relay-\xff.events".to_vec())
    } else {
        std::ffi::OsString::from("relay-unicode-例.events")
    };
    let profile = open(&alias, &device, &alias.join(&filename)).unwrap();
    drop(profile);
    drop(open(&dir, &device, &dir.join(&filename)).unwrap());
    let source = root.path().join("source");
    std::fs::write(&source, b"unchanged").unwrap();
    let unsafe_leaf = root.path().join("unsafe.events");
    symlink(&source, &unsafe_leaf).unwrap();
    assert!(open(root.path(), &device, &unsafe_leaf).is_err());
    assert_eq!(std::fs::read(source).unwrap(), b"unchanged");
    assert!(open(&dir, &device, &root.path().join("external.events")).is_err());
}

#[test]
fn interrupted_import_restarts_with_bounded_live_records_and_batches() {
    use super::publication::tests::{CUT, DECODES, MAX_BATCH, MAX_EDGES, MAX_LIVE};
    let dir = tempfile::tempdir().unwrap();
    let (device, path, event) = fixture(dir.path());
    let (mut writer, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
    let mut parent = event.id;
    for seq in 2..=20 {
        let e = Event::new(
            &device,
            event.conversation_id,
            seq,
            vec![parent],
            seq,
            seq,
            EventKind::Message,
            vec![],
        );
        parent = e.id;
        writer.append(&e).unwrap();
    }
    drop(writer);
    CUT.with(|v| v.replace(Some("event-batch")));
    assert!(open(dir.path(), &device, &path).is_err());
    DECODES.with(|v| v.set(0));
    MAX_LIVE.with(|v| v.set(0));
    MAX_BATCH.with(|v| v.set(0));
    MAX_EDGES.with(|v| v.set(0));
    let profile = open(dir.path(), &device, &path).unwrap();
    assert!(profile.events().unwrap().has(&parent).unwrap());
    assert_eq!(
        DECODES.with(|v| v.get()),
        20,
        "restart must visit the complete archived source"
    );
    assert_eq!(MAX_LIVE.with(|v| v.get()), 1);
    assert_eq!(MAX_BATCH.with(|v| v.get()), 20);
    assert!(MAX_EDGES.with(|v| v.get()) <= 128);
}

#[cfg(unix)]
#[test]
fn artifact_directory_redirection_is_rejected_before_work_or_cleanup() {
    use super::publication::tests::CUT;
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let (device, path, _) = fixture(dir.path());
    CUT.with(|v| v.replace(Some("prepared")));
    assert!(open(dir.path(), &device, &path).is_err());
    let artifact = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".relay-publication-")
        })
        .unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(&artifact).unwrap() {
        let entry = entry.unwrap();
        std::fs::rename(entry.path(), elsewhere.path().join(entry.file_name())).unwrap();
    }
    std::fs::remove_dir(&artifact).unwrap();
    symlink(elsewhere.path(), &artifact).unwrap();
    assert!(open(dir.path(), &device, &path).is_err());
    assert!(std::fs::read(&path).unwrap().starts_with(b"MTLOG1"));
    assert!(elsewhere.path().join("source-0.raw").exists());
}

#[test]
fn authenticated_decode_or_aead_failure_preserves_originals_before_freeze() {
    for malformed_encoding in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        let device = DeviceIdentity::generate();
        if malformed_encoding {
            let (mut writer, _) = EncryptedRecordLog::<u64>::open(&path, "pw", b"MTLOG1").unwrap();
            writer.append_durable(&5).unwrap();
        } else {
            let (_, _, _) = fixture(dir.path());
            let source = dir.path().join("custom.events");
            std::fs::rename(source, &path).unwrap();
            let mut bytes = std::fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            std::fs::write(&path, bytes).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        assert!(open(dir.path(), &device, &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("profile.db").exists());
    }
}
#[test]
fn prepared_source_or_archive_changes_are_not_adopted() {
    use super::publication::tests::CUT;
    for mutate_archive in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (device, path, _) = fixture(dir.path());
        CUT.with(|v| v.replace(Some("prepared")));
        assert!(open(dir.path(), &device, &path).is_err());
        let target = if mutate_archive {
            std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    p.is_dir()
                        && p.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(".relay-publication-")
                })
                .unwrap()
                .join("source-0.raw")
        } else {
            path.clone()
        };
        let mut bytes = std::fs::read(&target).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&target, &bytes).unwrap();
        assert!(open(dir.path(), &device, &path).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert!(std::fs::read(&path).unwrap().starts_with(b"MTLOG1"));
    }
}
#[test]
fn zero_foreign_and_future_final_databases_are_preserved() {
    for kind in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let (device, path, _) = fixture(dir.path());
        let database = dir.path().join("profile.db");
        if kind == 0 {
            std::fs::write(&database, []).unwrap();
        } else if kind == 1 {
            std::fs::write(&database, b"foreign database bytes").unwrap();
        } else {
            let profile = open(dir.path(), &device, &path).unwrap();
            profile
                .database()
                .read(|c| {
                    c.pragma_update(None, "user_version", 2)?;
                    Ok(())
                })
                .unwrap();
            drop(profile);
        }
        let before = std::fs::read(&database).unwrap();
        assert!(open(dir.path(), &device, &path).is_err());
        assert_eq!(std::fs::read(database).unwrap(), before);
    }
}

#[test]
fn published_open_requires_existing_event_authority_without_reinitialization() {
    for damaged in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let (device, path, _) = fixture(dir.path());
        let profile = open(dir.path(), &device, &path).unwrap();
        profile.database().read(|c| {
            c.execute_batch("PRAGMA foreign_keys=OFF")?;
            match damaged {
                0=> {let names={let mut stmt=c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE 'event_%' OR name IN ('retained_events','retained_event_parents'))")?;let rows=stmt.query_map([],|r|r.get::<_,String>(0))?;rows.collect::<Result<Vec<_>,_>>()?};for name in names {c.execute_batch(&format!("DROP TABLE {name}"))?;}},
                1=>{c.execute("DROP TABLE event_repository_schema",[])?;},
                2=>{c.execute("DELETE FROM event_domains",[])?;},
                _=>{c.execute("DROP TABLE retained_events",[])?;}
            }
            c.execute_batch("PRAGMA foreign_keys=ON")?;Ok(())
        }).unwrap();
        drop(profile);
        let before = std::fs::read(dir.path().join("profile.db")).unwrap();
        let barrier = std::fs::read(&path).unwrap();
        let result = open(dir.path(), &device, &path);
        let rejected = result.is_err();
        drop(result);
        assert!(
            rejected,
            "published event authority damage {damaged} was accepted"
        );
        assert_eq!(
            std::fs::read(dir.path().join("profile.db")).unwrap(),
            before
        );
        assert_eq!(std::fs::read(&path).unwrap(), barrier);
    }
}

#[test]
fn prepared_raw_archive_is_independent_and_preserves_original_torn_bytes() {
    use super::publication::tests::CUT;
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let (device, path, event) = fixture(dir.path());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&[0, 0, 1, 0, 7, 8])
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    CUT.with(|v| v.replace(Some("prepared")));
    assert!(open(dir.path(), &device, &path).is_err());
    let archive = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.is_dir()
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".relay-publication-")
        })
        .unwrap()
        .join("source-0.raw");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::read(&archive).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let source = std::fs::metadata(&path).unwrap();
        let raw = std::fs::metadata(&archive).unwrap();
        assert_ne!(source.ino(), raw.ino());
        assert_eq!(source.nlink(), 1);
        assert_eq!(raw.nlink(), 1);
    }
    let profile = open(dir.path(), &device, &path).unwrap();
    assert_eq!(
        profile.events().unwrap().get(&event.id).unwrap(),
        Some(event)
    );
}

#[test]
fn runtime_cleanup_preserves_authoritative_commit_error_before_any_diagnostic_retry() {
    use super::publication::tests::CUT;
    let root = tempfile::tempdir().unwrap();
    let (device, path, _) = fixture(root.path());
    CUT.with(|v| v.replace(Some("checkpoint")));
    let profile = open(root.path(), &device, &path).unwrap();
    profile.database().read(|c|{c.execute_batch("CREATE TEMP TABLE cleanup_fault_parent(id INTEGER PRIMARY KEY);CREATE TEMP TABLE cleanup_fault_child(id INTEGER REFERENCES cleanup_fault_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER cleanup_commit_fault AFTER UPDATE OF cleanup_pending ON relay_sources WHEN NEW.cleanup_pending=0 BEGIN INSERT INTO cleanup_fault_child VALUES(1); END;")?;Ok(())}).unwrap();
    let error = profile.cleanup_step(true).unwrap_err();
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    let mut code = None;
    while let Some(error) = source {
        if let Some(driver) = error.downcast_ref::<rusqlite::Error>() {
            code = driver.sqlite_error_code();
            break;
        }
        source = error.source();
    }
    assert_eq!(
        code,
        Some(rusqlite::ErrorCode::ConstraintViolation),
        "original COMMIT failure must survive faulted-handle diagnostics: {error}"
    );
    assert!(profile.database().requires_reopen());
}
#[test]
fn runtime_cleanup_rejects_invalid_artifact_provenance_instead_of_deferring_it() {
    use super::publication::tests::CUT;
    let root = tempfile::tempdir().unwrap();
    let (device, path, _) = fixture(root.path());
    CUT.with(|v| v.replace(Some("checkpoint")));
    let profile = open(root.path(), &device, &path).unwrap();
    let directory = std::fs::read_dir(root.path())
        .unwrap()
        .map(|v| v.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".relay-publication-")
        })
        .unwrap();
    let saved = root.path().join("saved-owned-artifacts");
    std::fs::rename(&directory, &saved).unwrap();
    std::fs::write(&directory, b"invalid parent identity").unwrap();
    assert!(
        matches!(profile.cleanup_step(true), Err(LogError::CorruptFile(_))),
        "unknown artifact ownership must not be downgraded to successful cleanup"
    );
    assert_eq!(
        std::fs::read(&directory).unwrap(),
        b"invalid parent identity"
    );
    assert!(saved.is_dir());
}
