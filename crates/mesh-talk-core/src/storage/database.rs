//! SQLCipher connection owner for profile-local indexed stores.
//! Domain tables and bounded query rules belong to their individual stores.
use crate::eventlog::LogError;
use rusqlite::{Connection, OpenFlags, Transaction};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use zeroize::Zeroizing;

// Part of the on-disk key format; prevents SQLCipher raw-key syntax bypass.
const PASSPHRASE_DOMAIN: &str = "mesh-talk:profile-indexed-database:v1:";
const APPLICATION_ID: i64 = 0x4d544442;
const SCHEMA_VERSION: i64 = 1;
// This is a per-connection page-cache budget, not a bound on process memory.
const PAGE_CACHE_KIB: i64 = 4096;
// Newly enabled VM reuse has a separate fixed bound from the page cache.
const PREPARED_STATEMENTS: usize = 16;
const WAL_CHECKPOINT_PAGES: i64 = 256;
const JOURNAL_BYTES: i64 = 4 * 1024 * 1024;
#[cfg(any(test, feature = "fast-test-kdf"))]
const KDF_ITERATIONS: i64 = 1;
#[cfg(not(any(test, feature = "fast-test-kdf")))]
const KDF_ITERATIONS: i64 = 600_000;

#[derive(Debug)]
struct DatabaseStateError(&'static str);
impl std::fmt::Display for DatabaseStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for DatabaseStateError {}
fn invalid(message: &'static str) -> LogError {
    LogError::Database(Box::new(DatabaseStateError(message)))
}

#[derive(Debug)]
struct DatabaseContext {
    operation: &'static str,
    source: LogError,
}
impl std::fmt::Display for DatabaseContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.operation, self.source)
    }
}
impl std::error::Error for DatabaseContext {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
fn context(operation: &'static str, source: rusqlite::Error) -> LogError {
    LogError::Database(Box::new(DatabaseContext {
        operation,
        source: source.into(),
    }))
}

/// Both causes are retained when transaction recovery itself fails. The
/// enclosing Database error and owner's fault flag remain authoritative even
/// when the original operation returned an ordinary admission outcome.
#[derive(Debug)]
struct RollbackFailure {
    operation: LogError,
    rollback: LogError,
}
impl std::fmt::Display for RollbackFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "database rollback failed after {}: {}",
            self.operation, self.rollback
        )
    }
}
impl std::error::Error for RollbackFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.operation)
    }
}

/// Freelist-page work cap, not a CPU, IO, lock-time or filesystem-byte cap.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PhysicalReclaimBudget(u32);
impl PhysicalReclaimBudget {
    pub(crate) fn new(pages: u32) -> Result<Self, LogError> {
        if !(1..=64).contains(&pages) {
            return Err(invalid("physical reclamation budget must be 1..=64 pages"));
        }
        Ok(Self(pages))
    }
}
impl Default for PhysicalReclaimBudget {
    fn default() -> Self {
        Self::new(64).expect("fixed positive physical reclamation budget")
    }
}

/// Scalar SQL page observations. Neither the logical domain charge nor actual
/// main/WAL filesystem usage: WAL readers may delay main-file truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhysicalStats {
    pub page_size: u64,
    pub page_count: u64,
    pub freelist_count: u64,
}
impl PhysicalStats {
    fn read_on(c: &Connection) -> Result<Self, LogError> {
        require_incremental_vacuum(c)?;
        let scalar = |name| -> Result<u64, LogError> {
            let value: i64 = c.pragma_query_value(None, name, |r| r.get(0))?;
            u64::try_from(value).map_err(|_| invalid("negative physical page observation"))
        };
        // SQLCipher's page_size handler returns cipher_page_size as TEXT,
        // unlike SQLite's integer page_count/freelist_count pragmas.
        let page_size: String = c.pragma_query_value(None, "page_size", |r| r.get(0))?;
        let stats = Self {
            page_size: page_size
                .parse::<u64>()
                .map_err(|_| invalid("invalid physical page size"))?,
            page_count: scalar("page_count")?,
            freelist_count: scalar("freelist_count")?,
        };
        if stats.page_size == 0 || stats.freelist_count > stats.page_count {
            return Err(invalid("invalid physical page observations"));
        }
        stats
            .page_size
            .checked_mul(stats.page_count)
            .ok_or_else(|| invalid("physical page estimate overflow"))?;
        Ok(stats)
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct PhysicalReclaimProgress {
    pub before: PhysicalStats,
    pub after: PhysicalStats,
    /// Successful engine iterations consumed through SQLITE_DONE. Returned only
    /// after the owning transaction commits; late errors cannot claim progress.
    pub yielded_steps: u32,
}
fn require_incremental_vacuum(c: &Connection) -> Result<(), LogError> {
    let mode: i64 = c.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?;
    if mode != 2 {
        return Err(invalid(
            "encrypted database requires incremental auto_vacuum mode (2)",
        ));
    }
    Ok(())
}

/// PASSIVE may transfer a historical WAL backlog: this has no page-work cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassiveCheckpointProgress {
    NoWal,
    Frames { log: u64, checkpointed: u64 },
}
impl PassiveCheckpointProgress {
    pub(crate) fn complete(self) -> bool {
        match self {
            Self::NoWal => true,
            Self::Frames { log, checkpointed } => log == checkpointed,
        }
    }
}

pub(crate) struct EncryptedDatabase {
    connection: Mutex<Connection>,
    path: PathBuf,
    faulted: AtomicBool,
}
impl EncryptedDatabase {
    /// An ambiguous commit or fatal driver failure requires a fresh owner.
    pub(crate) fn requires_reopen(&self) -> bool {
        self.faulted.load(Ordering::Acquire) || self.connection.is_poisoned()
    }
    pub(crate) fn open(path: &Path, password: &str, binding: &[u8]) -> Result<Self, LogError> {
        if password.is_empty() {
            return Err(invalid("database password must not be empty"));
        }
        if binding.is_empty() {
            return Err(invalid("database owner binding must not be empty"));
        }
        // SQLite NOFOLLOW also rejects symlinked ancestors (e.g. macOS /var).
        // Resolve the parent only; never resolve or follow the database leaf.
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let leaf = path
            .file_name()
            .ok_or_else(|| invalid("database path must name a regular file"))?;
        let path = std::fs::canonicalize(parent)?.join(leaf);
        let created = prepare_private_regular_file(&path)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(&path, flags)
            .map_err(|e| context("open encrypted connection", e))?;
        #[cfg(test)]
        observations::CONNECTION_OPENS.with(|count| count.set(count.get() + 1));
        let passphrase = Zeroizing::new(format!("{PASSPHRASE_DOMAIN}{password}"));
        assign_key(&mut connection, passphrase.as_bytes())?;
        drop(passphrase);
        // Do not touch the schema before configuring the key and its KDF.
        let cipher: String = connection
            .query_row("PRAGMA cipher_version", [], |r| r.get(0))
            .map_err(|e| context("verify cipher availability", e))?;
        if cipher.is_empty() {
            return Err(invalid("SQLCipher is unavailable"));
        }

        connection
            .execute_batch("PRAGMA cipher_compatibility = 4")
            .map_err(|e| context("configure cipher compatibility", e))?;
        connection
            .execute_batch(&format!("PRAGMA kdf_iter = {KDF_ITERATIONS}"))
            .map_err(|e| context("configure key derivation", e))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let foreign_keys: i64 =
            connection.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
        if foreign_keys != 1 {
            return Err(invalid(
                "encrypted database requires foreign key enforcement",
            ));
        }
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "temp_store", "MEMORY")?;
        connection.pragma_update(None, "cache_size", -PAGE_CACHE_KIB)?;
        connection.set_prepared_statement_cache_capacity(PREPARED_STATEMENTS);
        connection.pragma_update(None, "mmap_size", 0)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        // Validate provenance before changing the journal mode of an existing DB.
        let tx = connection
            .transaction()
            .map_err(|e| context("begin database transaction", e))?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        // Never infer that an existing empty file is safe to initialize: it may
        // be truncated history or an interrupted initialization needing recovery.
        if created && version == 0 && application == 0 {
            let tables: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if tables != 0 {
                return Err(invalid("unrecognized database schema"));
            }
            // Pointer maps must exist before the first domain/metadata table.
            // Only an exclusively created file is eligible for initialization.
            tx.pragma_update(None, "auto_vacuum", 2)?;
            require_incremental_vacuum(&tx)?;
            tx.execute_batch(
                "CREATE TABLE mesh_database_metadata (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                owner_binding BLOB NOT NULL,
                store_uuid BLOB NOT NULL CHECK(length(store_uuid) = 16)
            )",
            )?;
            tx.execute(
                "INSERT INTO mesh_database_metadata VALUES (1, ?1, ?2)",
                rusqlite::params![binding, uuid::Uuid::new_v4().as_bytes().as_slice()],
            )?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version != SCHEMA_VERSION || application != APPLICATION_ID {
            return Err(invalid(
                "incompatible database application or schema version",
            ));
        }
        let (owner, store_uuid): (Vec<u8>, Vec<u8>) = tx.query_row(
            "SELECT owner_binding, store_uuid FROM mesh_database_metadata WHERE singleton = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if owner != binding || uuid::Uuid::from_slice(&store_uuid).is_err() {
            return Err(invalid(
                "database owner binding or store identity does not match",
            ));
        }
        // Authenticate provenance and owner first on warm open. Never repair a
        // known authority with NONE/FULL, VACUUM, or a replacement empty store.
        require_incremental_vacuum(&tx)?;
        tx.commit()?;
        let journal: String =
            connection.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        if !journal.eq_ignore_ascii_case("wal") {
            return Err(invalid("encrypted database requires WAL mode"));
        }
        connection.pragma_update(None, "wal_autocheckpoint", WAL_CHECKPOINT_PAGES)?;
        connection.pragma_update(None, "journal_size_limit", JOURNAL_BYTES)?;
        Ok(Self {
            connection: Mutex::new(connection),
            path,
            faulted: AtomicBool::new(false),
        })
    }
    pub(crate) fn physical_stats(&self) -> Result<PhysicalStats, LogError> {
        self.read(PhysicalStats::read_on)
    }

    /// One known-main-file stat on the maintenance owner's first inspection.
    /// This detects a WAL-committed truncation after restart without a history
    /// walk or unconditional checkpoint of ordinary pending appends.
    pub(crate) fn main_file_exceeds_page_estimate(&self) -> Result<bool, LogError> {
        self.read(|c| {
            let stats = PhysicalStats::read_on(c)?;
            let estimate = stats
                .page_size
                .checked_mul(stats.page_count)
                .ok_or_else(|| invalid("physical page estimate overflow"))?;
            Ok(std::fs::metadata(&self.path)?.len() > estimate)
        })
    }

    /// One background completion attempt; never waits for readers, never loops
    /// or truncates the WAL. Engine checkpoint work is not limited by the
    /// reclamation page budget. Partial progress remains an explicit outcome.
    pub(crate) fn passive_checkpoint(&self) -> Result<PassiveCheckpointProgress, LogError> {
        self.read(|c| {
            #[cfg(test)]
            observations::PASSIVE_ATTEMPTS
                .with(|v| v.borrow_mut().push(tokio::time::Instant::now()));
            let (busy, log, checkpointed): (i64, i64, i64) = c
                .query_row("PRAGMA main.wal_checkpoint(PASSIVE)", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .map_err(|e| context("passive physical checkpoint", e))?;
            if busy == 1 {
                return Err(context(
                    "passive physical checkpoint",
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        Some(format!(
                            "checkpoint busy={busy}, log={log}, checkpointed={checkpointed}"
                        )),
                    ),
                ));
            }
            if busy != 0 {
                return Err(invalid("invalid passive checkpoint busy observation"));
            }
            if log == -1 && checkpointed == -1 {
                return Ok(PassiveCheckpointProgress::NoWal);
            }
            let log = u64::try_from(log)
                .map_err(|_| invalid("negative passive checkpoint log observation"))?;
            let checkpointed = u64::try_from(checkpointed)
                .map_err(|_| invalid("negative passive checkpoint completion observation"))?;
            if checkpointed > log {
                return Err(invalid("invalid passive checkpoint frame observations"));
            }
            Ok(PassiveCheckpointProgress::Frames { log, checkpointed })
        })
    }

    pub(crate) fn reclaim_pages(
        &self,
        budget: PhysicalReclaimBudget,
    ) -> Result<PhysicalReclaimProgress, LogError> {
        let before = self.physical_stats()?;
        if before.freelist_count == 0 {
            return Ok(PhysicalReclaimProgress {
                before,
                after: before,
                yielded_steps: 0,
            });
        }
        #[cfg(test)]
        {
            observations::VACUUM_ATTEMPTS
                .with(|v| v.borrow_mut().push(tokio::time::Instant::now()));
            if observations::FORCE_BUSY.with(|v| v.get()) {
                return Err(context(
                    "incremental vacuum fixture contention",
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        None,
                    ),
                ));
            }
        }
        self.transaction(|tx| reclaim_pages_on(tx, budget))
    }

    /// Checked durability boundary for manifest-directed archive reclamation.
    pub(crate) fn checkpoint(&self) -> Result<(), LogError> {
        self.read(|c| {
            let (busy, log, checkpointed): (i64, i64, i64) =
                c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            if busy != 0 || log != 0 || checkpointed != 0 {
                return Err(context(
                    "checkpoint profile publication",
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        None,
                    ),
                ));
            }
            Ok(())
        })
    }
    /// Retire the connection before relocating its main file. A successful
    /// TRUNCATE checkpoint has no retained frames; neither busy nor incomplete
    /// results authorize publishing the main file without its WAL.
    pub(crate) fn checkpoint_and_close(self) -> Result<(), LogError> {
        if self.faulted.load(Ordering::Acquire) {
            return Err(invalid("database connection faulted; reopen required"));
        }
        let connection = self
            .connection
            .into_inner()
            .map_err(|_| invalid("database connection mutex poisoned"))?;
        let (busy, log, checkpointed): (i64, i64, i64) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|error| context("truncate database WAL", error))?;
        if busy != 0 {
            // SQLite reports checkpoint contention as a result row, not an
            // execution error. Preserve its typed busy meaning for callers.
            return Err(context(
                "truncate database WAL",
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some(format!(
                        "checkpoint busy={busy}, log={log}, checkpointed={checkpointed}"
                    )),
                ),
            ));
        }
        if log != 0 || checkpointed != 0 {
            return Err(invalid(
                "truncate database WAL returned incomplete frame counts",
            ));
        }
        connection
            .close()
            .map_err(|(_connection, error)| context("close encrypted connection", error))
    }
    fn checked_operation<T>(&self, result: Result<T, LogError>) -> Result<T, LogError> {
        if result.as_ref().err().is_some_and(fatal_database_error) {
            self.faulted.store(true, Ordering::Release);
        }
        result
    }
    pub(crate) fn read<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| invalid("database connection mutex poisoned"))?;
        if self.faulted.load(Ordering::Acquire) {
            return Err(invalid("database connection faulted; reopen required"));
        }
        self.checked_operation(operation(&connection))
    }
    pub(crate) fn transaction<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| invalid("database connection mutex poisoned"))?;
        if self.faulted.load(Ordering::Acquire) {
            return Err(invalid("database connection faulted; reopen required"));
        }
        let tx = self.checked_operation(
            connection
                .transaction()
                .map_err(|e| context("begin database transaction", e)),
        )?;
        match self.checked_operation(operation(&tx)) {
            Ok(value) => match tx.commit() {
                Ok(()) => Ok(value),
                Err(error) => {
                    self.faulted.store(true, Ordering::Release);
                    Err(error.into())
                }
            },
            Err(error) => match tx.rollback() {
                Ok(()) => Err(error),
                Err(rollback) => {
                    self.faulted.store(true, Ordering::Release);
                    Err(LogError::Database(Box::new(RollbackFailure {
                        operation: error,
                        rollback: rollback.into(),
                    })))
                }
            },
        }
    }
}

fn reclaim_pages_on(
    c: &Connection,
    budget: PhysicalReclaimBudget,
) -> Result<PhysicalReclaimProgress, LogError> {
    let before = PhysicalStats::read_on(c)?;
    if before.freelist_count == 0 {
        return Ok(PhysicalReclaimProgress {
            before,
            after: before,
            yielded_steps: 0,
        });
    }
    #[cfg(test)]
    observations::VACUUM_STATEMENTS.with(|n| n.set(n.get() + 1));
    let mut statement = c
        .prepare(&format!("PRAGMA main.incremental_vacuum({})", budget.0))
        .map_err(|error| context("prepare incremental vacuum", error))?;
    let mut yielded_steps = 0;
    {
        let mut rows = statement
            .query([])
            .map_err(|error| context("start incremental vacuum", error))?;
        // Bundled SQLite emits a zero-column ROW after EACH IncrVacuum opcode.
        // execute_batch steps just once; exhaust Rows without reading columns or
        // collecting results so every iteration AND late error reaches the owner.
        while rows
            .next()
            .map_err(|error| context("step incremental vacuum", error))?
            .is_some()
        {
            yielded_steps += 1;
            #[cfg(test)]
            if observations::INTERRUPT_AFTER.with(|n| {
                if n.get() == Some(yielded_steps) {
                    n.set(None);
                    true
                } else {
                    false
                }
            }) {
                c.get_interrupt_handle().interrupt();
            }
        }
    }
    #[cfg(test)]
    observations::VACUUM_VM_STEPS
        .with(|n| n.set(statement.get_status(rusqlite::StatementStatus::VmStep)));
    Ok(PhysicalReclaimProgress {
        before,
        after: PhysicalStats::read_on(c)?,
        yielded_steps,
    })
}

/// Shared driver-source classification for retryable local SQLite contention.
/// A faulted owner must still reject admission even when this returns true.
pub(crate) fn database_busy(error: &LogError) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        if let Some(driver) = error.downcast_ref::<rusqlite::Error>() {
            return matches!(
                driver.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            );
        }
        source = error.source();
    }
    false
}

fn fatal_database_error(error: &LogError) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        if let Some(driver) = error.downcast_ref::<rusqlite::Error>() {
            return matches!(
                driver.sqlite_error_code(),
                Some(
                    rusqlite::ErrorCode::DatabaseCorrupt
                        | rusqlite::ErrorCode::NotADatabase
                        | rusqlite::ErrorCode::SystemIoFailure
                )
            );
        }
        source = error.source();
    }
    false
}

// rusqlite 0.41 validates extension pragma names against pragma_pragma_list,
// which excludes SQLCipher pragmas and would also access schema before keying.
// Keep key bytes out of SQL by using the same native API as SQLCipher's key pragma.
fn assign_key(connection: &mut Connection, password: &[u8]) -> Result<(), LogError> {
    let length =
        i32::try_from(password.len()).map_err(|_| invalid("database passphrase is too long"))?;
    // SAFETY: connection is live and exclusively borrowed during initialization;
    // the slice remains valid for its explicit checked byte length. sqlite3_key
    // copies the key bytes before returning and does not retain the Rust pointer.
    let result = unsafe {
        rusqlite::ffi::sqlite3_key(connection.handle(), password.as_ptr().cast(), length)
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(context(
            "configure database key",
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(result), None),
        ));
    }
    Ok(())
}

fn prepare_private_regular_file(path: &Path) -> Result<bool, LogError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let (file, created) = match options.open(path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !std::fs::symlink_metadata(path)?.file_type().is_file() {
                return Err(invalid("database path must be a regular file"));
            }
            let mut existing = OpenOptions::new();
            existing.read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                existing.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            (existing.open(path)?, false)
        }
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(invalid("database path must be a regular file"));
    }
    Ok(created)
}

// Actual successful SQLite connection opens, isolated to each test thread.
// This measures resource ownership, not production password-derivation timing.
#[cfg(test)]
pub(crate) mod observations {
    thread_local! {
        pub static PASSIVE_ATTEMPTS: std::cell::RefCell<Vec<tokio::time::Instant>> = const {std::cell::RefCell::new(Vec::new())};
        pub static INTERRUPT_AFTER: std::cell::Cell<Option<u32>> = const {std::cell::Cell::new(None)};
        pub static FORCE_BUSY: std::cell::Cell<bool> = const {std::cell::Cell::new(false)};
        pub static VACUUM_ATTEMPTS: std::cell::RefCell<Vec<tokio::time::Instant>> = const {std::cell::RefCell::new(Vec::new())};
        pub static VACUUM_STATEMENTS: std::cell::Cell<usize> = const {std::cell::Cell::new(0)};
        pub static VACUUM_VM_STEPS: std::cell::Cell<i32> = const {std::cell::Cell::new(0)};
        pub static CONNECTION_OPENS: std::cell::Cell<usize> = const {std::cell::Cell::new(0)};
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;
    use std::fs;
    const PASSWORD: &str = "quote'\"; secret password";
    const BINDING: &[u8] = b"profile owner";
    const SECRET: &str = "unique private transcript content 852189";
    fn create(path: &Path) -> EncryptedDatabase {
        let db = EncryptedDatabase::open(path, PASSWORD, BINDING).unwrap();
        db.transaction(|tx| {
            tx.execute_batch("CREATE TABLE messages(body TEXT NOT NULL); CREATE INDEX messages_body ON messages(body)")
                .unwrap();
            tx.execute("INSERT INTO messages VALUES (?1)", [SECRET])
                .unwrap();
            Ok(())
        })
        .unwrap();
        db
    }
    fn body(db: &EncryptedDatabase) -> String {
        db.read(|c| {
            Ok(c.query_row("SELECT body FROM messages", [], |r| r.get(0))
                .unwrap())
        })
        .unwrap()
    }
    #[test]
    fn fresh_and_warm_database_require_incremental_vacuum() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = create(&path);
        assert_eq!(
            db.read(|c| Ok(c.pragma_query_value(None, "auto_vacuum", |r| r.get::<_, i64>(0))?))
                .unwrap(),
            2
        );
        assert!(root.path().join("profile.db-wal").exists());
        drop(db);
        let db = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        assert_eq!(body(&db), SECRET);
        assert_eq!(
            db.read(|c| Ok(c.pragma_query_value(None, "auto_vacuum", |r| r.get::<_, i64>(0))?))
                .unwrap(),
            2
        );
    }

    #[test]
    fn warm_nonincremental_authority_is_rejected_without_repair() {
        for mode in [0, 1] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("profile.db");
            let db = create(&path);
            // Fixture conversion only: production warm open must never VACUUM.
            db.read(|c| {
                c.pragma_update(None, "auto_vacuum", mode)?;
                c.execute_batch("VACUUM")?;
                assert_eq!(
                    c.pragma_query_value(None, "auto_vacuum", |r| r.get::<_, i64>(0))?,
                    mode
                );
                Ok(())
            })
            .unwrap();
            db.checkpoint_and_close().unwrap();
            let bytes = fs::read(&path).unwrap();
            let result = EncryptedDatabase::open(&path, PASSWORD, BINDING);
            assert!(result.is_err(), "accepted unsupported vacuum mode {mode}");
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    fn free_body_pages(db: &EncryptedDatabase) {
        db.transaction(|c| {
            c.execute_batch("CREATE TABLE disposable(body BLOB NOT NULL)")?;
            for _ in 0..48 {
                c.execute("INSERT INTO disposable VALUES(zeroblob(8192))", [])?;
            }
            c.execute("INSERT INTO disposable VALUES(zeroblob(2097152))", [])?;
            c.execute("DELETE FROM disposable", [])?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn physical_reclamation_drives_multiple_rows_and_continues_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = create(&path);
        free_body_pages(&db);
        let before = db.physical_stats().unwrap();
        assert!(before.freelist_count > 128);
        for pages in [1, 8, 64] {
            let started = std::time::Instant::now();
            let progress = db
                .reclaim_pages(PhysicalReclaimBudget::new(pages).unwrap())
                .unwrap();
            let elapsed = started.elapsed();
            let vm_steps = observations::VACUUM_VM_STEPS.with(|v| v.get());
            assert_eq!(progress.yielded_steps, pages);
            assert_eq!(
                progress.before.freelist_count - progress.after.freelist_count,
                u64::from(pages)
            );
            assert!(progress.after.page_count < progress.before.page_count);
            assert_eq!(body(&db), SECRET);
            println!(
                "pages={pages}, elapsed={elapsed:?}, vm_steps={vm_steps}, progress={progress:?}"
            );
            assert!(vm_steps > i32::try_from(pages).unwrap());
        }
        let continuation = db.physical_stats().unwrap();
        drop(db);
        let db = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        assert_eq!(db.physical_stats().unwrap(), continuation);
        let resumed = db.reclaim_pages(PhysicalReclaimBudget::default()).unwrap();
        assert_eq!(resumed.yielded_steps, 64);
        assert_eq!(body(&db), SECRET);
        db.read(|c| {
            assert!(
                c.query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
                    .optional()?
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
        for entry in [path.clone(), root.path().join("profile.db-wal")] {
            let bytes = fs::read(&entry).unwrap();
            assert!(!bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
            println!(
                "{}: {} bytes",
                entry.file_name().unwrap().to_string_lossy(),
                bytes.len()
            );
        }
    }

    #[test]
    fn physical_busy_operation_preserves_source_and_retries_without_fault() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = create(&path);
        free_body_pages(&db);
        let before = db.physical_stats().unwrap();
        let contender = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        contender
            .read(|c| {
                c.execute_batch("BEGIN IMMEDIATE")?;
                Ok(())
            })
            .unwrap();
        db.read(|c| {
            c.busy_timeout(Duration::ZERO)?;
            Ok(())
        })
        .unwrap();
        let error = db
            .reclaim_pages(PhysicalReclaimBudget::default())
            .unwrap_err();
        assert!(
            database_busy(&error),
            "actual driver contention must retain source: {error}"
        );
        assert!(!db.requires_reopen());
        assert_eq!(db.physical_stats().unwrap(), before);
        contender
            .read(|c| {
                c.execute_batch("ROLLBACK")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            db.reclaim_pages(PhysicalReclaimBudget::default())
                .unwrap()
                .yielded_steps,
            64
        );
    }

    #[test]
    fn passive_checkpoint_distinguishes_no_wal_and_partial_reader_progress() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = create(&path);
        db.checkpoint().unwrap();
        db.read(|c| {
            c.pragma_update(None, "journal_mode", "DELETE")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            db.passive_checkpoint().unwrap(),
            PassiveCheckpointProgress::NoWal
        );
        db.read(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            Ok(())
        })
        .unwrap();
        let reader = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        reader
            .read(|c| {
                c.execute_batch("BEGIN")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(body(&reader), SECRET);
        db.transaction(|c| {
            c.execute("INSERT INTO messages VALUES('pending')", [])?;
            Ok(())
        })
        .unwrap();
        let partial = db.passive_checkpoint().unwrap();
        assert!(
            !partial.complete(),
            "busy flag zero must not hide reader-pinned frames: {partial:?}"
        );
        reader
            .read(|c| {
                c.execute_batch("ROLLBACK")?;
                Ok(())
            })
            .unwrap();
        assert!(db.passive_checkpoint().unwrap().complete());
    }

    #[test]
    fn wal_reclamation_does_not_promise_immediate_main_file_shrink() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = create(&path);
        free_body_pages(&db);
        db.checkpoint().unwrap();
        let reader = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        reader
            .read(|c| {
                c.execute_batch("BEGIN")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(body(&reader), SECRET);
        let main_before = fs::metadata(&path).unwrap().len();
        let progress = db.reclaim_pages(PhysicalReclaimBudget::default()).unwrap();
        assert_eq!(progress.yielded_steps, 64);
        assert!(progress.after.page_count < progress.before.page_count);
        assert_eq!(fs::metadata(&path).unwrap().len(), main_before);
        let wal = fs::metadata(root.path().join("profile.db-wal"))
            .unwrap()
            .len();
        assert!(wal > 0);
        db.read(|c| {
            c.busy_timeout(Duration::ZERO)?;
            Ok(())
        })
        .unwrap();
        assert!(database_busy(&db.checkpoint().unwrap_err()));
        assert_eq!(body(&reader), SECRET);
        println!(
            "pinned reader: SQL pages {} -> {}; main unchanged {main_before} bytes, WAL {wal} bytes",
            progress.before.page_count, progress.after.page_count
        );
        reader
            .read(|c| {
                c.execute_batch("ROLLBACK")?;
                Ok(())
            })
            .unwrap();
        drop(reader);
        drop(db);
        let reopened = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        assert_eq!(reopened.physical_stats().unwrap(), progress.after);
        assert_eq!(body(&reopened), SECRET);
    }

    #[test]
    fn incremental_vacuum_late_interrupt_is_not_false_committed_progress() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = create(&path);
        free_body_pages(&db);
        let before = db.physical_stats().unwrap();
        observations::INTERRUPT_AFTER.with(|n| n.set(Some(1)));
        let error = db
            .reclaim_pages(PhysicalReclaimBudget::new(8).unwrap())
            .unwrap_err();
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        let mut interrupted = false;
        while let Some(error) = cause {
            if let Some(driver) = error.downcast_ref::<rusqlite::Error>() {
                interrupted |=
                    driver.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted);
            }
            cause = error.source();
        }
        assert!(
            interrupted,
            "late SQLITE_INTERRUPT must retain source: {error}"
        );
        // SQLite rolls back interrupted write transactions. The owner's failed
        // explicit rollback still faults the handle and preserves both causes.
        assert!(db.requires_reopen());
        drop(db);
        let reopened = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        assert_eq!(reopened.physical_stats().unwrap(), before);
        assert_eq!(body(&reopened), SECRET);
        assert_eq!(
            reopened
                .reclaim_pages(PhysicalReclaimBudget::new(8).unwrap())
                .unwrap()
                .yielded_steps,
            8
        );
    }

    #[test]
    fn incremental_vacuum_commit_failure_uses_existing_fault_boundary() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = create(&path);
        free_body_pages(&db);
        let result = db.transaction(|c| {
            let progress = reclaim_pages_on(c, PhysicalReclaimBudget::new(8)?)?;
            assert_eq!(progress.yielded_steps, 8);
            c.execute_batch("COMMIT")?;
            Ok(progress)
        });
        assert!(
            result.is_err(),
            "owner COMMIT failure cannot return successful progress"
        );
        assert!(db.requires_reopen());
        drop(db);
        assert_eq!(
            body(&EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap()),
            SECRET
        );
    }

    #[test]
    fn physical_budget_rejects_zero_and_excess_and_idle_does_no_vacuum() {
        assert!(PhysicalReclaimBudget::new(0).is_err());
        assert!(PhysicalReclaimBudget::new(65).is_err());
        let root = tempfile::tempdir().unwrap();
        let db = create(&root.path().join("profile.db"));
        let before = db.physical_stats().unwrap();
        assert_eq!(before.freelist_count, 0);
        let changes = db.read(|c| Ok(c.total_changes())).unwrap();
        observations::VACUUM_STATEMENTS.with(|n| n.set(0));
        let progress = db.reclaim_pages(PhysicalReclaimBudget::default()).unwrap();
        assert_eq!(progress.yielded_steps, 0);
        assert_eq!(progress.before, before);
        assert_eq!(progress.after, before);
        assert_eq!(db.read(|c| Ok(c.total_changes())).unwrap(), changes);
        assert_eq!(observations::VACUUM_STATEMENTS.with(|n| n.get()), 0);
    }

    #[test]
    fn checked_close_preserves_encrypted_wal_rows_after_relocation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let db = create(&path);
        assert!(
            fs::metadata(dir.path().join("profile.db-wal"))
                .unwrap()
                .len()
                > 0
        );
        db.checkpoint_and_close().unwrap();
        let relocated = dir.path().join("relocated.db");
        fs::rename(&path, &relocated).unwrap();
        let reopened = EncryptedDatabase::open(&relocated, PASSWORD, BINDING).unwrap();
        assert_eq!(body(&reopened), SECRET);
        reopened.checkpoint_and_close().unwrap();
    }
    #[test]
    fn checked_close_refuses_reader_snapshot_and_preserves_sqlite_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let db = create(&path);
        let reader = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        reader
            .read(|c| {
                c.execute_batch("BEGIN")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(body(&reader), SECRET);
        db.transaction(|tx| {
            tx.execute("INSERT INTO messages VALUES ('new committed row')", [])?;
            Ok(())
        })
        .unwrap();
        db.read(|c| {
            c.busy_timeout(Duration::ZERO)?;
            Ok(())
        })
        .unwrap();
        let error = db.checkpoint_and_close().unwrap_err();
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        let mut busy = false;
        while let Some(error) = source {
            if let Some(error) = error.downcast_ref::<rusqlite::Error>() {
                busy = error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy);
            }
            source = error.source();
        }
        assert!(
            busy,
            "checkpoint must retain a typed SQLite busy source: {error}"
        );
        assert!(path.exists());
        assert!(dir.path().join("profile.db-wal").exists());
        reader
            .read(|c| {
                c.execute_batch("ROLLBACK")?;
                Ok(())
            })
            .unwrap();
        reader.checkpoint_and_close().unwrap();
        let reopened = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        assert_eq!(
            reopened
                .read(|c| Ok(
                    c.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))?
                ))
                .unwrap(),
            2
        );
    }
    #[test]
    fn checked_close_preserves_checkpoint_driver_error_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let db = create(&path);
        db.read(|c| {
            c.execute_batch("BEGIN IMMEDIATE; UPDATE messages SET body = body")?;
            Ok(())
        })
        .unwrap();
        let error = db.checkpoint_and_close().unwrap_err();
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        let mut locked = false;
        while let Some(error) = source {
            if let Some(error) = error.downcast_ref::<rusqlite::Error>() {
                locked = error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseLocked);
            }
            source = error.source();
        }
        assert!(
            locked,
            "checkpoint must retain its actual driver error: {error}"
        );
        assert_eq!(
            body(&EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap()),
            SECRET
        );
    }

    #[test]
    fn checked_close_refuses_faulted_and_poisoned_handles() {
        let dir = tempfile::tempdir().unwrap();
        let db = create(&dir.path().join("faulted.db"));
        db.faulted.store(true, Ordering::Release);
        assert!(db.checkpoint_and_close().is_err());
        let db = create(&dir.path().join("poisoned.db"));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = db.connection.lock().unwrap();
            panic!("poison connection");
        }));
        assert!(db.checkpoint_and_close().is_err());
    }
    #[test]
    fn encrypted_roundtrip_cipher_and_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let db = create(&path);
        db.read(|c| {
            let cipher: String = c
                .query_row("PRAGMA cipher_version", [], |r| r.get(0))
                .unwrap();
            assert!(!cipher.is_empty());
            let provider: String = c
                .query_row("PRAGMA cipher_provider", [], |r| r.get(0))
                .unwrap();
            assert_eq!(provider, "openssl");
            let kdf: String = c
                .query_row("PRAGMA cipher_kdf_algorithm", [], |r| r.get(0))
                .unwrap();
            assert_eq!(kdf, "PBKDF2_HMAC_SHA512");
            println!("SQLCipher {cipher}; crypto provider {provider}; KDF {kdf}");
            Ok(())
        })
        .unwrap();
        for path in [&path, &dir.path().join("profile.db-wal")] {
            let bytes = fs::read(path).unwrap();
            assert!(!bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
            assert!(!bytes.starts_with(b"SQLite format 3"));
        }
        drop(db);
        assert_eq!(
            body(&EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap()),
            SECRET
        );
    }
    #[test]
    fn wrong_key_binding_and_newer_schema_preserve_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        drop(create(&path));
        assert!(EncryptedDatabase::open(&path, "wrong", BINDING).is_err());
        assert!(EncryptedDatabase::open(&path, PASSWORD, b"other owner").is_err());
        let db = EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap();
        assert_eq!(body(&db), SECRET);
        db.read(|c| {
            c.pragma_update(None, "user_version", 2).unwrap();
            Ok(())
        })
        .unwrap();
        drop(db);
        let original = fs::read(&path).unwrap();
        assert!(EncryptedDatabase::open(&path, PASSWORD, BINDING).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn failed_transaction_rolls_back_and_remains_usable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let db = create(&path);
        let result: Result<(), LogError> = db.transaction(|tx| {
            tx.execute("UPDATE messages SET body = 'discarded'", [])
                .unwrap();
            Err(LogError::CorruptFile("caller rejected write".into()))
        });
        assert!(result.is_err());
        assert_eq!(body(&db), SECRET);
        db.transaction(|tx| {
            tx.execute("UPDATE messages SET body = ?1", [SECRET])?;
            Ok(())
        })
        .unwrap();
        drop(db);
        assert_eq!(
            body(&EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap()),
            SECRET
        );
    }
    #[test]
    fn connection_has_explicit_operational_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let db =
            EncryptedDatabase::open(&dir.path().join("profile.db"), PASSWORD, BINDING).unwrap();
        db.read(|c| {
            for (name, expected) in [
                ("foreign_keys", 1),
                ("auto_vacuum", 2),
                ("cache_size", -4096),
                ("mmap_size", 0),
                ("temp_store", 2),
                ("synchronous", 2),
                ("wal_autocheckpoint", 256),
                ("journal_size_limit", 4194304),
                ("busy_timeout", 5000),
            ] {
                let actual: i64 = c.pragma_query_value(None, name, |r| r.get(0)).unwrap();
                assert_eq!(actual, expected, "{name}");
            }
            let iterations: String = c.query_row("PRAGMA kdf_iter", [], |r| r.get(0)).unwrap();
            assert_eq!(iterations, "1");
            let mode: String = c
                .pragma_query_value(None, "journal_mode", |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn corruption_and_foreign_application_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let db = create(&path);
        db.read(|c| {
            c.pragma_update(None, "application_id", 42).unwrap();
            Ok(())
        })
        .unwrap();
        drop(db);
        assert!(EncryptedDatabase::open(&path, PASSWORD, BINDING).is_err());
        let corrupt = dir.path().join("corrupt.db");
        fs::write(&corrupt, b"corrupt database bytes").unwrap();
        assert!(EncryptedDatabase::open(&corrupt, PASSWORD, BINDING).is_err());
        assert_eq!(fs::read(&corrupt).unwrap(), b"corrupt database bytes");
    }
    #[test]
    fn poisoned_connection_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let db =
            EncryptedDatabase::open(&dir.path().join("profile.db"), PASSWORD, BINDING).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), LogError> = db.transaction(|_| panic!("simulated caller panic"));
        }));
        assert!(db.read(|_| Ok(())).is_err());
        assert!(db.transaction(|_| Ok(())).is_err());
    }
    #[test]
    fn d_regression_failed_rollback_preserves_primary_database_error() {
        use std::error::Error;
        for primary in [
            LogError::Database(Box::new(std::io::Error::other("primary write failure"))),
            LogError::AdmissionRejected("ordinary policy rejection".into()),
        ] {
            let root = tempfile::tempdir().unwrap();
            let db = create(&root.path().join("profile.db"));
            let expected = primary.to_string();
            let result: Result<(), LogError> = db.transaction(|c| {
                c.execute_batch("ROLLBACK")?;
                Err(primary)
            });
            let error = result.unwrap_err();
            assert!(db.requires_reopen());
            let LogError::Database(cause) = &error else {
                panic!("rollback failure cannot remain an ordinary outcome")
            };
            let both = cause.downcast_ref::<RollbackFailure>().unwrap();
            assert_eq!(both.operation.to_string(), expected);
            assert!(matches!(both.rollback, LogError::Database(_)));
            assert_eq!(both.source().unwrap().to_string(), expected);
            assert!(error.to_string().contains(&expected));
            assert!(error.to_string().contains("rollback failed"));
        }
    }
    #[test]
    fn failed_commit_and_rollback_fault_the_connection() {
        for statement in ["COMMIT", "ROLLBACK"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("profile.db");
            let db = create(&path);
            let result = db.transaction(|tx| {
                tx.execute_batch(statement).unwrap();
                if statement == "ROLLBACK" {
                    Err(LogError::CorruptFile("aborted".into()))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert!(db.read(|_| Ok(())).is_err());
            assert!(db.transaction(|_| Ok(())).is_err());
            drop(db);
            assert_eq!(
                body(&EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap()),
                SECRET
            );
        }
    }
    #[test]
    fn empty_password_never_creates_plaintext_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        assert!(EncryptedDatabase::open(&path, "", BINDING).is_err());
        assert!(!path.exists());
    }
    #[test]
    fn hex_shaped_password_is_a_passphrase_not_a_raw_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let password = format!("x'{}'", "42".repeat(32));
        let db = EncryptedDatabase::open(&path, &password, BINDING).unwrap();
        drop(db);
        drop(EncryptedDatabase::open(&path, &password, BINDING).unwrap());
        let mut raw = Connection::open(&path).unwrap();
        assign_key(&mut raw, password.as_bytes()).unwrap();
        raw.execute_batch("PRAGMA cipher_compatibility = 4")
            .unwrap();
        raw.execute_batch(&format!("PRAGMA kdf_iter = {KDF_ITERATIONS}"))
            .unwrap();
        assert!(
            raw.query_row("SELECT count(*) FROM sqlite_master", [], |r| r
                .get::<_, i64>(0))
                .is_err(),
            "user password bypassed the password KDF"
        );
    }
    #[test]
    fn empty_binding_never_creates_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        assert!(EncryptedDatabase::open(&path, PASSWORD, b"").is_err());
        assert!(!path.exists());
    }
    #[test]
    fn fatal_query_corruption_faults_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        let db = create(&path);
        db.read(|c| {
            c.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_master SET rootpage=2147483647 WHERE name='messages'; PRAGMA writable_schema=RESET")?;
            Ok(())
        }).unwrap();
        let result = db
            .read(|c| Ok(c.query_row("SELECT body FROM messages", [], |r| r.get::<_, String>(0))?));
        assert!(result.is_err());
        assert!(db.read(|_| Ok(())).is_err());
        assert!(db.transaction(|_| Ok(())).is_err());
    }
    #[test]
    fn embedded_nul_password_is_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        drop(EncryptedDatabase::open(&path, "passphrase\0suffix", BINDING).unwrap());
        assert!(EncryptedDatabase::open(&path, "passphrase", BINDING).is_err());
        drop(EncryptedDatabase::open(&path, "passphrase\0suffix", BINDING).unwrap());
    }
    #[test]
    fn checked_begin_result_faults_only_fatal_storage_errors() {
        for (code, fatal) in [
            (rusqlite::ffi::SQLITE_IOERR, true),
            (rusqlite::ffi::SQLITE_CORRUPT, true),
            (rusqlite::ffi::SQLITE_NOTADB, true),
            (rusqlite::ffi::SQLITE_BUSY, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("profile.db");
            let db = create(&path);
            // BEGIN IO failures are VFS/platform-dependent. Exercise the same
            // checked result boundary with the exact driver codes it receives.
            let error = context(
                "begin database transaction",
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None),
            );
            let result = db.checked_operation::<()>(Err(error));
            assert!(result.is_err());
            assert_eq!(db.read(|_| Ok(())).is_err(), fatal);
            assert_eq!(db.transaction(|_| Ok(())).is_err(), fatal);
        }
    }
    fn assert_existing_empty_preserved(encrypted: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        if encrypted {
            let mut external = Connection::open(&path).unwrap();
            let passphrase = Zeroizing::new(format!("{PASSPHRASE_DOMAIN}{PASSWORD}"));
            assign_key(&mut external, passphrase.as_bytes()).unwrap();
            external
                .execute_batch("PRAGMA cipher_compatibility = 4")
                .unwrap();
            external
                .execute_batch(&format!("PRAGMA kdf_iter = {KDF_ITERATIONS}"))
                .unwrap();
            external
                .execute_batch("CREATE TABLE discarded(x); DROP TABLE discarded")
                .unwrap();
        } else {
            fs::write(&path, []).unwrap();
        }
        let original = fs::read(&path).unwrap();
        assert!(
            EncryptedDatabase::open(&path, PASSWORD, BINDING).is_err(),
            "adopted an existing empty database: encrypted={encrypted}"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn preexisting_zero_byte_file_is_preserved() {
        assert_existing_empty_preserved(false);
    }
    #[test]
    fn preexisting_encrypted_empty_schema_is_preserved() {
        assert_existing_empty_preserved(true);
    }
    #[test]
    fn rejects_nonregular_database_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(EncryptedDatabase::open(dir.path(), PASSWORD, BINDING).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn private_new_file_and_reject_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.db");
        drop(create(&path));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = dir.path().join("link.db");
        symlink(&path, &link).unwrap();
        assert!(EncryptedDatabase::open(&link, PASSWORD, BINDING).is_err());
        assert_eq!(
            body(&EncryptedDatabase::open(&path, PASSWORD, BINDING).unwrap()),
            SECRET
        );
    }
}

#[cfg(test)]
mod publication_checkpoint_tests {
    use super::*;
    #[test]
    fn publication_checkpoint_retries_busy_snapshot_without_closing_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = EncryptedDatabase::open(&path, "pw", b"fixture").unwrap();
        db.transaction(|tx|{tx.execute_batch("CREATE TABLE values_for_checkpoint(value INTEGER);INSERT INTO values_for_checkpoint VALUES(1)")?;Ok(())}).unwrap();
        let reader = EncryptedDatabase::open(&path, "pw", b"fixture").unwrap();
        reader
            .read(|c| {
                c.execute_batch("BEGIN")?;
                let _: i64 =
                    c.query_row("SELECT value FROM values_for_checkpoint", [], |r| r.get(0))?;
                Ok(())
            })
            .unwrap();
        db.transaction(|tx| {
            tx.execute("INSERT INTO values_for_checkpoint VALUES(2)", [])?;
            Ok(())
        })
        .unwrap();
        db.read(|c| {
            c.busy_timeout(Duration::ZERO)?;
            Ok(())
        })
        .unwrap();
        assert!(db.checkpoint().is_err());
        let count: i64 = db
            .read(|c| {
                Ok(
                    c.query_row("SELECT count(*) FROM values_for_checkpoint", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(count, 2);
        reader
            .read(|c| {
                c.execute_batch("ROLLBACK")?;
                Ok(())
            })
            .unwrap();
        drop(reader);
        db.checkpoint().unwrap();
        db.checkpoint_and_close().unwrap();
    }
}
