//! Disposable encrypted query pages owned by the unpublished profile connection.
//! SQLite's temporary pager coalesces commits in its bounded cache and spills to
//! a DELETEONCLOSE file. No schema here may supply durable or permission authority.
use super::{Connection, LogError, invalid};

/// Only call during connection construction, before TEMP hints or other owners
/// are installed. Changing temp_store destroys TEMP objects, hence the guards.
/// Any failure closes the unpublished connection; no partial owner is returned.
pub(super) fn initialize(connection: &Connection) -> Result<(), LogError> {
    if !connection.is_autocommit() {
        return Err(invalid(
            "query scratch requires an idle unpublished connection",
        ));
    }
    let memory_only: bool = connection.query_row(
        "SELECT sqlite_compileoption_used('TEMP_STORE=3')",
        [],
        |row| row.get(0),
    )?;
    let attached: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='participant_work')",
        [],
        |row| row.get(0),
    )?;
    let temporary: i64 =
        connection.query_row("SELECT count(*) FROM temp.sqlite_master", [], |row| {
            row.get(0)
        })?;
    let storage: i64 = connection.query_row("PRAGMA temp_store", [], |row| row.get(0))?;
    if memory_only || attached || temporary != 0 || storage != 2 {
        return Err(invalid(
            "query scratch requires file-capable pristine TEMP storage",
        ));
    }
    // The pager chooses FILE versus MEMORY when opened. Restoring temp_store
    // closes only SQLite's TEMP slot (index1), not this attached slot (index2).
    // Omitting KEY inherits main's already-derived SQLCipher key and salt:
    // https://www.zetetic.net/sqlcipher/sqlcipher-api/#attach
    connection.execute_batch(
        "PRAGMA temp_store=FILE;
         ATTACH DATABASE '' AS participant_work;
         PRAGMA temp_store=MEMORY;
         PRAGMA participant_work.cache_size=-8192;
         PRAGMA participant_work.cache_spill=ON;
         PRAGMA participant_work.journal_mode=MEMORY;
         PRAGMA participant_work.synchronous=OFF;
         PRAGMA participant_work.mmap_size=0;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::EncryptedDatabase;
    use rusqlite::{OptionalExtension, ffi};
    use std::io::Write;

    fn database(directory: &tempfile::TempDir) -> EncryptedDatabase {
        EncryptedDatabase::open(&directory.path().join("profile.db"), "pw", b"scratch-owner")
            .unwrap()
    }
    fn writes(connection: &Connection, reset: bool) -> i32 {
        let (mut current, mut high) = (0, 0);
        // SAFETY: the borrowed live connection is exclusively held by the test;
        // both output pointers refer to initialized stack integers.
        assert_eq!(
            unsafe {
                ffi::sqlite3_db_status(
                    connection.handle(),
                    ffi::SQLITE_DBSTATUS_CACHE_WRITE,
                    &mut current,
                    &mut high,
                    i32::from(reset),
                )
            },
            ffi::SQLITE_OK
        );
        current
    }
    fn budgets(connection: &Connection) {
        for (pragma, expected) in [
            ("temp_store", 2),
            ("main.synchronous", 2),
            ("participant_work.cache_size", -8192),
            ("participant_work.synchronous", 0),
        ] {
            let actual: i64 = connection
                .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
                .unwrap();
            assert_eq!(actual, expected);
        }
        // A not-yet-spilled temp pager has no mmap-capable OS file and SQLite
        // returns no row. Once open it must still have zero mapped bytes.
        let mmap: Option<i64> = connection
            .query_row("PRAGMA participant_work.mmap_size", [], |row| row.get(0))
            .optional()
            .unwrap();
        assert!(mmap.is_none_or(|size| size == 0));
        let journal: String = connection
            .query_row("PRAGMA participant_work.journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "memory");
    }

    #[test]
    fn scratch_small_commits_coalesce_with_rollback_and_final_temp_memory() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(&directory);
        db.read(|connection| {
            connection.execute_batch("CREATE TABLE participant_work.probe(body BLOB)")?;
            writes(connection, true);
            for _ in 0..100 {
                connection.execute_batch(
                    "BEGIN; INSERT INTO participant_work.probe VALUES (zeroblob(4096)); COMMIT;
                    BEGIN; DELETE FROM participant_work.probe; COMMIT;",
                )?;
            }
            assert_eq!(
                writes(connection, false),
                0,
                "200 small scratch commits must not force pager writes"
            );
            connection.execute_batch(
                "BEGIN; INSERT INTO participant_work.probe VALUES ('rollback'); ROLLBACK;",
            )?;
            let rows: i64 =
                connection.query_row("SELECT count(*) FROM participant_work.probe", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(rows, 0);
            budgets(connection);
            Ok(())
        })
        .unwrap();
        db.checkpoint_and_close().unwrap();
    }

    #[test]
    fn scratch_forced_spill_is_encrypted_and_readable_only_with_main_key() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(&directory);
        db.read(|connection| {
            connection.execute_batch("CREATE TABLE participant_work.probe(body BLOB); BEGIN")?;
            let marker = b"synthetic-encrypted-spill-canary";
            let mut payload = [b'Q'; 4096];
            payload[..marker.len()].copy_from_slice(marker);
            {
                let mut insert =
                    connection.prepare("INSERT INTO participant_work.probe VALUES (?1)")?;
                for _ in 0..4096 {
                    insert.execute([payload.as_slice()])?;
                }
            }
            connection.execute_batch("COMMIT")?;
            let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
            // SAFETY: the schema is attached to this exclusively borrowed live
            // connection. SQLite returns its own file pointer; never close it.
            assert_eq!(
                unsafe {
                    ffi::sqlite3_file_control(
                        connection.handle(),
                        c"participant_work".as_ptr(),
                        ffi::SQLITE_FCNTL_FILE_POINTER,
                        (&mut file as *mut *mut ffi::sqlite3_file).cast(),
                    )
                },
                ffi::SQLITE_OK
            );
            let file =
                std::ptr::NonNull::new(file).expect("SQLite must return its live spill file");
            // SAFETY: the pager/file remain live until the connection closure
            // returns. Check both FFI output pointers before borrowing the
            // methods table; neither pointer is owned or closed by this test.
            let methods = unsafe { file.as_ref().pMethods.as_ref() }
                .expect("a live SQLite spill file must have a methods table");
            let mut length = 0;
            // SAFETY: output points to a valid integer and file is still live.
            assert_eq!(
                unsafe { methods.xFileSize.unwrap()(file.as_ptr(), &mut length) },
                ffi::SQLITE_OK
            );
            assert!(
                length > 8 * 1024 * 1024,
                "data larger than cache must really spill to an OS file"
            );
            // Flush only this synthetic test so a raw copy has every dirty page.
            // Production relies on bounded spill rather than per-commit flush.
            // SAFETY: no statement/transaction is active; the borrowed connection
            // stays live and exclusively owned throughout this synchronous flush.
            assert_eq!(
                unsafe { ffi::sqlite3_db_cacheflush(connection.handle()) },
                ffi::SQLITE_OK
            );
            // SAFETY: same live file and initialized size output as above.
            assert_eq!(
                unsafe { methods.xFileSize.unwrap()(file.as_ptr(), &mut length) },
                ffi::SQLITE_OK
            );
            assert!(length > 8 * 1024 * 1024 && length <= 32 * 1024 * 1024);
            let mut copy = tempfile::NamedTempFile::new_in(directory.path())?;
            let mut bytes = [0u8; 4096];
            for offset in (0..length).step_by(bytes.len()) {
                let size = usize::try_from((length - offset).min(bytes.len() as i64)).unwrap();
                // SAFETY: xRead writes at most size <=4096 into bytes; offsets
                // are bounded by xFileSize, while the owned file remains live.
                assert_eq!(
                    unsafe {
                        methods.xRead.unwrap()(
                            file.as_ptr(),
                            bytes.as_mut_ptr().cast(),
                            size as i32,
                            offset,
                        )
                    },
                    ffi::SQLITE_OK
                );
                if offset == 0 {
                    assert!(!bytes.starts_with(b"SQLite format 3"));
                }
                assert!(
                    !bytes[..size]
                        .windows(marker.len())
                        .any(|window| window == marker)
                );
                copy.write_all(&bytes[..size])?;
            }
            copy.flush()?;
            let raw = Connection::open_with_flags(
                copy.path(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            assert!(
                raw.query_row("SELECT count(*) FROM sqlite_master", [], |row| row
                    .get::<_, i64>(0))
                    .is_err()
            );
            drop(raw);
            // MAIN's NOFOLLOW policy also applies to attached paths. Resolve
            // macOS's /var alias for this test-owned ciphertext copy.
            let copy_path = copy.path().canonicalize()?;
            connection.execute(
                "ATTACH DATABASE ?1 AS verify_spill",
                [copy_path.to_str().unwrap()],
            )?;
            let rows: i64 =
                connection.query_row("SELECT count(*) FROM verify_spill.probe", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(rows, 4096);
            connection.execute_batch("DETACH DATABASE verify_spill")?;
            budgets(connection);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn scratch_initialization_refuses_live_temp_or_attached_state_without_erasing_it() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA temp_store=MEMORY; CREATE TEMP TABLE retained(value); INSERT INTO retained VALUES(7)").unwrap();
        assert!(initialize(&connection).is_err());
        assert_eq!(
            connection
                .query_row("SELECT value FROM temp.retained", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            7
        );
        connection.execute_batch("DROP TABLE temp.retained; ATTACH ':memory:' AS participant_work; CREATE TABLE participant_work.retained(value); INSERT INTO participant_work.retained VALUES(9)").unwrap();
        assert!(initialize(&connection).is_err());
        assert_eq!(
            connection
                .query_row("SELECT value FROM participant_work.retained", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            9
        );
        connection
            .execute_batch("DETACH participant_work; BEGIN")
            .unwrap();
        assert!(initialize(&connection).is_err());
        assert!(!connection.is_autocommit());
        connection.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA temp_store", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
