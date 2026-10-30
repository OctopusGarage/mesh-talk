//! Durable sent fanout references. A scope is usable only while the existing
//! validated FILES owner retains a binding; parent authorization remains in sync.
use super::indexed::{active, blob, invalid};
use crate::{
    eventlog::{Author, ConversationId, EventId, LogError},
    node::{
        privacy_runtime::{ManifestScope, StoredManifestScope},
        transcript_repository::{SENT_MANIFEST_SCOPES, publication::PublishedHistorySource},
    },
    storage::{
        profile::{context::ProfileStorage, manifest},
        record_log::EncryptedRecordLog,
    },
};
use rusqlite::{Connection, params};
use std::{path::Path, sync::Arc};

const SCHEMA: &str = "
CREATE TABLE sent_file_scope_revision(account BLOB PRIMARY KEY CHECK(length(account)=32),revision INTEGER NOT NULL CHECK(revision>=0)) WITHOUT ROWID;
CREATE TABLE sent_file_scopes(account BLOB NOT NULL CHECK(length(account)=32),file BLOB NOT NULL CHECK(length(file)=32),event BLOB NOT NULL CHECK(length(event)=32),parent BLOB NOT NULL CHECK(length(parent)=32),author BLOB NOT NULL CHECK(length(author)=32),PRIMARY KEY(account,file,event)) WITHOUT ROWID;
";
#[derive(Clone)]
pub(crate) struct IndexedManifestScopes {
    source: Arc<PublishedHistorySource>,
    account: [u8; 32],
    device: Author,
}
impl IndexedManifestScopes {
    pub(crate) fn authenticate(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy::<StoredManifestScope>(
            profile,
            path,
            password,
            SENT_MANIFEST_SCOPES,
            b"MTFSC1",
        )
    }
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
        events: &crate::eventlog::repository::EventRepository,
        device: Author,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='sent_file_scopes')",
                [],
                |r| r.get(0),
            )?;
            let published: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM transcript_sources WHERE kind=?1)",
                [SENT_MANIFEST_SCOPES],
                |r| r.get(0),
            )?;
            if !present && !published {
                c.execute_batch(SCHEMA)?;
            }
            manifest::validate_ddl(c, SCHEMA)
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            SENT_MANIFEST_SCOPES,
            |c, snapshot, password| {
                EncryptedRecordLog::<StoredManifestScope>::open_with_visitor(
                    snapshot,
                    password,
                    b"MTFSC1",
                    |scope| {
                        let header = match events.header_on(c, &scope.event) {
                            Ok(header) => header,
                            Err(LogError::ConversationRetiring) => None,
                            Err(error) => return Err(error),
                        };
                        if scope.author == device
                            && header.is_some_and(|header| {
                                header.kind == crate::eventlog::EventKind::FileManifest
                                    && header.conversation == scope.parent
                                    && header.author == scope.author
                            })
                        {
                            Self::put_on(c, &scope)?;
                        }
                        Ok(())
                    },
                )?;
                Ok(())
            },
        )?;
        let account = source.profile().database().read(active)?;
        Ok(Self {
            source: Arc::new(source),
            account,
            device,
        })
    }
    pub(crate) fn recapture(&mut self) -> Result<(), LogError> {
        self.account = self.source.profile().database().read(active)?;
        Ok(())
    }
    fn check(&self, c: &Connection) -> Result<(), LogError> {
        if active(c)? != self.account {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub(in crate::node) fn put_on(
        c: &Connection,
        scope: &StoredManifestScope,
    ) -> Result<(), LogError> {
        if scope.file == scope.parent {
            return Ok(());
        }
        let account = active(c)?;
        let changed = c.execute(
            "INSERT OR IGNORE INTO sent_file_scopes VALUES(?1,?2,?3,?4,?5)",
            params![
                account.as_slice(),
                scope.file.as_bytes().as_slice(),
                scope.event.as_bytes().as_slice(),
                scope.parent.as_bytes().as_slice(),
                scope.author.ed25519_pub().as_slice()
            ],
        )?;
        if changed != 0 {
            c.execute("INSERT INTO sent_file_scope_revision VALUES(?1,1) ON CONFLICT(account) DO UPDATE SET revision=revision+1",[account.as_slice()])?;
        }
        Ok(())
    }
    pub(crate) fn insert(
        &self,
        file: ConversationId,
        scope: ManifestScope,
    ) -> Result<(), LogError> {
        self.source.check_write()?;
        self.source.profile().database().transaction(|c| {
            self.check(c)?;
            Self::put_on(
                c,
                &StoredManifestScope {
                    file,
                    parent: scope.parent,
                    event: scope.event,
                    author: scope.author,
                },
            )
        })
    }
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.source.profile().database().read(|c| {
            self.check(c)?;
            let value:i64=c.query_row("SELECT coalesce((SELECT revision FROM sent_file_scope_revision WHERE account=?1),0)",[self.account.as_slice()],|r|r.get(0))?;
            u64::try_from(value).map_err(|_|invalid("invalid file scope revision"))
        })
    }
    pub(crate) fn page(
        &self,
        file: ConversationId,
        after: Option<EventId>,
    ) -> Result<Vec<ManifestScope>, LogError> {
        self.source.profile().database().read(|c| {
            self.check(c)?;
            let mut statement=c.prepare_cached("SELECT s.event,s.parent,s.author FROM sent_file_scopes s WHERE s.account=?1 AND s.file=?2 AND s.event>coalesce(?3,x'') AND s.author=?4 AND EXISTS(SELECT 1 FROM file_book_offers b WHERE b.account=s.account AND b.file=s.file AND b.author=?4) ORDER BY s.event LIMIT 128")?;
            let mut rows=statement.query(params![self.account.as_slice(),file.as_bytes().as_slice(),after.as_ref().map(|id|id.as_bytes().as_slice()),self.device.ed25519_pub().as_slice()])?;
            let mut result=Vec::new();
            while let Some(row)=rows.next()? { result.push(ManifestScope {event:EventId::new(blob(row,0)?),parent:ConversationId::new(blob(row,1)?),author:Author::from_ed25519(blob(row,2)?)}); }
            Ok(result)
        })
    }
    pub(crate) fn scopes(
        &self,
        file: ConversationId,
    ) -> impl Iterator<Item = Result<ManifestScope, LogError>> + use<> {
        let source = self.clone();
        let mut after = None;
        let mut pending = std::collections::VecDeque::new();
        let mut done = false;
        std::iter::from_fn(move || {
            if pending.is_empty() && !done {
                match source.page(file, after) {
                    Ok(page) => {
                        done = page.len() < 128;
                        after = page.last().map(|scope| scope.event).or(after);
                        pending.extend(page);
                    }
                    Err(error) => {
                        done = true;
                        return Some(Err(error));
                    }
                }
            }
            pending.pop_front().map(Ok)
        })
    }
}
