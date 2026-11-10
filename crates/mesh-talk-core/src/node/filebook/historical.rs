//! Local historical eligibility is distinct from the active account's offers.
//! These are current validated facts; deleted offers leave no change journal.
use super::indexed::{IndexedFileBook, blob, invalid};
use crate::eventlog::LogError;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "
CREATE TABLE file_book_history_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),deleted_revision INTEGER NOT NULL CHECK(typeof(deleted_revision)='integer' AND deleted_revision>=0));
CREATE TABLE file_book_offer_revisions(account BLOB NOT NULL,ordinal INTEGER NOT NULL,revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),PRIMARY KEY(account,ordinal),FOREIGN KEY(account,ordinal) REFERENCES file_book_offers(account,ordinal) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX file_book_retained_ordinals ON file_book_offer_revisions(ordinal);
CREATE INDEX file_book_history_appends ON file_book_offer_revisions(revision,ordinal,account);
";
const TRIGGERS: &[(&str, &str)] = &[
    (
        "file_book_history_added",
        "CREATE TRIGGER file_book_history_added AFTER INSERT ON file_book_offers BEGIN UPDATE file_book_history_state SET revision=revision+1 WHERE singleton=1; INSERT INTO file_book_offer_revisions SELECT NEW.account,NEW.ordinal,revision FROM file_book_history_state WHERE singleton=1; END",
    ),
    (
        "file_book_history_removed",
        "CREATE TRIGGER file_book_history_removed AFTER DELETE ON file_book_offers BEGIN UPDATE file_book_history_state SET revision=revision+1,deleted_revision=revision+1 WHERE singleton=1; END",
    ),
];

pub(super) fn initialize_on(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_book_history_state')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
        c.execute("INSERT INTO file_book_history_state VALUES(1,0,0)", [])?;
        // One-time metadata upgrade. No transcript/proof body is materialized.
        // Revision zero is the initial snapshot, never a fabricated arrival.
        c.execute(
            "INSERT INTO file_book_offer_revisions SELECT account,ordinal,0 FROM file_book_offers",
            [],
        )?;
        for (_, sql) in TRIGGERS {
            c.execute_batch(sql)?;
        }
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    for (name, expected) in TRIGGERS {
        let actual: Option<String> = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref() != Some(*expected) {
            return Err(invalid(
                "incompatible historical file eligibility projection",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistoricalFileOffer {
    pub account: [u8; 32],
    pub ordinal: i64,
    pub revision: u64,
}

impl IndexedFileBook {
    /// An already validated retained card may be shown in an explicit old host
    /// after account adoption. This does not authorize a current transfer route.
    pub(crate) fn historical_eligible_ordinal(&self, ordinal: i64) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM file_book_offer_revisions WHERE ordinal=?1)",
                [ordinal],
                |r| r.get(0),
            )?)
        })
    }
    pub(crate) fn historical_revision(&self) -> Result<u64, LogError> {
        self.historical_revisions().map(|(revision, _)| revision)
    }
    /// The second value changes only when previously eligible material retires.
    pub(crate) fn historical_revisions(&self) -> Result<(u64, u64), LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            let (revision, deleted): (i64, i64) = c.query_row(
                "SELECT revision,deleted_revision FROM file_book_history_state WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((
                u64::try_from(revision).map_err(|_| invalid("invalid historical file revision"))?,
                u64::try_from(deleted)
                    .map_err(|_| invalid("invalid historical file deletion revision"))?,
            ))
        })
    }
    /// At most 128 fixed-size facts, ordered by accepted revision. Existing
    /// baseline rows have revision zero; callers initially build their snapshot.
    pub(crate) fn historical_offers_after(
        &self,
        after: u64,
        rows: usize,
    ) -> Result<Vec<HistoricalFileOffer>, LogError> {
        if rows == 0 || rows > 128 {
            return Err(LogError::InvalidPageBudget);
        }
        let after = i64::try_from(after).map_err(|_| LogError::InvalidPageBudget)?;
        self.profile.database().read(|c|{
            self.check(c)?;
            let mut statement=c.prepare("SELECT account,ordinal,revision FROM file_book_offer_revisions WHERE revision>?1 ORDER BY revision,ordinal,account LIMIT ?2")?;
            let mut found=statement.query(params![after,rows as i64])?;
            let mut page=Vec::new();
            while let Some(row)=found.next()? {
                let revision:i64=row.get(2)?;
                page.push(HistoricalFileOffer{account:blob(row,0)?,ordinal:row.get(1)?,revision:u64::try_from(revision).map_err(|_|invalid("invalid historical file offer revision"))?});
            }
            Ok(page)
        })
    }
}
