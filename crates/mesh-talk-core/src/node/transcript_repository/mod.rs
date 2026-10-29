//! Profile-owned encrypted transcript rows. Headers and bodies have separate
//! tables: SQL selection admits a bounded set of keys/bytes before fetching bodies.
use crate::eventlog::event::EventId;
use crate::eventlog::{LogError, event::ConversationId};
use crate::storage::profile::{context::ProfileStorage, manifest};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};

mod metadata;
mod pages;
pub(crate) mod publication;
#[cfg(test)]
mod tests;

pub(crate) const SENT: i64 = 1;
pub(crate) const RECEIVED: i64 = 2;
pub(crate) const FILES: i64 = 3;
pub(crate) const SUPPRESSION: i64 = 4;
pub(crate) const RECOVERED_IDS: i64 = 5;
pub(crate) const FILE_PROOFS: i64 = 6;
pub(crate) const FILE_IDENTITIES: i64 = 7;
pub(crate) const MAX_PAGE_ROWS: usize = 128;
pub(crate) const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

const SCHEMA: &str = "
CREATE TABLE transcript_sources(kind INTEGER PRIMARY KEY CHECK(kind BETWEEN 1 AND 7), locator BLOB NOT NULL UNIQUE, stub BLOB NOT NULL CHECK(length(stub)=54), raw_size INTEGER NOT NULL CHECK(raw_size>=0), raw_hash BLOB NOT NULL CHECK(length(raw_hash)=32), published INTEGER NOT NULL CHECK(published IN (0,1)), record_count INTEGER NOT NULL DEFAULT 0 CHECK(record_count>=0), body_bytes INTEGER NOT NULL DEFAULT 0 CHECK(body_bytes>=0), revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(revision)='integer' AND revision>=0), rewrite_revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(rewrite_revision)='integer' AND rewrite_revision>=0));
CREATE TABLE transcript_rows(ordinal INTEGER PRIMARY KEY AUTOINCREMENT, kind INTEGER NOT NULL REFERENCES transcript_sources(kind), conversation BLOB NOT NULL CHECK(length(conversation)=32), row_key BLOB NOT NULL CHECK(length(row_key) BETWEEN 9 AND 33), clock BLOB NOT NULL CHECK(length(clock)=8), sequence BLOB NOT NULL CHECK(length(sequence)=8), sender_bytes INTEGER NOT NULL CHECK(sender_bytes>=0), body_bytes INTEGER NOT NULL CHECK(body_bytes>=0), sender TEXT NOT NULL, deadline BLOB CHECK(deadline IS NULL OR length(deadline)=8), logical_id BLOB CHECK(logical_id IS NULL OR length(logical_id)=32), plaintext_sha256 BLOB NOT NULL CHECK(length(plaintext_sha256)=32), file_conversation BLOB CHECK(file_conversation IS NULL OR (kind=3 AND length(file_conversation)=32)), UNIQUE(kind,conversation,row_key));
CREATE INDEX transcript_row_keys ON transcript_rows(kind,row_key,ordinal);
CREATE INDEX transcript_history ON transcript_rows(kind,conversation,clock,sequence,ordinal);
CREATE INDEX transcript_all_history ON transcript_rows(kind,clock,sequence,ordinal);
CREATE INDEX transcript_scan ON transcript_rows(kind,ordinal);
CREATE INDEX transcript_conversation_scan ON transcript_rows(kind,conversation,ordinal);
CREATE INDEX transcript_deadlines ON transcript_rows(kind,deadline,ordinal) WHERE deadline IS NOT NULL;
CREATE INDEX transcript_logical ON transcript_rows(kind,conversation,logical_id,ordinal) WHERE logical_id IS NOT NULL;
CREATE INDEX transcript_file_conversations ON transcript_rows(kind,file_conversation,ordinal) WHERE file_conversation IS NOT NULL;
CREATE TABLE transcript_bodies(ordinal INTEGER PRIMARY KEY REFERENCES transcript_rows(ordinal) ON DELETE CASCADE, sender TEXT NOT NULL, plaintext BLOB NOT NULL);
CREATE TABLE transcript_conversations(kind INTEGER NOT NULL REFERENCES transcript_sources(kind), conversation BLOB NOT NULL CHECK(length(conversation)=32), record_count INTEGER NOT NULL CHECK(record_count>=0), body_bytes INTEGER NOT NULL CHECK(body_bytes>=0), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0), PRIMARY KEY(kind,conversation)) WITHOUT ROWID;
CREATE TABLE transcript_suppression(conversation BLOB NOT NULL CHECK(length(conversation)=32), id BLOB NOT NULL CHECK(length(id)=32), PRIMARY KEY(conversation,id)) WITHOUT ROWID;
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptUsage {
    pub records: u64,
    pub body_bytes: u64,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct TranscriptPageLimit {
    rows: usize,
    bytes: usize,
}
impl TranscriptPageLimit {
    pub(crate) fn new(rows: usize, bytes: usize) -> Result<Self, LogError> {
        if rows == 0 || rows > MAX_PAGE_ROWS || bytes == 0 || bytes > MAX_PAGE_BYTES {
            return Err(LogError::InvalidPageBudget);
        }
        Ok(Self { rows, bytes })
    }
    fn sql_limit(self) -> Result<i64, LogError> {
        Self::new(self.rows, self.bytes)?;
        i64::try_from(self.rows)
            .ok()
            .and_then(|rows| rows.checked_add(1))
            .ok_or(LogError::InvalidPageBudget)
    }
}
impl Default for TranscriptPageLimit {
    fn default() -> Self {
        Self {
            rows: MAX_PAGE_ROWS,
            bytes: MAX_PAGE_BYTES,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptCursor {
    pub wall_clock: u64,
    pub sequence: u64,
    pub ordinal: i64,
}
pub(crate) struct TranscriptPage<T> {
    pub rows: Vec<T>,
    pub next: Option<TranscriptCursor>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptDeadlineCursor {
    pub deadline: u64,
    pub ordinal: i64,
}
pub(crate) struct TranscriptDeadline {
    pub cursor: TranscriptDeadlineCursor,
    pub conversation: ConversationId,
    pub event_id: Option<EventId>,
    pub logical_id: Option<EventId>,
    pub sequence: u64,
    pub sender: String,
    pub wall_clock: u64,
}
pub(crate) struct TranscriptScanPage<T> {
    pub rows: Vec<T>,
    pub next: Option<i64>,
}

pub(crate) struct TranscriptRepository {
    pub(super) profile: Arc<ProfileStorage>,
    pub(super) kind: i64,
    source: publication::PublishedHistorySource,
    revision: u64,
    rewrite_revision: u64,
    #[cfg(test)]
    fail_writes: bool,
    #[cfg(test)]
    fail_next: std::cell::Cell<bool>,
    #[cfg(test)]
    body_reads: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Debug)]
pub(crate) struct TranscriptHeader {
    pub ordinal: i64,
    pub conversation: ConversationId,
    pub key: Vec<u8>,
    pub wall_clock: u64,
    pub sequence: u64,
    pub sender_bytes: usize,
    pub body_bytes: usize,
    pub deadline: Option<u64>,
    pub logical_id: Option<EventId>,
    pub plaintext_sha256: [u8; 32],
    /// Selector only; it carries no manifest authorization.
    pub file_conversation: Option<ConversationId>,
}
type Header = TranscriptHeader;
pub(super) struct StoredRow {
    pub header: Header,
    pub sender: String,
    pub plaintext: Vec<u8>,
}
const HEADER_COLUMNS: &str = "ordinal,conversation,row_key,clock,sequence,sender_bytes,body_bytes,deadline,logical_id,plaintext_sha256,file_conversation";

pub(super) fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn size(value: usize) -> Result<i64, LogError> {
    i64::try_from(value).map_err(|_| invalid("transcript size overflow"))
}
fn blob<const N: usize>(row: &rusqlite::Row<'_>, index: usize) -> Result<[u8; N], LogError> {
    row.get_ref(index)?
        .as_blob()
        .map_err(|_| invalid("invalid transcript blob"))?
        .try_into()
        .map_err(|_| invalid("invalid transcript identifier width"))
}
fn header(row: &rusqlite::Row<'_>) -> Result<Header, LogError> {
    let key = row
        .get_ref(2)?
        .as_blob()
        .map_err(|_| invalid("invalid transcript key"))?;
    if !(9..=33).contains(&key.len()) {
        return Err(invalid("invalid transcript key length"));
    }
    let read_size = |index| -> Result<usize, LogError> {
        let n: i64 = row.get(index)?;
        usize::try_from(n).map_err(|_| invalid("invalid transcript byte count"))
    };
    let result = Header {
        ordinal: row.get(0)?,
        conversation: ConversationId::new(blob(row, 1)?),
        key: key.to_vec(),
        wall_clock: u64::from_be_bytes(blob(row, 3)?),
        sequence: u64::from_be_bytes(blob(row, 4)?),
        sender_bytes: read_size(5)?,
        body_bytes: read_size(6)?,
        deadline: optional_blob::<8>(row, 7)?.map(u64::from_be_bytes),
        logical_id: optional_blob::<32>(row, 8)?.map(EventId::new),
        plaintext_sha256: blob(row, 9)?,
        file_conversation: optional_blob::<32>(row, 10)?.map(ConversationId::new),
    };
    if result.ordinal <= 0 {
        return Err(invalid("invalid transcript ordinal"));
    }
    Ok(result)
}

fn optional_blob<const N: usize>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> Result<Option<[u8; N]>, LogError> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Null => Ok(None),
        _ => blob(row, index).map(Some),
    }
}

fn initialize(profile: &ProfileStorage) -> Result<(), LogError> {
    let database = profile
        .participant_database()
        .ok_or_else(|| invalid("transcript profile is not initialized"))?;
    database.transaction(|c| {
        let present: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",
            [],
            |r| r.get(0),
        )?;
        if !present {
            c.execute_batch(SCHEMA)?;
        }
        manifest::validate_ddl(c, SCHEMA)
    })
}

impl TranscriptRepository {
    pub(super) fn open(
        profile: Arc<ProfileStorage>,
        source: &Path,
        password: &str,
        kind: i64,
        import: impl FnOnce(&Connection, &Path, &str) -> Result<(), LogError>,
    ) -> Result<Self, LogError> {
        let source = publication::PublishedHistorySource::open(
            profile.clone(),
            source,
            password,
            kind,
            import,
        )?;
        let (revision, rewrite_revision) = profile.database().read(|c| {
            let (revision, rewrite): (i64, i64) = c.query_row(
                "SELECT revision,rewrite_revision FROM transcript_sources WHERE kind=?1",
                [kind],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((
                u64::try_from(revision).map_err(|_| invalid("invalid source revision"))?,
                u64::try_from(rewrite).map_err(|_| invalid("invalid source revision"))?,
            ))
        })?;
        Ok(Self {
            profile,
            kind,
            source,
            revision,
            rewrite_revision,
            #[cfg(test)]
            fail_writes: false,
            #[cfg(test)]
            fail_next: std::cell::Cell::new(false),
            #[cfg(test)]
            body_reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    pub(super) fn check_write(&self) -> Result<(), LogError> {
        #[cfg(test)]
        if self.fail_writes || self.fail_next.replace(false) {
            return Err(std::io::Error::other("injected transcript write failure").into());
        }
        self.source.check_write()
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn rewrite_revision(&self) -> u64 {
        self.rewrite_revision
    }
    #[cfg(test)]
    pub(crate) fn body_reads(&self) -> usize {
        self.body_reads.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(test)]
    pub(crate) fn fail_appends_for_test(&mut self, enabled: bool) -> std::io::Result<()> {
        self.fail_writes = enabled;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn fail_next_append_rollback_for_test(&mut self, _bytes: usize) {
        self.fail_next.set(true);
    }
    pub(crate) fn usage(&self) -> Result<TranscriptUsage, LogError> {
        self.profile.database().read(|c| {
            let (records, bytes): (i64, i64) = c.query_row("SELECT record_count,body_bytes FROM transcript_sources WHERE kind=?1 AND published=1", [self.kind], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(TranscriptUsage {
                records: u64::try_from(records).map_err(|_| invalid("invalid transcript count"))?,
                body_bytes: u64::try_from(bytes).map_err(|_| invalid("invalid transcript byte count"))?,
            })
        })
    }
    pub(crate) fn count(&self, conversation: Option<&ConversationId>) -> Result<usize, LogError> {
        let n = match conversation {
            None => self.usage()?.records,
            Some(conversation) => self.profile.database().read(|c| {
                let n: i64 = c.query_row("SELECT record_count FROM transcript_conversations WHERE kind=?1 AND conversation=?2", params![self.kind,conversation.as_bytes().as_slice()], |r|r.get(0)).optional()?.unwrap_or(0);
                u64::try_from(n).map_err(|_| invalid("invalid transcript count"))
            })?,
        };
        usize::try_from(n).map_err(|_| invalid("transcript record count exceeds address space"))
    }
    pub(crate) fn conversation_page(
        &self,
        after: Option<ConversationId>,
        limit: usize,
    ) -> Result<Vec<ConversationId>, LogError> {
        if limit == 0 || limit > MAX_PAGE_ROWS {
            return Err(invalid("transcript conversation page exceeds budget"));
        }
        self.profile.database().read(|c| {
            let cursor = if after.is_some() { " AND conversation>?2" } else { " AND ?2 IS NULL" };
            let mut statement=c.prepare(&format!("SELECT conversation FROM transcript_conversations WHERE kind=?1 AND record_count>0{cursor} ORDER BY conversation LIMIT ?3"))?;
            let mut rows=statement.query(params![self.kind,after.as_ref().map(|id|id.as_bytes().as_slice()),size(limit)?])?;
            let mut selected=Vec::new(); while let Some(row)=rows.next()? {selected.push(ConversationId::new(blob(row,0)?));} Ok(selected)
        })
    }
    pub(super) fn load_on(&self, c: &Connection, h: Header) -> Result<StoredRow, LogError> {
        let bytes = h
            .body_bytes
            .checked_add(h.sender_bytes)
            .ok_or_else(|| invalid("transcript row byte overflow"))?;
        if bytes > MAX_PAGE_BYTES {
            return Err(invalid("transcript row exceeds payload budget"));
        }
        let mut statement =
            c.prepare("SELECT sender,plaintext FROM transcript_bodies WHERE ordinal=?1")?;
        let mut rows = statement.query([h.ordinal])?;
        let row = rows
            .next()?
            .ok_or_else(|| invalid("transcript body missing"))?;
        let sender = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| invalid("invalid transcript sender"))?;
        let plaintext = row
            .get_ref(1)?
            .as_blob()
            .map_err(|_| invalid("invalid transcript body"))?;
        if sender.len() != h.sender_bytes || plaintext.len() != h.body_bytes {
            return Err(invalid("transcript body disagrees with header"));
        }
        #[cfg(test)]
        self.body_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(StoredRow {
            header: h,
            sender: sender.to_owned(),
            plaintext: plaintext.to_vec(),
        })
    }
    pub(super) fn lookup(
        &self,
        conversation: Option<&ConversationId>,
        key: &[u8],
    ) -> Result<Option<StoredRow>, LogError> {
        self.profile.database().read(|c| {
            let h = Self::find_header_on(c, self.kind, conversation, key)?;
            h.map(|h| self.load_on(c, h)).transpose()
        })
    }
    fn find_header_on(
        c: &Connection,
        kind: i64,
        conversation: Option<&ConversationId>,
        key: &[u8],
    ) -> Result<Option<Header>, LogError> {
        let host = if conversation.is_some() {
            " AND conversation=?3"
        } else {
            " AND ?3 IS NULL"
        };
        let query = format!(
            "SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1 AND row_key=?2{host} ORDER BY ordinal LIMIT 1"
        );
        let mut statement = c.prepare(&query)?;
        let mut rows = statement.query(params![
            kind,
            key,
            conversation.map(|id| id.as_bytes().as_slice())
        ])?;
        rows.next()?.map(header).transpose()
    }
    /// Import and runtime use the same exact conflict rules. Received non-durable
    /// backfill may ignore an existing ID; the durable journal always compares it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn put_on(
        c: &Connection,
        kind: i64,
        conversation: ConversationId,
        key: &[u8],
        clock: u64,
        sequence: u64,
        sender: &str,
        plaintext: &[u8],
        alias: bool,
        ignore_existing: bool,
    ) -> Result<bool, LogError> {
        if plaintext
            .len()
            .checked_add(sender.len())
            .is_none_or(|bytes| bytes > MAX_PAGE_BYTES)
        {
            return Err(invalid("transcript row exceeds payload budget"));
        }
        let local = Self::find_header_on(c, kind, Some(&conversation), key)?;
        let previous = if kind == SENT {
            local.clone()
        } else {
            Self::find_header_on(c, kind, None, key)?
        };
        if ignore_existing && previous.is_some() {
            return Ok(false);
        }
        for (existing, is_local) in [(previous.as_ref(), false), (local.as_ref(), true)] {
            let Some(existing) = existing else { continue };
            let same_fields = existing.wall_clock == clock
                && existing.sequence == sequence
                && existing.sender_bytes == sender.len()
                && existing.body_bytes == plaintext.len();
            if !same_fields || (existing.conversation != conversation && !alias) {
                return Err(invalid("conflicting transcript transaction"));
            }
            let mut statement =
                c.prepare("SELECT sender,plaintext FROM transcript_bodies WHERE ordinal=?1")?;
            let mut rows = statement.query([existing.ordinal])?;
            let row = rows
                .next()?
                .ok_or_else(|| invalid("transcript body missing"))?;
            if row
                .get_ref(0)?
                .as_str()
                .map_err(|_| invalid("invalid transcript sender"))?
                != sender
                || row
                    .get_ref(1)?
                    .as_blob()
                    .map_err(|_| invalid("invalid transcript body"))?
                    != plaintext
            {
                return Err(invalid("conflicting transcript transaction"));
            }
            if is_local {
                return Ok(false);
            }
        }
        let envelope = (kind != FILES)
            .then(|| super::dm_envelope::DmEnvelope::decode_ref(plaintext))
            .flatten();
        let deadline = crate::expiry::deadline(envelope.as_ref().map_or(plaintext, |env| env.body))
            .map(u64::to_be_bytes);
        let logical_id = envelope.as_ref().map(|env| env.msg_id);
        let fingerprint: [u8; 32] = Sha256::digest(plaintext).into();
        let file_conversation = (kind == FILES)
            .then(|| super::files::validated_manifest(plaintext))
            .flatten()
            .map(|manifest| manifest.file_conv());
        c.execute("INSERT INTO transcript_rows(kind,conversation,row_key,clock,sequence,sender_bytes,body_bytes,sender,deadline,logical_id,plaintext_sha256,file_conversation) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![kind,conversation.as_bytes().as_slice(),key,clock.to_be_bytes().as_slice(),sequence.to_be_bytes().as_slice(),size(sender.len())?,size(plaintext.len())?,sender,deadline.as_ref().map(|id|id.as_slice()),logical_id.as_ref().map(|id|id.as_slice()),fingerprint.as_slice(),file_conversation.as_ref().map(|id|id.as_bytes().as_slice())])?;
        c.execute(
            "INSERT INTO transcript_bodies VALUES(?1,?2,?3)",
            params![c.last_insert_rowid(), sender, plaintext],
        )?;
        c.execute("UPDATE transcript_sources SET record_count=record_count+1,body_bytes=body_bytes+?2,revision=revision+1 WHERE kind=?1",params![kind,size(plaintext.len())?])?;
        c.execute("INSERT INTO transcript_conversations VALUES(?1,?2,1,?3,1) ON CONFLICT(kind,conversation) DO UPDATE SET record_count=record_count+1,body_bytes=body_bytes+excluded.body_bytes,revision=revision+1",params![kind,conversation.as_bytes().as_slice(),size(plaintext.len())?])?;
        Ok(true)
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn put(
        &mut self,
        conversation: ConversationId,
        key: &[u8],
        clock: u64,
        sequence: u64,
        sender: &str,
        plaintext: &[u8],
        alias: bool,
        ignore_existing: bool,
    ) -> Result<(), LogError> {
        self.check_write()?;
        let inserted = self.profile.database().transaction(|c| {
            Self::put_on(
                c,
                self.kind,
                conversation,
                key,
                clock,
                sequence,
                sender,
                plaintext,
                alias,
                ignore_existing,
            )
        })?;
        if inserted {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(())
    }
    pub(super) fn remove_where(
        &mut self,
        mut should_remove: impl FnMut(StoredRow) -> Result<bool, LogError>,
    ) -> Result<usize, LogError> {
        self.check_write()?;
        let removed=self.profile.database().transaction(|c| {
            let mut cursor=None; let mut removed=0;
            loop {
                let (headers,more)=self.select_scan_on(c,None,cursor,TranscriptPageLimit::default())?;
                if headers.is_empty() {break;}
                cursor=headers.last().map(|h|h.ordinal);
                for h in headers {
                    let ordinal=h.ordinal; let conversation=h.conversation; let bytes=h.body_bytes;
                    if should_remove(self.load_on(c,h)?)? {
                        c.execute("DELETE FROM transcript_rows WHERE ordinal=?1 AND kind=?2",params![ordinal,self.kind])?;
                        c.execute("UPDATE transcript_sources SET record_count=record_count-1,body_bytes=body_bytes-?2 WHERE kind=?1",params![self.kind,size(bytes)?])?;
                        c.execute("UPDATE transcript_conversations SET record_count=record_count-1,body_bytes=body_bytes-?3,revision=revision+1 WHERE kind=?1 AND conversation=?2",params![self.kind,conversation.as_bytes().as_slice(),size(bytes)?])?;
                        removed+=1;
                    }
                }
                if !more {break;}
            }
            if removed>0 { c.execute("UPDATE transcript_sources SET revision=revision+1,rewrite_revision=rewrite_revision+1 WHERE kind=?1",[self.kind])?; }
            Ok(removed)
        })?;
        if removed > 0 {
            self.revision = self.revision.wrapping_add(1);
            self.rewrite_revision = self.rewrite_revision.wrapping_add(1);
        }
        Ok(removed)
    }
}
