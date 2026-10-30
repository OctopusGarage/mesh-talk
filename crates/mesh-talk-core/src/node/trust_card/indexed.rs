//! Selected current signed values. Version admission stays with this profile
//! domain; the shared publisher only owns encrypted source retirement.
use super::{MAGIC, TrustCard};
use crate::eventlog::LogError;
use crate::node::transcript_repository::{TRUST_CARDS, publication::PublishedHistorySource};
use crate::storage::{profile::context::ProfileStorage, record_log::EncryptedRecordLog};
use bincode::Options;
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};

const PAGE_ROWS: usize = 16;
const PAGE_BYTES: usize = 1024 * 1024;
const MAX_ACCOUNTS: usize = 1024;
const MAX_STORE_BYTES: usize = 32 * 1024 * 1024;
const SCHEMA: &str = "
CREATE TABLE current_trust_card_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(revision)='integer' AND revision>=0),records INTEGER NOT NULL DEFAULT 0 CHECK(records>=0),wire_bytes INTEGER NOT NULL DEFAULT 0 CHECK(wire_bytes>=0));
CREATE TABLE current_trust_card_headers(account TEXT PRIMARY KEY CHECK(length(account)=32),version BLOB NOT NULL CHECK(length(version)=8),created BLOB CHECK(created IS NULL OR length(created)=8),encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes>=0)) WITHOUT ROWID;
CREATE TABLE current_trust_card_bodies(account TEXT PRIMARY KEY REFERENCES current_trust_card_headers(account) ON DELETE CASCADE,body BLOB NOT NULL) WITHOUT ROWID;
";

pub(crate) struct IndexedTrustCardStore {
    source: PublishedHistorySource,
}
pub(crate) struct TrustCardPage {
    pub rows: Vec<TrustCard>,
    pub after: Option<String>,
    pub complete: bool,
    pub revision: u64,
}
impl IndexedTrustCardStore {
    pub(crate) fn authenticate_legacy(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        // Invalid signed records were historically ignored by admission. Frame
        // authentication must still succeed before publishing any source.
        PublishedHistorySource::authenticate_legacy::<TrustCard>(
            profile,
            path,
            password,
            TRUST_CARDS,
            MAGIC,
        )
    }
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='current_trust_card_state')",
                [],
                |r| r.get(0),
            )?;
            let registry: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",
                [],
                |r| r.get(0),
            )?;
            let imported = registry
                && c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM transcript_sources WHERE kind=?1)",
                    [TRUST_CARDS],
                    |r| r.get::<_, bool>(0),
                )?;
            if !exists && !imported {
                c.execute_batch(SCHEMA)?;
                c.execute(
                    "INSERT INTO current_trust_card_state(singleton) VALUES(1)",
                    [],
                )?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
            state_on(c).map(|_| ())
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            TRUST_CARDS,
            |c, path, password| {
                let log = EncryptedRecordLog::<TrustCard>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |value| Self::put_on(c, &value).map(|_| ()),
                )?;
                log.sync()
            },
        )?;
        Ok(Self { source })
    }
    fn read_on(c: &Connection, account: &str) -> Result<Option<TrustCard>, LogError> {
        let header: Option<(Vec<u8>,Option<Vec<u8>>,i64,Option<i64>)> = c.query_row("SELECT h.version,h.created,h.encoded_bytes,length(b.body) FROM current_trust_card_headers h LEFT JOIN current_trust_card_bodies b USING(account) WHERE h.account=?1",[account],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((version, created, size, actual)) = header else {
            return Ok(None);
        };
        let size = usize::try_from(size).map_err(|_| invalid("invalid current profile size"))?;
        if actual.and_then(|n| usize::try_from(n).ok()) != Some(size) {
            return Err(invalid("current profile body length disagrees with header"));
        }
        if size > super::MAX_WIRE {
            return Err(LogError::PageBudgetTooSmall { required: size });
        }
        #[cfg(test)]
        BODY_READS.with(|n| n.set(n.get() + 1));
        let body: Vec<u8> = c.query_row(
            "SELECT body FROM current_trust_card_bodies WHERE account=?1",
            [account],
            |r| r.get(0),
        )?;
        let value: TrustCard = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(size as u64)
            .reject_trailing_bytes()
            .deserialize(&body)
            .map_err(|e| LogError::Serialization(e.to_string()))?;
        if value.account_id() != account
            || version.as_slice() != value.revision.to_be_bytes()
            || created != value.created_at_ms.map(|n| n.to_be_bytes().to_vec())
            || !value.verify()
        {
            return Err(invalid(
                "current profile body disagrees with authenticated header",
            ));
        }
        Ok(Some(value))
    }
    fn accepts_on(c: &Connection, value: &TrustCard) -> Result<bool, LogError> {
        if !value.verify() {
            return Ok(false);
        }
        let old = Self::read_on(c, &value.account_id())?;
        if let Some(old) = &old {
            if old.created_at_ms.is_some() && old.created_at_ms != value.created_at_ms {
                return Ok(false);
            }
            if old.revision >= value.revision {
                return Ok(false);
            }
        }
        let (_, count, bytes) = state_on(c)?;
        if old.is_none() && count >= MAX_ACCOUNTS {
            return Ok(false);
        }
        let old_bytes = old.as_ref().map_or(0, |value| value.encode().len());
        Ok(bytes
            .checked_sub(old_bytes)
            .ok_or_else(|| invalid("current profile byte count underflow"))?
            .saturating_add(value.encode().len())
            <= MAX_STORE_BYTES)
    }
    fn put_on(c: &Connection, value: &TrustCard) -> Result<bool, LogError> {
        if !Self::accepts_on(c, value)? {
            return Ok(false);
        }
        let account = value.account_id();
        let previous: Option<i64> = c
            .query_row(
                "SELECT encoded_bytes FROM current_trust_card_headers WHERE account=?1",
                [&account],
                |r| r.get(0),
            )
            .optional()?;
        let body = bincode::serialize(value).map_err(|e| LogError::Serialization(e.to_string()))?;
        let bytes = i64::try_from(body.len()).map_err(|_| LogError::RepositoryCounterExhausted)?;
        c.execute("INSERT INTO current_trust_card_headers VALUES(?1,?2,?3,?4) ON CONFLICT(account) DO UPDATE SET version=excluded.version,created=excluded.created,encoded_bytes=excluded.encoded_bytes",params![account,value.revision.to_be_bytes().as_slice(),value.created_at_ms.map(|n|n.to_be_bytes().to_vec()),bytes])?;
        c.execute("INSERT INTO current_trust_card_bodies VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET body=excluded.body",params![account,body])?;
        // Wire encoding adds the existing four-byte version tag to the stored
        // bincode payload. Admission counts wire bytes exactly as the legacy owner.
        c.execute("UPDATE current_trust_card_state SET revision=revision+1,records=records+?1,wire_bytes=wire_bytes+?2 WHERE singleton=1",params![i64::from(previous.is_none()),bytes+4-previous.map_or(0,|n|n+4)])?;
        Ok(true)
    }
    pub(crate) fn get(&self, account: &str) -> Result<Option<TrustCard>, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| Self::read_on(c, account))
    }
    pub(crate) fn accepts(&self, value: &TrustCard) -> Result<bool, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| Self::accepts_on(c, value))
    }
    pub(crate) fn record(&mut self, value: TrustCard) -> Result<bool, LogError> {
        self.source.check_write()?;
        self.source
            .profile()
            .database()
            .transaction(|c| Self::put_on(c, &value))
    }
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| state_on(c).map(|s| s.0))
    }
    pub(crate) fn page(&self, after: Option<&str>) -> Result<TrustCardPage, LogError> {
        self.source.profile().database().read(|c| {
            let revision=state_on(c)?.0;
            let mut statement=c.prepare_cached("SELECT account,encoded_bytes FROM current_trust_card_headers WHERE (?1 IS NULL OR account>?1) ORDER BY account LIMIT 17")?;
            let mut rows=statement.query([after])?;
            let mut selected=Vec::with_capacity(PAGE_ROWS);
            let mut charged=0usize;
            let mut complete=true;
            while let Some(row)=rows.next()? {
                let account=row.get_ref(0)?.as_str().map_err(|_|invalid("invalid current profile account"))?;
                let size=usize::try_from(row.get::<_,i64>(1)?).map_err(|_|invalid("invalid current profile size"))?.checked_add(account.len()).ok_or(LogError::RepositoryCounterExhausted)?;
                if selected.len()==PAGE_ROWS || charged.saturating_add(size)>PAGE_BYTES {
                    if selected.is_empty() { return Err(LogError::PageBudgetTooSmall {required:size}); }
                    complete=false; break;
                }
                charged+=size; selected.push(account.to_owned());
            }
            drop(rows); drop(statement);
            let mut values=Vec::with_capacity(selected.len());
            for account in &selected { values.push(Self::read_on(c,account)?.ok_or_else(||invalid("selected profile disappeared"))?); }
            Ok(TrustCardPage {rows:values,after:selected.last().cloned(),complete,revision})
        })
    }
    pub(crate) fn visit(
        &self,
        mut visit: impl FnMut(TrustCard) -> Result<(), LogError>,
    ) -> Result<(), LogError> {
        let mut after = None;
        loop {
            let page = self.page(after.as_deref())?;
            for value in page.rows {
                visit(value)?;
            }
            if page.complete {
                return Ok(());
            }
            after = page.after;
        }
    }
}
fn state_on(c: &Connection) -> Result<(u64, usize, usize), LogError> {
    let (revision, count, bytes): (i64, i64, i64) = c.query_row(
        "SELECT revision,records,wire_bytes FROM current_trust_card_state WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok((
        revision
            .try_into()
            .map_err(|_| invalid("invalid profile revision"))?,
        count
            .try_into()
            .map_err(|_| invalid("invalid profile count"))?,
        bytes
            .try_into()
            .map_err(|_| invalid("invalid profile bytes"))?,
    ))
}
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
#[cfg(test)]
thread_local! { static BODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(super) fn reset_body_reads() {
    BODY_READS.with(|n| n.set(0));
}
#[cfg(test)]
pub(super) fn body_reads() -> usize {
    BODY_READS.with(|n| n.get())
}
#[cfg(test)]
mod tests;
