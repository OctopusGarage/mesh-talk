//! Exact historical dispatch acceptance, never live custody or delivery authority.
use super::indexed::IndexedFileBook;
use crate::{
    eventlog::{ConversationId, EventId, LogError},
    identity::device::PublicIdentity,
};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "
CREATE TABLE file_book_custody_dispatch(account BLOB NOT NULL CHECK(length(account)=32),ordinal INTEGER NOT NULL,recipient_ed BLOB NOT NULL CHECK(length(recipient_ed)=32),recipient_x BLOB NOT NULL CHECK(length(recipient_x)=32),demand BLOB CHECK(demand IS NULL OR length(demand)=32),PRIMARY KEY(account,ordinal,recipient_ed,recipient_x),FOREIGN KEY(account,ordinal) REFERENCES file_book_offers(account,ordinal) ON DELETE CASCADE) WITHOUT ROWID;
";

pub(super) fn initialize_on(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_book_custody_dispatch')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}

impl IndexedFileBook {
    /// A previous dispatch for this exact demand was durably accepted. This
    /// fact grants no access and makes no assertion about current remote bytes.
    pub(crate) fn group_custody_settled(
        &self,
        file: ConversationId,
        manifest: EventId,
        recipient: &PublicIdentity,
        demand: Option<EventId>,
    ) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            let mut statement = c.prepare_cached("SELECT EXISTS(SELECT 1 FROM file_book_offers o JOIN file_book_custody_dispatch d ON d.account=o.account AND d.ordinal=o.ordinal WHERE o.account=?1 AND o.file=?2 AND o.event=?3 AND d.recipient_ed=?4 AND d.recipient_x=?5 AND d.demand IS ?6)")?;
            Ok(statement.query_row(params![self.account.as_slice(), file.as_bytes().as_slice(), manifest.as_bytes().as_slice(), recipient.ed25519_pub.as_slice(), recipient.x25519_pub.as_slice(), demand.as_ref().map(|id| id.as_bytes().as_slice())], |r| r.get(0))?)
        })
    }

    /// Called by the serialized producer only after actual upload and HAVE.
    /// Repeating a settled token performs no write transaction.
    pub(crate) fn settle_group_custody(
        &self,
        file: ConversationId,
        manifest: EventId,
        recipient: &PublicIdentity,
        demand: Option<EventId>,
    ) -> Result<bool, LogError> {
        if self.group_custody_settled(file, manifest, recipient, demand)? {
            return Ok(true);
        }
        if !self.has_manifest_event(file, manifest)? {
            return Ok(false);
        }
        self.profile.database().transaction(|c| {
            self.check(c)?;
            let ordinal: Option<i64> = c.prepare_cached("SELECT ordinal FROM file_book_offers WHERE account=?1 AND file=?2 AND event=?3 ORDER BY ordinal LIMIT 1")?
                .query_row(params![self.account.as_slice(), file.as_bytes().as_slice(), manifest.as_bytes().as_slice()], |r| r.get(0)).optional()?;
            let Some(ordinal) = ordinal else { return Ok(false); };
            c.prepare_cached("INSERT INTO file_book_custody_dispatch VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account,ordinal,recipient_ed,recipient_x) DO UPDATE SET demand=excluded.demand WHERE demand IS NOT excluded.demand")?
                .execute(params![self.account.as_slice(), ordinal, recipient.ed25519_pub.as_slice(), recipient.x25519_pub.as_slice(), demand.as_ref().map(|id| id.as_bytes().as_slice())])?;
            Ok(true)
        })
    }
}
