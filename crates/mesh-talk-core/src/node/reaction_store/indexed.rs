//! Exact authored reactions on the participant connection. Account-scoped source
//! publication preserves the legacy path's lifetime across account adoption.
use super::{MAGIC, Record};
use crate::{
    eventlog::{ConversationId, EventId, LogError},
    node::{
        reaction::ReactionPayload,
        transcript_repository::{
            AUTHORED_REACTIONS, TranscriptPageLimit, publication::PublishedHistorySource,
        },
    },
    storage::{profile::context::ProfileStorage, record_log::EncryptedRecordLog},
};
use bincode::Options;
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};
const SCHEMA:&str="
CREATE TABLE authored_reaction_state(kind INTEGER NOT NULL DEFAULT 15 CHECK(kind=15),account BLOB PRIMARY KEY CHECK(length(account)=32),revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(revision)='integer' AND revision>=0),FOREIGN KEY(kind,account) REFERENCES account_history_sources(kind,account)) WITHOUT ROWID;
CREATE TABLE authored_reaction_keys(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,account BLOB NOT NULL REFERENCES authored_reaction_state(account),conversation BLOB NOT NULL CHECK(length(conversation)=32),target BLOB NOT NULL CHECK(length(target)=32),emoji TEXT NOT NULL,clock BLOB NOT NULL CHECK(length(clock)=8),removed INTEGER NOT NULL CHECK(removed IN(0,1)),body_bytes INTEGER NOT NULL CHECK(body_bytes>=0),UNIQUE(account,conversation,target,emoji));
CREATE INDEX authored_reaction_scan ON authored_reaction_keys(account,ordinal);
CREATE INDEX authored_reaction_conversation ON authored_reaction_keys(account,conversation,ordinal);
CREATE INDEX authored_reaction_target ON authored_reaction_keys(account,conversation,target,ordinal);
CREATE TABLE authored_reaction_bodies(ordinal INTEGER PRIMARY KEY REFERENCES authored_reaction_keys(ordinal) ON DELETE CASCADE,body BLOB NOT NULL);
";
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
pub(in crate::node) struct IndexedReactionStore {
    source: PublishedHistorySource,
    account: [u8; 32],
    #[cfg(test)]
    fail_writes: bool,
}
pub(in crate::node) struct ReactionPage {
    pub rows: Vec<Record>,
    pub next: Option<i64>,
}
impl IndexedReactionStore {
    pub(in crate::node) fn authenticate_legacy(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
        account: [u8; 32],
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy_for_account::<Record>(
            profile,
            path,
            password,
            AUTHORED_REACTIONS,
            account,
            MAGIC,
        )
    }
    pub(in crate::node) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
        account: [u8; 32],
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let present:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='authored_reaction_state')",[],|r|r.get(0))?;
            let registry:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='account_history_sources')",[],|r|r.get(0))?;
            let imported=registry && c.query_row("SELECT EXISTS(SELECT 1 FROM account_history_sources WHERE kind=15 AND account=?1)",[account.as_slice()],|r|r.get::<_,bool>(0))?;
            if !present && !imported {c.execute_batch(SCHEMA)?;}
            crate::storage::profile::manifest::validate_ddl(c,SCHEMA)?;
            let initialized:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM authored_reaction_state WHERE account=?1)",[account.as_slice()],|r|r.get(0))?;
            if initialized!=imported {return Err(invalid("authored reaction source authority is missing"));}
            Ok(())
        })?;
        let source = PublishedHistorySource::open_for_account(
            profile,
            path,
            password,
            AUTHORED_REACTIONS,
            account,
            |c, path, password| {
                c.execute(
                    "INSERT INTO authored_reaction_state(account) VALUES(?1)",
                    [account.as_slice()],
                )?;
                let log = EncryptedRecordLog::<Record>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |record| {
                        Self::put_on(c, &account, &record)?;
                        Ok(())
                    },
                )?;
                log.sync()
            },
        )?;
        Ok(Self {
            source,
            account,
            #[cfg(test)]
            fail_writes: false,
        })
    }
    fn check_account(&self, c: &Connection) -> Result<(), LogError> {
        let active: Vec<u8> = c.query_row(
            "SELECT account FROM participant_profile WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if active != self.account {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    fn check_write(&self) -> Result<(), LogError> {
        #[cfg(test)]
        if self.fail_writes {
            return Err(std::io::Error::other("injected reaction write failure").into());
        }
        self.source.check_write()
    }
    fn put_on(c: &Connection, account: &[u8; 32], record: &Record) -> Result<(), LogError> {
        let old:Option<(i64,Vec<u8>,bool)>=c.query_row("SELECT ordinal,clock,removed FROM authored_reaction_keys WHERE account=?1 AND conversation=?2 AND target=?3 AND emoji=?4",params![account.as_slice(),record.conversation.as_bytes().as_slice(),record.payload.target.as_bytes().as_slice(),record.payload.emoji],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((_, clock, removed)) = &old {
            let clock = u64::from_be_bytes(
                clock
                    .as_slice()
                    .try_into()
                    .map_err(|_| invalid("invalid reaction clock"))?,
            );
            if record.wall < clock || (record.wall == clock && (*removed || !record.payload.remove))
            {
                return Ok(());
            }
        }
        let bytes =
            bincode::serialize(record).map_err(|e| LogError::Serialization(e.to_string()))?;
        let size = i64::try_from(bytes.len()).map_err(|_| invalid("reaction body too large"))?;
        let ordinal = if let Some((ordinal, _, _)) = old {
            c.execute("UPDATE authored_reaction_keys SET clock=?2,removed=?3,body_bytes=?4 WHERE ordinal=?1",params![ordinal,record.wall.to_be_bytes().as_slice(),record.payload.remove,size])?;
            ordinal
        } else {
            c.execute("INSERT INTO authored_reaction_keys(account,conversation,target,emoji,clock,removed,body_bytes) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![account.as_slice(),record.conversation.as_bytes().as_slice(),record.payload.target.as_bytes().as_slice(),record.payload.emoji,record.wall.to_be_bytes().as_slice(),record.payload.remove,size])?;
            c.last_insert_rowid()
        };
        c.execute("INSERT INTO authored_reaction_bodies VALUES(?1,?2) ON CONFLICT(ordinal) DO UPDATE SET body=excluded.body",params![ordinal,bytes])?;
        c.execute(
            "UPDATE authored_reaction_state SET revision=revision+1 WHERE account=?1",
            [account.as_slice()],
        )?;
        Ok(())
    }
    pub(in crate::node) fn record(
        &mut self,
        conversation: ConversationId,
        wall: u64,
        payload: ReactionPayload,
    ) -> Result<(), LogError> {
        self.check_write()?;
        self.source.profile().database().transaction(|c| {
            self.check_account(c)?;
            Self::put_on(
                c,
                &self.account,
                &Record {
                    conversation,
                    wall,
                    payload,
                },
            )
        })
    }
    /// SQL admits fixed-size headers and their total charge before any body fetch.
    pub(in crate::node) fn page(
        &self,
        conversation: Option<ConversationId>,
        target: Option<EventId>,
        after: Option<i64>,
        rows: usize,
        bytes: usize,
    ) -> Result<ReactionPage, LogError> {
        let selected = self.page_with_ordinals(conversation, target, after, rows, bytes)?;
        Ok(ReactionPage {
            next: selected.last().map(|r| r.0),
            rows: selected.into_iter().map(|r| r.1).collect(),
        })
    }
    fn page_with_ordinals(
        &self,
        conversation: Option<ConversationId>,
        target: Option<EventId>,
        after: Option<i64>,
        limit: usize,
        budget: usize,
    ) -> Result<Vec<(i64, Record)>, LogError> {
        TranscriptPageLimit::new(limit, budget)?;
        if after.is_some_and(|n| n < 0) || (target.is_some() && conversation.is_none()) {
            return Err(LogError::InvalidPageBudget);
        }
        self.source.profile().database().read(|c| {
            self.check_account(c)?;
            let scope=match (conversation,target) {
                (Some(_),Some(_))=>"AND k.conversation=?2 AND k.target=?3",
                (Some(_),None)=>"AND k.conversation=?2 AND ?3 IS NULL",
                (None,None)=>"AND ?2 IS NULL AND ?3 IS NULL",
                (None,Some(_))=>return Err(LogError::InvalidPageBudget),
            };
            let sql=format!("SELECT k.ordinal,k.body_bytes,length(b.body) FROM authored_reaction_keys k LEFT JOIN authored_reaction_bodies b USING(ordinal) WHERE k.account=?1 {scope} AND k.ordinal>?4 ORDER BY k.ordinal LIMIT ?5");
            let mut statement=c.prepare(&sql)?;
            let mut rows=statement.query(params![self.account.as_slice(),conversation.as_ref().map(|v|v.as_bytes().as_slice()),target.as_ref().map(|v|v.as_bytes().as_slice()),after.unwrap_or(0),limit as i64])?;
            let mut headers=Vec::new();let mut charged=0usize;
            while let Some(row)=rows.next()? {
                let ordinal:i64=row.get(0)?;let encoded:i64=row.get(1)?;let actual:Option<i64>=row.get(2)?;
                let encoded=usize::try_from(encoded).map_err(|_|invalid("invalid reaction body size"))?;
                if actual.and_then(|n|usize::try_from(n).ok())!=Some(encoded) {return Err(invalid("reaction body length disagrees with header"));}
                let required=encoded.checked_mul(3).and_then(|n|n.checked_add(128)).ok_or(LogError::RepositoryCounterExhausted)?;
                if charged.checked_add(required).is_none_or(|n|n>budget) {if headers.is_empty(){return Err(LogError::PageBudgetTooSmall{required});}break;}
                charged+=required;headers.push((ordinal,encoded));
            }
            drop(rows);drop(statement);
            let mut out=Vec::with_capacity(headers.len());
            for (ordinal,size) in headers {
                let bytes:Vec<u8>=c.query_row("SELECT body FROM authored_reaction_bodies WHERE ordinal=?1",[ordinal],|r|r.get(0))?;
                #[cfg(test)] BODY_READS.with(|n|n.set(n.get()+1));
                let record:Record=bincode::DefaultOptions::new().with_fixint_encoding().with_limit(size as u64).reject_trailing_bytes().deserialize(&bytes).map_err(|_|invalid("invalid reaction body"))?;
                if conversation.is_some_and(|v|v!=record.conversation) || target.is_some_and(|v|v!=record.payload.target) {return Err(invalid("reaction selector disagrees with body"));}
                out.push((ordinal,record));
            }
            Ok(out)
        })
    }
    pub(in crate::node) fn remove_where(
        &mut self,
        remove: impl Fn(ConversationId, u64, &ReactionPayload) -> bool,
    ) -> Result<(), LogError> {
        self.try_remove_where(|conv, wall, payload| Ok(remove(conv, wall, payload)))
    }
    /// Predicates execute outside the connection mutex. Physical cleanup makes
    /// bounded transactional progress; an error leaves remaining rows retryable.
    pub(in crate::node) fn try_remove_where(
        &mut self,
        remove: impl Fn(ConversationId, u64, &ReactionPayload) -> Result<bool, LogError>,
    ) -> Result<(), LogError> {
        self.check_write()?;
        let mut after = None;
        loop {
            let page = self.page_with_ordinals(None, None, after, 128, 8 * 1024 * 1024)?;
            if page.is_empty() {
                return Ok(());
            }
            after = page.last().map(|r| r.0);
            let mut selected = Vec::new();
            for (ordinal, row) in page {
                if remove(row.conversation, row.wall, &row.payload)? {
                    selected.push(ordinal);
                }
            }
            self.source.profile().database().transaction(|c| {
                self.check_account(c)?;
                let mut removed = 0;
                for ordinal in selected {
                    removed += c.execute(
                        "DELETE FROM authored_reaction_keys WHERE account=?1 AND ordinal=?2",
                        params![self.account.as_slice(), ordinal],
                    )?;
                }
                if removed > 0 {
                    c.execute(
                        "UPDATE authored_reaction_state SET revision=revision+1 WHERE account=?1",
                        [self.account.as_slice()],
                    )?;
                }
                Ok(())
            })?;
        }
    }
    #[cfg(test)]
    pub(in crate::node) fn len(&self) -> usize {
        self.source
            .profile()
            .database()
            .read(|c| {
                let count: i64 = c.query_row(
                    "SELECT count(*) FROM authored_reaction_keys WHERE account=?1",
                    [self.account.as_slice()],
                    |r| r.get(0),
                )?;
                usize::try_from(count).map_err(|_| invalid("invalid authored reaction count"))
            })
            .unwrap()
    }
    #[cfg(test)]
    pub(in crate::node) fn fail_appends_for_test(&mut self) {
        self.fail_writes = true;
    }
}
#[cfg(test)]
thread_local! {static BODY_READS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
#[cfg(test)]
mod tests;
