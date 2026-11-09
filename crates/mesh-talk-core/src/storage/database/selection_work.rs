//! Closed write boundaries for disposable selections and transport hints.
//! The database mutex owns this guard through commit/rollback and restoration.
use super::{Connection, LogError, context};
use rusqlite::ffi;
use std::ffi::{CStr, c_char, c_int, c_void};

#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Selection,
    TransportHint,
    ChannelScheduling,
}
impl Purpose {
    fn tables(self) -> &'static [&'static [u8]] {
        match self {
            Self::Selection => &[
                b"participant_sync_views",
                b"participant_sync_rows",
                b"participant_sync_have",
                b"participant_sync_ancestors",
                b"participant_sync_departures",
                b"participant_sync_authors",
                b"participant_sync_progress",
                b"participant_sync_receipts",
                b"participant_sync_export_work",
            ],
            Self::ChannelScheduling => &[b"channel_runtime_work"],
            Self::TransportHint => &[
                b"opaque_checkpoints",
                b"opaque_wrapper_ids",
                b"opaque_checkpoint_usage",
            ],
        }
    }
}

unsafe extern "C" fn authorize(
    context: *mut c_void,
    action: c_int,
    table: *const c_char,
    _: *const c_char,
    database: *const c_char,
    _: *const c_char,
) -> c_int {
    if context.is_null() {
        return ffi::SQLITE_DENY;
    }
    // SAFETY: install supplies one of the immutable program-lifetime statics;
    // SQLite retains no borrowed connection or transaction in this context.
    let purpose = unsafe { *(context.cast::<Purpose>()) };
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
            if database == b"temp" || (database == b"main" && purpose.tables().contains(&table)) {
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
    pub(super) fn install(connection: &Connection, purpose: Purpose) -> Result<Self, LogError> {
        static SELECTION: Purpose = Purpose::Selection;
        static TRANSPORT_HINT: Purpose = Purpose::TransportHint;
        static CHANNEL_SCHEDULING: Purpose = Purpose::ChannelScheduling;
        let policy_context = match purpose {
            Purpose::Selection => &SELECTION,
            Purpose::TransportHint => &TRANSPORT_HINT,
            Purpose::ChannelScheduling => &CHANNEL_SCHEDULING,
        } as *const Purpose;
        // SAFETY: connection is live and exclusively locked by the owner. This
        // owner has no other authorizer. SQLite expires previously prepared
        // statements when changing the authorizer, so caches cannot bypass it.
        let database = unsafe { connection.handle() };
        let result = unsafe {
            ffi::sqlite3_set_authorizer(database, Some(authorize), policy_context.cast_mut().cast())
        };
        if result != ffi::SQLITE_OK {
            return Err(context(
                "guard derived work writes",
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
