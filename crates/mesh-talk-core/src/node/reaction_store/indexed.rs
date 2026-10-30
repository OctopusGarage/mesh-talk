//! Exact authored reactions on the participant connection. Account-scoped source
//! publication preserves the legacy path's lifetime across account adoption.
#[cfg(test)]
use super::LEGACY_MAGIC as MAGIC;
use super::Record;
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
        if legacy_magic(path)? == super::MAGIC {
            PublishedHistorySource::authenticate_legacy_for_account::<super::StoreRecord>(
                profile,
                path,
                password,
                AUTHORED_REACTIONS,
                account,
                super::MAGIC,
            )
        } else {
            PublishedHistorySource::authenticate_legacy_for_account::<Record>(
                profile,
                path,
                password,
                AUTHORED_REACTIONS,
                account,
                super::LEGACY_MAGIC,
            )
        }
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
            let channels:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='private_channel_reactions')",[],|r|r.get(0))?;
            if !channels { c.execute_batch(CHANNEL_SCHEMA)?; }
            crate::storage::profile::manifest::validate_ddl(c,CHANNEL_SCHEMA)?;
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
                if legacy_magic(path)? == super::MAGIC {
                    let log = EncryptedRecordLog::<super::StoreRecord>::open_with_visitor(
                        path,
                        password,
                        super::MAGIC,
                        |record| Self::import_record(c, &account, record),
                    )?;
                    log.sync()
                } else {
                    let log = EncryptedRecordLog::<Record>::open_with_visitor(
                        path,
                        password,
                        super::LEGACY_MAGIC,
                        |record| Self::put_on(c, &account, &record),
                    )?;
                    log.sync()
                }
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

const CHANNEL_SCHEMA: &str = "
CREATE TABLE private_channel_reactions(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,account BLOB NOT NULL REFERENCES authored_reaction_state(account),conversation BLOB NOT NULL CHECK(length(conversation)=32),target BLOB NOT NULL CHECK(length(target)=32),emoji TEXT NOT NULL,source BLOB NOT NULL CHECK(length(source)=32),binding BLOB NOT NULL CHECK(length(binding) IN(0,32)),effect BLOB NOT NULL CHECK(length(effect)=32),clock BLOB NOT NULL CHECK(length(clock)=8),removed INTEGER NOT NULL CHECK(removed IN(0,1)),body_bytes INTEGER NOT NULL CHECK(body_bytes>=0),body BLOB NOT NULL,UNIQUE(account,conversation,target,emoji,source,binding));
CREATE INDEX private_channel_reaction_scan ON private_channel_reactions(account,conversation,ordinal);
CREATE INDEX private_channel_reaction_effect ON private_channel_reactions(account,effect);
";
pub(in crate::node) struct ChannelReactionPage {
    pub rows: Vec<super::RetainedChannelReaction>,
    pub next: Option<i64>,
}
fn legacy_magic(path: &Path) -> Result<&'static [u8; 6], LogError> {
    use std::io::Read;
    match std::fs::File::open(path) {
        Ok(mut file) => {
            let mut magic = [0; 6];
            file.read_exact(&mut magic)?;
            Ok(if &magic == super::MAGIC {
                super::MAGIC
            } else {
                super::LEGACY_MAGIC
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(super::LEGACY_MAGIC),
        Err(error) => Err(error.into()),
    }
}
fn decode_channel(bytes: &[u8]) -> Result<super::ChannelRecord, LogError> {
    let record: super::StoreRecord = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|_| invalid("invalid channel reaction body"))?;
    match record {
        super::StoreRecord::Channel(record) => Ok(record),
        super::StoreRecord::ChannelBound {
            mut record,
            source_certificate,
        } => {
            if !source_certificate.verify()
                || source_certificate.device_ed25519_pub != record.source.ed25519_pub
            {
                return Err(invalid("invalid bound channel reaction certificate"));
            }
            record.certificate = Some(source_certificate);
            Ok(record)
        }
        super::StoreRecord::OwnDm(_) => Err(invalid("unexpected direct reaction body")),
    }
}
impl IndexedReactionStore {
    fn import_record(
        c: &Connection,
        account: &[u8; 32],
        record: super::StoreRecord,
    ) -> Result<(), LogError> {
        match record {
            super::StoreRecord::OwnDm(record) => Self::put_on(c, account, &record),
            record => {
                let bytes = bincode::serialize(&record)
                    .map_err(|error| LogError::Serialization(error.to_string()))?;
                Self::put_channel_on(c, account, &decode_channel(&bytes)?)
            }
        }
    }
    fn put_channel_on(
        c: &Connection,
        account: &[u8; 32],
        record: &super::ChannelRecord,
    ) -> Result<(), LogError> {
        if record.effect_id == EventId::new([0; 32]) {
            return Err(invalid("invalid channel reaction effect"));
        }
        if record.certificate.as_ref().is_some_and(|certificate| {
            !certificate.verify() || certificate.device_ed25519_pub != record.source.ed25519_pub
        }) {
            return Err(invalid("invalid bound channel reaction certificate"));
        }
        let mut statement =
            c.prepare("SELECT body FROM private_channel_reactions WHERE account=?1 AND effect=?2")?;
        let mut rows = statement.query(params![
            account.as_slice(),
            record.effect_id.as_bytes().as_slice()
        ])?;
        while let Some(row) = rows.next()? {
            let bytes: Vec<u8> = row.get(0)?;
            let old = decode_channel(&bytes)?;
            if old.conversation != record.conversation
                || old.source != record.source
                || old.wall != record.wall
                || old.payload != record.payload
                || old
                    .certificate
                    .as_ref()
                    .zip(record.certificate.as_ref())
                    .is_some_and(|(a, b)| a.account_ed25519_pub != b.account_ed25519_pub)
            {
                return Err(invalid("conflicting channel reaction provenance"));
            }
        }
        drop(rows);
        drop(statement);
        let binding = record
            .certificate
            .as_ref()
            .map(|certificate| certificate.account_ed25519_pub.to_vec())
            .unwrap_or_default();
        let old: Option<(i64, Vec<u8>, bool, i64)> = c.query_row("SELECT ordinal,clock,removed,body_bytes FROM private_channel_reactions WHERE account=?1 AND conversation=?2 AND target=?3 AND emoji=?4 AND source=?5 AND binding=?6", params![account.as_slice(),record.conversation.as_bytes().as_slice(),record.payload.target.as_bytes().as_slice(),record.payload.emoji,record.source.ed25519_pub.as_slice(),binding], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((_, clock, removed, _)) = &old {
            let wall = u64::from_be_bytes(
                clock
                    .as_slice()
                    .try_into()
                    .map_err(|_| invalid("invalid channel reaction clock"))?,
            );
            if record.wall < wall || record.wall == wall && (*removed || !record.payload.remove) {
                return Ok(());
            }
        }
        let body = bincode::serialize(&super::channel_record(record.clone()))
            .map_err(|error| LogError::Serialization(error.to_string()))?;
        let size =
            i64::try_from(body.len()).map_err(|_| invalid("channel reaction body too large"))?;
        let (count, total): (i64, i64) = c.query_row("SELECT count(*),COALESCE(sum(body_bytes+32),0) FROM private_channel_reactions WHERE account=?1",[account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let next = total - old.as_ref().map_or(0, |row| row.3 + 32) + size + 32;
        if next > super::MAX_CHANNEL_BYTES as i64
            || old.is_none() && count >= super::MAX_CHANNEL_ENTRIES as i64
        {
            return Err(invalid("channel reaction projection capacity exhausted"));
        }
        c.execute("INSERT INTO private_channel_reactions(account,conversation,target,emoji,source,binding,effect,clock,removed,body_bytes,body) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(account,conversation,target,emoji,source,binding) DO UPDATE SET effect=excluded.effect,clock=excluded.clock,removed=excluded.removed,body_bytes=excluded.body_bytes,body=excluded.body",params![account.as_slice(),record.conversation.as_bytes().as_slice(),record.payload.target.as_bytes().as_slice(),record.payload.emoji,record.source.ed25519_pub.as_slice(),binding,record.effect_id.as_bytes().as_slice(),record.wall.to_be_bytes().as_slice(),record.payload.remove,size,body])?;
        c.execute(
            "UPDATE authored_reaction_state SET revision=revision+1 WHERE account=?1",
            [account.as_slice()],
        )?;
        Ok(())
    }
    pub(in crate::node) fn record_channel(
        &mut self,
        conversation: ConversationId,
        source: crate::identity::device::PublicIdentity,
        effect_id: EventId,
        wall: u64,
        payload: ReactionPayload,
    ) -> Result<(), LogError> {
        self.record_channel_with_certificate(conversation, source, effect_id, wall, payload, None)
    }
    pub(in crate::node) fn record_channel_bound(
        &mut self,
        conversation: ConversationId,
        source: crate::identity::device::PublicIdentity,
        effect_id: EventId,
        wall: u64,
        payload: ReactionPayload,
        certificate: crate::identity::account::DeviceCertificate,
    ) -> Result<(), LogError> {
        self.record_channel_with_certificate(
            conversation,
            source,
            effect_id,
            wall,
            payload,
            Some(certificate),
        )
    }
    fn record_channel_with_certificate(
        &mut self,
        conversation: ConversationId,
        source: crate::identity::device::PublicIdentity,
        effect_id: EventId,
        wall: u64,
        payload: ReactionPayload,
        certificate: Option<crate::identity::account::DeviceCertificate>,
    ) -> Result<(), LogError> {
        self.check_write()?;
        self.source.profile().database().transaction(|c| {
            self.check_account(c)?;
            Self::put_channel_on(
                c,
                &self.account,
                &super::ChannelRecord {
                    conversation,
                    source,
                    effect_id,
                    wall,
                    payload,
                    certificate,
                },
            )
        })
    }
    pub(in crate::node) fn channel_page(
        &self,
        conversation: ConversationId,
        after: Option<i64>,
        limit: usize,
        budget: usize,
    ) -> Result<ChannelReactionPage, LogError> {
        TranscriptPageLimit::new(limit, budget)?;
        if after.is_some_and(|id| id < 0) {
            return Err(LogError::InvalidPageBudget);
        }
        self.source.profile().database().read(|c| {
            self.check_account(c)?;
            let mut statement = c.prepare("SELECT ordinal,body_bytes,length(body) FROM private_channel_reactions WHERE account=?1 AND conversation=?2 AND ordinal>?3 ORDER BY ordinal LIMIT ?4")?;
            let mut rows=statement.query(params![self.account.as_slice(),conversation.as_bytes().as_slice(),after.unwrap_or(0),limit as i64])?;
            let mut selected=Vec::new(); let mut charge=0usize;
            while let Some(row)=rows.next()? {
                let ordinal:i64=row.get(0)?; let encoded:i64=row.get(1)?; let actual:i64=row.get(2)?;
                if encoded<0 || encoded!=actual { return Err(invalid("invalid channel reaction length")); }
                let required=(encoded as usize).checked_mul(3).and_then(|n|n.checked_add(256)).ok_or(LogError::RepositoryCounterExhausted)?;
                if charge.checked_add(required).is_none_or(|n|n>budget) { if selected.is_empty(){return Err(LogError::PageBudgetTooSmall{required});} break; }
                charge+=required; selected.push(ordinal);
            }
            drop(rows); drop(statement);
            let next=selected.last().copied(); let mut out=Vec::new();
            for ordinal in selected {
                let bytes:Vec<u8>=c.query_row("SELECT body FROM private_channel_reactions WHERE ordinal=?1",[ordinal],|r|r.get(0))?;
                let record=decode_channel(&bytes)?;
                if record.conversation!=conversation { return Err(invalid("channel reaction selector disagrees with body")); }
                out.push((record.source,record.effect_id,record.wall,record.payload,record.certificate.map(|certificate|certificate.account_ed25519_pub)));
            }
            Ok(ChannelReactionPage{rows:out,next})
        })
    }
    /// Exact erasure uses fixed-size metadata, so display budgets cannot block deletion.
    pub(in crate::node) fn remove_matching(
        &mut self,
        remove: impl Fn(ConversationId, u64, EventId) -> Result<bool, LogError>,
    ) -> Result<(), LogError> {
        self.check_write()?;
        for table in ["authored_reaction_keys", "private_channel_reactions"] {
            let mut after = 0i64;
            loop {
                let rows=self.source.profile().database().read(|c| {
                    self.check_account(c)?;
                    let mut statement=c.prepare(&format!("SELECT ordinal,conversation,clock,target FROM {table} WHERE account=?1 AND ordinal>?2 ORDER BY ordinal LIMIT 128"))?;
                    let rows=statement.query_map(params![self.account.as_slice(),after],|r| Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,Vec<u8>>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
                    Ok(rows)
                })?;
                if rows.is_empty() {
                    break;
                }
                let mut selected = Vec::new();
                for (ordinal, conversation, clock, target) in rows {
                    after = ordinal;
                    let conversation = ConversationId::new(
                        conversation
                            .try_into()
                            .map_err(|_| invalid("invalid reaction conversation"))?,
                    );
                    let wall = u64::from_be_bytes(
                        clock
                            .try_into()
                            .map_err(|_| invalid("invalid reaction clock"))?,
                    );
                    let target = EventId::new(
                        target
                            .try_into()
                            .map_err(|_| invalid("invalid reaction target"))?,
                    );
                    if remove(conversation, wall, target)? {
                        selected.push(ordinal);
                    }
                }
                self.source.profile().database().transaction(|c| {
                    self.check_account(c)?;
                    let mut removed=0;
                    for ordinal in selected {removed+=c.execute(&format!("DELETE FROM {table} WHERE account=?1 AND ordinal=?2"),params![self.account.as_slice(),ordinal])?;}
                    if removed>0 {c.execute("UPDATE authored_reaction_state SET revision=revision+1 WHERE account=?1",[self.account.as_slice()])?;}
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::node) fn channel_entries_with_account(
        &self,
        conversation: ConversationId,
    ) -> Vec<super::RetainedChannelReaction> {
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .channel_page(conversation, after, 128, 8 * 1024 * 1024)
                .unwrap();
            if page.rows.is_empty() {
                return out;
            }
            after = page.next;
            out.extend(page.rows);
        }
    }
    #[cfg(test)]
    pub(in crate::node) fn channel_entries(
        &self,
        conversation: ConversationId,
    ) -> Vec<(
        crate::identity::device::PublicIdentity,
        EventId,
        u64,
        ReactionPayload,
    )> {
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .channel_page(conversation, after, 128, 8 * 1024 * 1024)
                .unwrap();
            if page.rows.is_empty() {
                return out;
            }
            after = page.next;
            out.extend(
                page.rows
                    .into_iter()
                    .map(|(source, event, wall, payload, _)| (source, event, wall, payload)),
            );
        }
    }
}
