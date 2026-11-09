//! A closed write boundary for disposable participant selection tables.
//! The database mutex owns this guard through commit/rollback and restoration.
use super::{Connection, LogError, context};
use rusqlite::ffi;
use std::ffi::{CStr, c_char, c_int, c_void};

unsafe extern "C" fn authorize(
    _: *mut c_void,
    action: c_int,
    table: *const c_char,
    _: *const c_char,
    database: *const c_char,
    _: *const c_char,
) -> c_int {
    match action {
        ffi::SQLITE_READ
        | ffi::SQLITE_SELECT
        | ffi::SQLITE_FUNCTION
        | ffi::SQLITE_TRANSACTION
        | ffi::SQLITE_SAVEPOINT
        | ffi::SQLITE_RECURSIVE => ffi::SQLITE_OK,
        ffi::SQLITE_INSERT | ffi::SQLITE_UPDATE | ffi::SQLITE_DELETE => {
            if table.is_null() || database.is_null() {
                return ffi::SQLITE_DENY;
            }
            // SAFETY: SQLite supplies nullable NUL-terminated callback arguments.
            let (table, database) = unsafe {
                (
                    CStr::from_ptr(table).to_bytes(),
                    CStr::from_ptr(database).to_bytes(),
                )
            };
            // TEMP scratch is not part of MAIN work evidence. Trigger writes
            // back into MAIN are authorized independently and remain fenced.
            if database == b"temp"
                || (database == b"main"
                    && [
                        b"participant_sync_views".as_slice(),
                        b"participant_sync_rows",
                        b"participant_sync_have",
                        b"participant_sync_ancestors",
                        b"participant_sync_departures",
                        b"participant_sync_authors",
                        b"participant_sync_progress",
                        b"participant_sync_receipts",
                        b"participant_sync_export_work",
                    ]
                    .contains(&table))
            {
                ffi::SQLITE_OK
            } else {
                ffi::SQLITE_DENY
            }
        }
        _ => ffi::SQLITE_DENY,
    }
}

pub(super) struct Guard(*mut ffi::sqlite3);
impl Guard {
    /// Caller must retain the exclusive live connection until this guard drops.
    pub(super) fn install(connection: &Connection) -> Result<Self, LogError> {
        // SAFETY: connection is live and exclusively locked by the owner. This
        // owner has no other authorizer. SQLite expires previously prepared
        // statements when changing the authorizer, so caches cannot bypass it.
        let database = unsafe { connection.handle() };
        let result =
            unsafe { ffi::sqlite3_set_authorizer(database, Some(authorize), std::ptr::null_mut()) };
        if result != ffi::SQLITE_OK {
            return Err(context(
                "guard disposable selection writes",
                rusqlite::Error::SqliteFailure(ffi::Error::new(result), None),
            ));
        }
        Ok(Self(database))
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: lexical transaction ownership drops this before releasing the
        // mutex, including rollback and unwinding. Restores ordinary authority.
        unsafe {
            ffi::sqlite3_set_authorizer(self.0, None, std::ptr::null_mut());
        }
    }
}
