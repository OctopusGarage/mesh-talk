//! Current avatar values on the participant connection. The legacy source is
//! independently authenticated and retired by the shared history publisher.
use super::{AvatarPage, MAGIC, ProfileEntry, ProfileStore};
use crate::eventlog::LogError;
use crate::node::profile::MAX_AVATAR_BYTES;
use crate::node::transcript_repository::{AVATAR_PROFILES, publication::PublishedHistorySource};
use crate::storage::{profile::context::ProfileStorage, record_log::EncryptedRecordLog};
use bincode::Options;
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};

const PAGE_ROWS: usize = 16;
const PAGE_BYTES: usize = 1024 * 1024;
const SCHEMA: &str = "
CREATE TABLE current_avatar_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(revision)='integer' AND revision>=0));
CREATE TABLE current_avatar_headers(account TEXT PRIMARY KEY,version BLOB NOT NULL CHECK(length(version)=8),encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes>=0)) WITHOUT ROWID;
CREATE TABLE current_avatar_bodies(account TEXT PRIMARY KEY REFERENCES current_avatar_headers(account) ON DELETE CASCADE,body BLOB NOT NULL) WITHOUT ROWID;
";

pub(crate) struct IndexedProfileStore {
    source: PublishedHistorySource,
    #[cfg(test)]
    fail_writes: bool,
}

impl IndexedProfileStore {
    pub(crate) fn authenticate_legacy(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy::<ProfileEntry>(
            profile,
            path,
            password,
            AVATAR_PROFILES,
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
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='current_avatar_state')",
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
                    [AVATAR_PROFILES],
                    |r| r.get::<_, bool>(0),
                )?;
            if !exists && !imported {
                c.execute_batch(SCHEMA)?;
                c.execute("INSERT INTO current_avatar_state(singleton) VALUES(1)", [])?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
            revision_on(c).map(|_| ())
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            AVATAR_PROFILES,
            |c, path, password| {
                let log = EncryptedRecordLog::<ProfileEntry>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |entry| {
                        Self::put_on(c, &entry)?;
                        Ok(())
                    },
                )?;
                log.sync()
            },
        )?;
        Ok(Self {
            source,
            #[cfg(test)]
            fail_writes: false,
        })
    }
    fn read_on(c: &Connection, account: &str) -> Result<Option<ProfileEntry>, LogError> {
        let header: Option<(Vec<u8>,i64,Option<i64>)> = c.query_row("SELECT h.version,h.encoded_bytes,length(b.body) FROM current_avatar_headers h LEFT JOIN current_avatar_bodies b USING(account) WHERE h.account=?1",[account],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((version, bytes, actual)) = header else {
            return Ok(None);
        };
        let bytes = usize::try_from(bytes).map_err(|_| invalid("invalid avatar body length"))?;
        if actual.and_then(|n| usize::try_from(n).ok()) != Some(bytes) {
            return Err(invalid("avatar body length disagrees with header"));
        }
        if bytes > 8 * 1024 * 1024 {
            return Err(LogError::PageBudgetTooSmall { required: bytes });
        }
        #[cfg(test)]
        BODY_READS.with(|n| n.set(n.get() + 1));
        let body: Vec<u8> = c.query_row(
            "SELECT body FROM current_avatar_bodies WHERE account=?1",
            [account],
            |r| r.get(0),
        )?;
        let entry: ProfileEntry = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes as u64)
            .reject_trailing_bytes()
            .deserialize(&body)
            .map_err(|e| LogError::Serialization(e.to_string()))?;
        if entry.account_id != account || version.as_slice() != entry.updated_at.to_be_bytes() {
            return Err(invalid("avatar body disagrees with header"));
        }
        Ok(Some(entry))
    }
    fn put_on(c: &Connection, entry: &ProfileEntry) -> Result<bool, LogError> {
        if Self::read_on(c, &entry.account_id)?.is_some_and(|old| {
            ProfileStore::order(old.updated_at, &old.avatar)
                >= ProfileStore::order(entry.updated_at, &entry.avatar)
        }) {
            return Ok(false);
        }
        let body = bincode::serialize(entry).map_err(|e| LogError::Serialization(e.to_string()))?;
        let bytes = i64::try_from(body.len()).map_err(|_| LogError::RepositoryCounterExhausted)?;
        c.execute("INSERT INTO current_avatar_headers VALUES(?1,?2,?3) ON CONFLICT(account) DO UPDATE SET version=excluded.version,encoded_bytes=excluded.encoded_bytes",params![entry.account_id,entry.updated_at.to_be_bytes().as_slice(),bytes])?;
        c.execute("INSERT INTO current_avatar_bodies VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET body=excluded.body",params![entry.account_id,body])?;
        c.execute(
            "UPDATE current_avatar_state SET revision=revision+1 WHERE singleton=1",
            [],
        )?;
        Ok(true)
    }
    pub(crate) fn record(
        &mut self,
        account: &str,
        updated_at: u64,
        avatar: Option<Vec<u8>>,
    ) -> Result<bool, LogError> {
        if avatar
            .as_ref()
            .is_some_and(|avatar| avatar.len() > MAX_AVATAR_BYTES)
        {
            return Ok(false);
        }
        self.source.check_write()?;
        #[cfg(test)]
        if self.fail_writes {
            return Err(LogError::Io(std::io::Error::other(
                "injected avatar write failure",
            )));
        }
        self.source.profile().database().transaction(|c| {
            Self::put_on(
                c,
                &ProfileEntry {
                    account_id: account.to_owned(),
                    updated_at,
                    avatar,
                },
            )
        })
    }
    pub(crate) fn avatar(&self, account: &str) -> Result<Option<Vec<u8>>, LogError> {
        let value = self
            .source
            .profile()
            .database()
            .read(|c| Self::read_on(c, account))?;
        Ok(value.and_then(|entry| entry.avatar))
    }
    pub(crate) fn updated_at(&self, account: &str) -> Result<Option<u64>, LogError> {
        self.source.profile().database().read(|c| {
            let value: Option<Vec<u8>> = c
                .query_row(
                    "SELECT version FROM current_avatar_headers WHERE account=?1",
                    [account],
                    |r| r.get(0),
                )
                .optional()?;
            value
                .map(|v| {
                    v.as_slice()
                        .try_into()
                        .map(u64::from_be_bytes)
                        .map_err(|_| invalid("invalid avatar version"))
                })
                .transpose()
        })
    }
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.source.profile().database().read(revision_on)
    }
    fn entries_page(
        &self,
        after: Option<&str>,
    ) -> Result<(Vec<ProfileEntry>, bool, u64), LogError> {
        self.source.profile().database().read(|c| {
            let revision = revision_on(c)?;
            let mut statement = c.prepare_cached("SELECT account,encoded_bytes FROM current_avatar_headers WHERE (?1 IS NULL OR account>?1) ORDER BY account LIMIT 17")?;
            let mut rows = statement.query([after])?;
            let mut selected = Vec::with_capacity(PAGE_ROWS);
            let mut charged = 0usize;
            let mut complete = true;
            while let Some(row) = rows.next()? {
                let account = row.get_ref(0)?.as_str().map_err(|_|invalid("invalid avatar account"))?;
                let size = usize::try_from(row.get::<_,i64>(1)?).map_err(|_|invalid("invalid avatar length"))?.checked_add(account.len()).ok_or(LogError::RepositoryCounterExhausted)?;
                if selected.len() == PAGE_ROWS || charged.saturating_add(size)>PAGE_BYTES {
                    if selected.is_empty() { return Err(LogError::PageBudgetTooSmall{required:size}); }
                    complete = false; break;
                }
                charged += size;
                selected.push(account.to_owned());
            }
            drop(rows); drop(statement);
            let mut values = Vec::with_capacity(selected.len());
            for account in &selected {
                let entry = Self::read_on(c,account)?.ok_or_else(||invalid("selected avatar disappeared"))?;
                values.push(entry);
            }
            Ok((values,complete,revision))
        })
    }
    pub(crate) fn page(&self, after: Option<&str>) -> Result<AvatarPage, LogError> {
        let (entries, complete, revision) = self.entries_page(after)?;
        let after = entries.last().map(|entry| entry.account_id.clone());
        let rows = entries
            .into_iter()
            .map(|entry| (entry.account_id, entry.avatar))
            .collect();
        Ok(AvatarPage {
            rows,
            after,
            complete,
            revision,
        })
    }
    pub(crate) fn visit(
        &self,
        mut visit: impl FnMut(String, u64, Option<Vec<u8>>) -> Result<(), LogError>,
    ) -> Result<(), LogError> {
        let mut after = None;
        loop {
            let (entries, complete, _) = self.entries_page(after.as_deref())?;
            after = entries.last().map(|entry| entry.account_id.clone());
            for entry in entries {
                visit(entry.account_id, entry.updated_at, entry.avatar)?;
            }
            if complete {
                return Ok(());
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn fail_appends_for_test(&mut self, enabled: bool) {
        self.fail_writes = enabled;
    }
}
fn revision_on(c: &Connection) -> Result<u64, LogError> {
    let revision: i64 = c.query_row(
        "SELECT revision FROM current_avatar_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    u64::try_from(revision).map_err(|_| invalid("invalid avatar revision"))
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
