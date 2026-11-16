//! Opt-in pager-write attribution. Stores static operation classes, never SQL or values.
//! SQLite page writes are not OS/NAND bytes or evidence of business acceptance.
use rusqlite::{Connection, ffi};
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_void},
    sync::{Mutex, OnceLock},
};

const LIMIT: usize = 64;
const NAMES: [&str; 12] = [
    "other",
    "participant_sync",
    "file_receive",
    "file_download",
    "file_consent",
    "channel",
    "events",
    "media",
    "relay",
    "reclaim",
    "checkpoint",
    "schema",
];
const FAMILIES: usize = NAMES.len();
struct Entry {
    id: u64,
    last: u64,
    family: usize,
    pages: [u64; FAMILIES],
}
#[derive(Default)]
struct State {
    next: u64,
    entries: BTreeMap<usize, Entry>,
    closed: Vec<Entry>,
    overflow_pages: u64,
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}
fn retire(s: &mut State, entry: Entry) {
    if entry.pages.iter().all(|pages| *pages == 0) {
        return;
    }
    if s.closed.len() == LIMIT {
        let old = s.closed.remove(0);
        s.overflow_pages = old
            .pages
            .iter()
            .fold(s.overflow_pages, |sum, n| sum.saturating_add(*n));
    }
    s.closed.push(entry);
}
fn contains(sql: &[u8], part: &[u8]) -> bool {
    sql.windows(part.len())
        .any(|s| s.eq_ignore_ascii_case(part))
}
fn family(sql: &[u8]) -> usize {
    let sql = &sql[..sql.len().min(2048)];
    for (index, needles) in [
        (1, &[b"participant_sync_".as_slice()][..]),
        (2, &[b"file_receive_".as_slice()][..]),
        (3, &[b"file_download_".as_slice()][..]),
        (4, &[b"file_consent_".as_slice()][..]),
        (5, &[b"channel_".as_slice(), b"event_order_"][..]),
        (
            6,
            &[
                b"retained_events".as_slice(),
                b"event_conversations",
                b"participant_event_",
            ][..],
        ),
        (7, &[b"media_".as_slice()][..]),
        (8, &[b"relay_".as_slice(), b"mailbox_"][..]),
        (9, &[b"incremental_vacuum".as_slice()][..]),
        (10, &[b"wal_checkpoint".as_slice()][..]),
        (11, &[b"CREATE ".as_slice(), b"DROP ", b"ALTER "][..]),
    ] {
        if needles.iter().any(|part| contains(sql, part)) {
            return index;
        }
    }
    0
}
// The caller owns the live SQLite connection; this is scalar status inspection,
// not SQL execution, and it neither resets counters nor changes pager policy.
unsafe fn written_pages(db: *mut ffi::sqlite3) -> Option<u64> {
    let (mut current, mut high) = (0, 0);
    // SAFETY: db is live for the owning connection/SQLite trace callback.
    let result = unsafe {
        ffi::sqlite3_db_status(
            db,
            ffi::SQLITE_DBSTATUS_CACHE_WRITE,
            &mut current,
            &mut high,
            0,
        )
    };
    if result != ffi::SQLITE_OK {
        return None;
    }
    u64::try_from(current).ok()
}
unsafe extern "C" fn trace(kind: u32, _: *mut c_void, pointer: *mut c_void, _: *mut c_void) -> i32 {
    if pointer.is_null() {
        return 0;
    }
    if kind == ffi::SQLITE_TRACE_CLOSE {
        if let Ok(mut s) = state().try_lock()
            && let Some(entry) = s.entries.remove(&(pointer as usize))
        {
            retire(&mut s, entry);
        }
        return 0;
    }
    let stmt = pointer.cast::<ffi::sqlite3_stmt>();
    // SAFETY: STMT/PROFILE provide a live sqlite3_stmt for this callback only.
    let db = unsafe { ffi::sqlite3_db_handle(stmt) };
    let raw = unsafe { ffi::sqlite3_sql(stmt) };
    if db.is_null() || raw.is_null() {
        return 0;
    }
    // SQL templates are inspected only to select a fixed label. Neither this
    // borrowed text nor parameters are retained, expanded or logged.
    let sql = unsafe { CStr::from_ptr(raw) }.to_bytes();
    let Ok(mut s) = state().try_lock() else {
        return 0;
    };
    let Some(entry) = s.entries.get_mut(&(db as usize)) else {
        return 0;
    };
    if kind == ffi::SQLITE_TRACE_STMT {
        if sql.starts_with(b"BEGIN") {
            entry.family = 0;
        } else if !sql.starts_with(b"COMMIT")
            && !sql.starts_with(b"ROLLBACK")
            && unsafe { ffi::sqlite3_stmt_readonly(stmt) } == 0
        {
            entry.family = family(sql);
        }
    } else if kind == ffi::SQLITE_TRACE_PROFILE {
        if let Some(current) = unsafe { written_pages(db) } {
            let pages = current.checked_sub(entry.last).unwrap_or(current);
            entry.last = current;
            entry.pages[entry.family] = entry.pages[entry.family].saturating_add(pages);
        }
        if sql.starts_with(b"COMMIT") || sql.starts_with(b"ROLLBACK") {
            entry.family = 0;
        }
    }
    0
}
/// Called only by the explicitly enabled work observer, before publishing the
/// exclusive connection. No callback context pointer or borrowed Rust owner survives.
pub(super) fn install(connection: &mut Connection) {
    // SAFETY: exclusive connection borrow; handle remains SQLite-owned. CLOSE
    // removes its bounded registration and the callback context is static/null.
    let db = unsafe { connection.handle() };
    let Some(last) = (unsafe { written_pages(db) }) else {
        return;
    };
    let Ok(mut s) = state().try_lock() else {
        return;
    };
    if let Some(old) = s.entries.remove(&(db as usize)) {
        retire(&mut s, old);
    }
    if s.entries.len() == LIMIT {
        return;
    }
    s.next = s.next.saturating_add(1);
    let id = s.next;
    s.entries.insert(
        db as usize,
        Entry {
            id,
            last,
            family: 0,
            pages: [0; FAMILIES],
        },
    );
    let result = unsafe {
        ffi::sqlite3_trace_v2(
            db,
            ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_CLOSE,
            Some(trace),
            std::ptr::null_mut(),
        )
    };
    if result != ffi::SQLITE_OK {
        s.entries.remove(&(db as usize));
    }
}
/// Sampled by the existing opt-in maintenance observer after releasing storage
/// guards. Callback and sampling use try_lock; attribution is explicitly best-effort.
pub(crate) fn snapshot() -> Option<String> {
    use std::fmt::Write;
    let Ok(mut s) = state().try_lock() else {
        return None;
    };
    let mut result = format!(
        "sql-page-work best_effort=true connections={}",
        s.entries.len()
    );
    for entry in s.closed.iter().chain(s.entries.values()) {
        for (family, pages) in entry.pages.iter().enumerate() {
            if *pages != 0 {
                let _ = write!(
                    result,
                    " [db={} kind={} pages={}]",
                    entry.id, NAMES[family], pages
                );
            }
        }
    }
    let _ = write!(result, " overflow_pages={}", s.overflow_pages);
    s.closed.clear();
    s.overflow_pages = 0;
    for entry in s.entries.values_mut() {
        entry.pages = [0; FAMILIES];
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pager_writes_distinguish_committed_updates_from_reads_and_noops() {
        let root = tempfile::tempdir().unwrap();
        let mut connection = Connection::open(root.path().join("pages.db")).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        install(&mut connection);
        connection.execute_batch("CREATE TABLE file_receive_pending(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO file_receive_pending VALUES(1,'first');").unwrap();
        let before = observed_for_test(&connection);
        assert!(
            before > 0,
            "real pager writes must be attributed, not inferred from affected rows"
        );
        connection
            .query_row("SELECT value FROM file_receive_pending", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap();
        connection
            .execute(
                "UPDATE file_receive_pending SET value='ignored' WHERE id=2",
                [],
            )
            .unwrap();
        assert_eq!(
            observed_for_test(&connection),
            before,
            "reads and no-op updates write no pages"
        );
        connection
            .execute_batch(
                "BEGIN; UPDATE file_receive_pending SET value='second' WHERE id=1; COMMIT;",
            )
            .unwrap();
        assert!(
            observed_for_test(&connection) > before,
            "commit-owned pager flushes retain their statement family"
        );
        // CLOSE retires only diagnostic ownership; flushed counts survive until sampling.
        let key = unsafe { connection.handle() } as usize;
        drop(connection);
        assert!(!state().lock().unwrap().entries.contains_key(&key));
        let report = snapshot().unwrap();
        assert!(report.contains("kind=file_receive"));
        assert!(!report.contains("first") && !report.contains("second"));
    }
    fn observed_for_test(connection: &Connection) -> u64 {
        // SAFETY: the live connection is borrowed and no statement is executing.
        let key = unsafe { connection.handle() } as usize;
        state().lock().unwrap().entries[&key].pages.iter().sum()
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;
    #[test]
    fn closed_connection_attribution_has_a_fixed_bound() {
        let mut s = State::default();
        for id in 0..(LIMIT + 4) {
            let mut pages = [0; FAMILIES];
            pages[1] = 3;
            retire(
                &mut s,
                Entry {
                    id: id as u64,
                    last: 0,
                    family: 1,
                    pages,
                },
            );
        }
        assert_eq!(s.closed.len(), LIMIT);
        assert_eq!(s.overflow_pages, 12);
    }
}
