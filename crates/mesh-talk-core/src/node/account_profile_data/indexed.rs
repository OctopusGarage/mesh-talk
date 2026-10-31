//! Selected current signed values. Version admission stays with this profile
//! domain; the shared publisher only owns encrypted source retirement.
use super::{AccountProfile, MAGIC, MAX_ACCOUNTS, MAX_STORE_BYTES};
use crate::eventlog::LogError;
use crate::node::transcript_repository::{ACCOUNT_PROFILES, publication::PublishedHistorySource};
use crate::storage::{profile::context::ProfileStorage, record_log::EncryptedRecordLog};
use bincode::Options;
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};

const PAGE_ROWS: usize = 16;
const PAGE_BYTES: usize = 1024 * 1024;
const SCHEMA: &str = "
CREATE TABLE current_account_profile_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(revision)='integer' AND revision>=0),records INTEGER NOT NULL DEFAULT 0 CHECK(records>=0),wire_bytes INTEGER NOT NULL DEFAULT 0 CHECK(wire_bytes>=0));
CREATE TABLE current_account_profile_headers(account TEXT PRIMARY KEY CHECK(length(account)=32),version BLOB NOT NULL CHECK(length(version)=8),created BLOB CHECK(created IS NULL OR length(created)=8),encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes>=0),change_revision INTEGER NOT NULL CHECK(change_revision>0),projection_pending INTEGER NOT NULL CHECK(projection_pending IN (0,1)),name TEXT NOT NULL CHECK(length(CAST(name AS BLOB))<=256)) WITHOUT ROWID;
CREATE INDEX current_account_profile_pending ON current_account_profile_headers(projection_pending,account) WHERE projection_pending=1;
CREATE TABLE current_account_profile_bodies(account TEXT PRIMARY KEY REFERENCES current_account_profile_headers(account) ON DELETE CASCADE,body BLOB NOT NULL) WITHOUT ROWID;
";

struct CurrentHeader {
    version: Vec<u8>,
    created: Option<Vec<u8>>,
    size: i64,
    actual: Option<i64>,
    name: String,
}
pub(crate) struct IndexedAccountProfileStore {
    source: PublishedHistorySource,
}
pub(crate) struct AccountProfilePage {
    pub rows: Vec<AccountProfile>,
    pub after: Option<String>,
    pub complete: bool,
}
impl IndexedAccountProfileStore {
    pub(crate) fn authenticate_legacy(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        // Invalid signed records were historically ignored by admission. Frame
        // authentication must still succeed before publishing any source.
        PublishedHistorySource::authenticate_legacy::<AccountProfile>(
            profile,
            path,
            password,
            ACCOUNT_PROFILES,
            MAGIC,
        )
    }
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='current_account_profile_state')",[],|r|r.get(0))?;
            let registry: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",[],|r|r.get(0))?;
            let imported = registry && c.query_row("SELECT EXISTS(SELECT 1 FROM transcript_sources WHERE kind=?1)",[ACCOUNT_PROFILES],|r|r.get::<_,bool>(0))?;
            if !exists && !imported {
                c.execute_batch(SCHEMA)?;
                c.execute("INSERT INTO current_account_profile_state(singleton) VALUES(1)",[])?;
            }
            crate::storage::profile::manifest::validate_ddl(c,SCHEMA)?;
            state_on(c).map(|_|())
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            ACCOUNT_PROFILES,
            |c, path, password| {
                let log = EncryptedRecordLog::<AccountProfile>::open_with_visitor(
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
    fn read_on(c: &Connection, account: &str) -> Result<Option<AccountProfile>, LogError> {
        let header:Option<CurrentHeader>=c.query_row("SELECT h.version,h.created,h.encoded_bytes,length(b.body),h.name FROM current_account_profile_headers h LEFT JOIN current_account_profile_bodies b USING(account) WHERE h.account=?1",[account],|r|Ok(CurrentHeader {version:r.get(0)?,created:r.get(1)?,size:r.get(2)?,actual:r.get(3)?,name:r.get(4)?})).optional()?;
        let Some(CurrentHeader {
            version,
            created,
            size,
            actual,
            name,
        }) = header
        else {
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
            "SELECT body FROM current_account_profile_bodies WHERE account=?1",
            [account],
            |r| r.get(0),
        )?;
        let value: AccountProfile = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(size as u64)
            .reject_trailing_bytes()
            .deserialize(&body)
            .map_err(|e| LogError::Serialization(e.to_string()))?;
        if value.account_id() != account
            || version.as_slice() != value.revision.to_be_bytes()
            || created != value.created_at_ms.map(|n| n.to_be_bytes().to_vec())
            || name != value.display_name
            || !value.verify()
        {
            return Err(invalid(
                "current profile body disagrees with authenticated header",
            ));
        }
        Ok(Some(value))
    }
    fn accepts_on(c: &Connection, value: &AccountProfile) -> Result<bool, LogError> {
        if !value.verify() {
            return Ok(false);
        }
        let old = Self::read_on(c, &value.account_id())?;
        if let Some(old) = &old {
            if old.created_at_ms.is_some() && old.created_at_ms != value.created_at_ms {
                return Ok(false);
            }
            if super::profile_order(value) <= super::profile_order(old) {
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
    fn put_on(c: &Connection, value: &AccountProfile) -> Result<bool, LogError> {
        if !Self::accepts_on(c, value)? {
            return Ok(false);
        }
        let account = value.account_id();
        let change_revision = state_on(c)?
            .0
            .checked_add(1)
            .and_then(|n| i64::try_from(n).ok())
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let previous: Option<i64> = c
            .query_row(
                "SELECT encoded_bytes FROM current_account_profile_headers WHERE account=?1",
                [&account],
                |r| r.get(0),
            )
            .optional()?;
        let body = bincode::serialize(value).map_err(|e| LogError::Serialization(e.to_string()))?;
        let bytes = i64::try_from(body.len()).map_err(|_| LogError::RepositoryCounterExhausted)?;
        c.execute("INSERT INTO current_account_profile_headers VALUES(?1,?2,?3,?4,?5,1,?6) ON CONFLICT(account) DO UPDATE SET version=excluded.version,created=excluded.created,encoded_bytes=excluded.encoded_bytes,change_revision=excluded.change_revision,projection_pending=1,name=excluded.name",params![account,value.revision.to_be_bytes().as_slice(),value.created_at_ms.map(|n|n.to_be_bytes().to_vec()),bytes,change_revision,value.display_name])?;
        c.execute("INSERT INTO current_account_profile_bodies VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET body=excluded.body",params![account,body])?;
        // Wire encoding adds the existing four-byte version tag to the stored
        // bincode payload. Admission counts wire bytes exactly as the legacy owner.
        c.execute("UPDATE current_account_profile_state SET revision=revision+1,records=records+?1,wire_bytes=wire_bytes+?2 WHERE singleton=1",params![i64::from(previous.is_none()),bytes+4-previous.map_or(0,|n|n+4)])?;
        Ok(true)
    }
    /// Authenticated descriptive names without reading any avatar payload.
    pub(crate) fn names_page(
        &self,
        after: Option<&str>,
    ) -> Result<Vec<(String, String)>, LogError> {
        self.source.profile().database().read(|c| {
            let mut statement=c.prepare_cached("SELECT account,name FROM current_account_profile_headers WHERE (?1 IS NULL OR account>?1) ORDER BY account LIMIT 128")?;
            let mut rows=statement.query([after])?;let mut names=Vec::with_capacity(128);
            while let Some(row)=rows.next()? {names.push((row.get(0)?,row.get(1)?));}
            Ok(names)
        })
    }
    /// Current revision metadata; no avatar or signed body is fetched.
    pub(crate) fn version(&self, account: &str) -> Result<Option<u64>, LogError> {
        self.source.profile().database().read(|c| {
            let value: Option<Vec<u8>> = c
                .query_row(
                    "SELECT version FROM current_account_profile_headers WHERE account=?1",
                    [account],
                    |r| r.get(0),
                )
                .optional()?;
            value
                .map(|v| {
                    v.as_slice()
                        .try_into()
                        .map(u64::from_be_bytes)
                        .map_err(|_| invalid("invalid profile version"))
                })
                .transpose()
        })
    }
    /// One durable pending bit per current value, independent of later writes
    /// to other accounts. Exact change revisions prevent stale acknowledgements.
    pub(crate) fn projection_revision(&self, account: &str) -> Result<Option<u64>, LogError> {
        self.source.profile().database().read(|c| {
            let revision: Option<i64> = c.query_row("SELECT change_revision FROM current_account_profile_headers WHERE account=?1 AND projection_pending=1",[account],|r|r.get(0)).optional()?;
            revision.map(|n|u64::try_from(n).map_err(|_|invalid("invalid projection revision"))).transpose()
        })
    }
    pub(crate) fn pending_projection(&self) -> Result<Vec<(String, u64)>, LogError> {
        self.source.profile().database().read(|c| {
            let mut statement=c.prepare_cached("SELECT account,change_revision FROM current_account_profile_headers WHERE projection_pending=1 ORDER BY account LIMIT 16")?;
            let mut rows=statement.query([])?;let mut pending=Vec::with_capacity(16);
            while let Some(row)=rows.next()? {pending.push((row.get(0)?,u64::try_from(row.get::<_,i64>(1)?).map_err(|_|invalid("invalid projection revision"))?));}
            Ok(pending)
        })
    }
    pub(crate) fn acknowledge_projection(
        &self,
        account: &str,
        revision: u64,
    ) -> Result<(), LogError> {
        let revision =
            i64::try_from(revision).map_err(|_| invalid("invalid projection revision"))?;
        self.source.check_write()?;
        self.source.profile().database().transaction(|c| {
            c.execute("UPDATE current_account_profile_headers SET projection_pending=0 WHERE account=?1 AND change_revision=?2",params![account,revision])?;Ok(())
        })
    }
    pub(crate) fn get(&self, account: &str) -> Result<Option<AccountProfile>, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| Self::read_on(c, account))
    }
    #[cfg(test)]
    pub(crate) fn accepts(&self, value: &AccountProfile) -> Result<bool, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| Self::accepts_on(c, value))
    }
    pub(crate) fn record(&mut self, value: AccountProfile) -> Result<bool, LogError> {
        self.source.check_write()?;
        self.source
            .profile()
            .database()
            .transaction(|c| Self::put_on(c, &value))
    }
    #[cfg(test)]
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| state_on(c).map(|s| s.0))
    }
    pub(crate) fn page(&self, after: Option<&str>) -> Result<AccountProfilePage, LogError> {
        self.source.profile().database().read(|c| {
            let mut statement=c.prepare_cached("SELECT account,encoded_bytes FROM current_account_profile_headers WHERE (?1 IS NULL OR account>?1) ORDER BY account LIMIT 17")?;
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
            Ok(AccountProfilePage {rows:values,after:selected.last().cloned(),complete})
        })
    }
    #[cfg(test)]
    pub(crate) fn visit(
        &self,
        mut visit: impl FnMut(AccountProfile) -> Result<(), LogError>,
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
        "SELECT revision,records,wire_bytes FROM current_account_profile_state WHERE singleton=1",
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
