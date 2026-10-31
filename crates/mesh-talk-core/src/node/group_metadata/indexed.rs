//! Selected metadata controls share the profile lease and channel authority.
//! Only identifiers and authenticated publication/release facts are indexed;
//! message bodies and historical plaintext are never a metadata working set.
use super::*;
use crate::eventlog::sync::network::{PreparationBudget, PreparationStep};
use crate::{
    eventlog::repository::{EventHeader, EventRepository},
    node::channel::checkpoint::ChannelSource,
};
use rusqlite::{Connection, OptionalExtension, params};

mod preparation;
pub(super) use preparation::PreparationCache;

const SCHEMA: &str = "
CREATE TABLE group_metadata_index(channel BLOB PRIMARY KEY CHECK(length(channel)=32), job BLOB NOT NULL CHECK(length(job)=16), source_job BLOB NOT NULL CHECK(length(source_job)=16), authority_stamp INTEGER NOT NULL, account TEXT NOT NULL, identity BLOB NOT NULL CHECK(length(identity)=64), after_rank INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE group_metadata_publications(job BLOB NOT NULL, event BLOB NOT NULL CHECK(length(event)=32), rank INTEGER NOT NULL, lamport BLOB NOT NULL CHECK(length(lamport)=8), field INTEGER NOT NULL CHECK(field IN(0,1)), author BLOB NOT NULL CHECK(length(author)=64), account TEXT NOT NULL, PRIMARY KEY(job,event)) WITHOUT ROWID;
CREATE INDEX group_metadata_publication_field ON group_metadata_publications(job,field,rank,event);
CREATE INDEX group_metadata_publication_clock ON group_metadata_publications(job,field,lamport,event);
CREATE TABLE group_metadata_releases(job BLOB NOT NULL,event BLOB NOT NULL CHECK(length(event)=32),publication BLOB NOT NULL CHECK(length(publication)=32),recipient BLOB NOT NULL CHECK(length(recipient)=64),author BLOB NOT NULL CHECK(length(author)=64),rank INTEGER NOT NULL,PRIMARY KEY(job,event)) WITHOUT ROWID;
CREATE INDEX group_metadata_release_recipient ON group_metadata_releases(job,publication,recipient,rank,event);
CREATE INDEX group_metadata_release_author ON group_metadata_releases(job,publication,recipient,author);
CREATE TABLE group_metadata_key_search(job BLOB NOT NULL,publication BLOB NOT NULL,after_rank INTEGER NOT NULL,selected BLOB,PRIMARY KEY(job,publication)) WITHOUT ROWID;
";

pub(in crate::node) fn initialize(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='group_metadata_index')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
fn retire_rows_on(c: &Connection, job: &[u8], limit: usize) -> Result<usize, LogError> {
    let mut remaining = limit;
    for (table, key) in [
        ("group_metadata_key_search", "publication"),
        ("group_metadata_releases", "event"),
        ("group_metadata_publications", "event"),
    ] {
        let sql = format!(
            "DELETE FROM {table} WHERE job=?1 AND {key} IN(SELECT {key} FROM {table} WHERE job=?1 LIMIT ?2)"
        );
        remaining -= c.execute(&sql, params![job, remaining as i64])?;
        if remaining == 0 {
            break;
        }
    }
    Ok(limit - remaining)
}
/// The checkpoint retirement owner has already classified this conversation as
/// retired or orphaned. Its transaction and turn budget own this cleanup; no
/// metadata retry or user read is required to reclaim the disposable rows.
pub(in crate::node) fn retire_index_on(
    c: &Connection,
    channel: ConversationId,
    limit: usize,
) -> Result<usize, LogError> {
    if limit == 0 {
        return Err(LogError::InvalidPageBudget);
    }
    let limit = limit.min(128);
    let job: Option<Vec<u8>> = c
        .query_row(
            "SELECT job FROM group_metadata_index WHERE channel=?1",
            [channel.as_bytes().as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(job) = job else {
        return Ok(0);
    };
    let mut removed = retire_rows_on(c, &job, limit)?;
    if removed < limit {
        removed += c.execute(
            "DELETE FROM group_metadata_index WHERE channel=?1",
            [channel.as_bytes().as_slice()],
        )?;
    }
    Ok(removed)
}
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn identity_key(public: &PublicIdentity) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[..32].copy_from_slice(&public.ed25519_pub);
    bytes[32..].copy_from_slice(&public.x25519_pub);
    bytes
}
fn public(bytes: Vec<u8>) -> Result<PublicIdentity, LogError> {
    if bytes.len() != 64 {
        return Err(invalid("invalid metadata public identity"));
    }
    Ok(PublicIdentity {
        ed25519_pub: bytes[..32]
            .try_into()
            .map_err(|_| invalid("invalid signing key"))?,
        x25519_pub: bytes[32..]
            .try_into()
            .map_err(|_| invalid("invalid exchange key"))?,
    })
}
fn event_id(bytes: Vec<u8>) -> Result<EventId, LogError> {
    Ok(EventId::new(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid metadata event identity"))?,
    ))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    pub job: [u8; 16],
    pub authority: i64,
    pub metadata: i64,
}

/// The same body/byte budget covers discovery, release search and selected values.
/// A too-large stored row reports its required budget before fetching the body.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Budget {
    bodies: usize,
    bytes: usize,
    rows: usize,
    edges: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            bodies: 64,
            bytes: 8 * 1024 * 1024,
            rows: 128,
            edges: 512,
        }
    }
}
impl Budget {
    #[cfg(test)]
    pub(super) fn limit_rows_for_test(&mut self, rows: usize) {
        self.rows = self.rows.min(rows);
    }
    pub fn exhausted(&self) -> bool {
        self.bodies < 2 || self.rows < 2 || self.edges == 0 || self.bytes < 256
    }
    pub(super) fn preparation(&self) -> PreparationBudget {
        PreparationBudget {
            rows: self.rows.min(self.bodies),
            edges: self.edges,
            bytes: self.bytes,
        }
    }
    pub(super) fn charge(&mut self, work: PreparationStep) -> Result<(), LogError> {
        let limit = self.preparation();
        if work.rows > limit.rows || work.edges > limit.edges || work.bytes > limit.bytes {
            return Err(invalid("metadata authority exceeded its work budget"));
        }
        // Every authority row may decode a control. Charge conservatively against
        // the same body allowance as selected metadata hydration, including rejected rows.
        let rows = work.rows.max(1);
        self.rows -= rows;
        self.bodies -= rows;
        self.edges -= work.edges;
        self.bytes -= work.bytes;
        Ok(())
    }
    fn row(&mut self) -> bool {
        if self.rows == 0 {
            false
        } else {
            self.rows -= 1;
            true
        }
    }
    fn load(
        &mut self,
        repository: &EventRepository,
        id: EventId,
    ) -> Result<Option<Event>, LogError> {
        let header = repository.header(&id)?.ok_or(LogError::StaleCursor)?;
        let charge = header
            .stored_bytes
            .checked_mul(2)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if charge > 8 * 1024 * 1024 {
            return Err(LogError::PageBudgetTooSmall { required: charge });
        }
        if self.bodies == 0 || charge > self.bytes {
            self.bodies = 0;
            return Ok(None);
        }
        self.bodies -= 1;
        self.bytes -= charge;
        repository.get(&id)?.ok_or(LogError::StaleCursor).map(Some)
    }
}
pub(super) enum Progress<T> {
    Pending,
    Ready(T),
}
pub(super) struct Index<'a> {
    node: &'a Node,
    repository: &'a EventRepository,
    pub source: ChannelSource,
    pub stamp: Stamp,
    job: [u8; 16],
}
impl<'a> Index<'a> {
    pub fn open(
        node: &'a Node,
        repository: &'a EventRepository,
        channel: ConversationId,
        budget: &mut Budget,
    ) -> Result<Progress<Self>, LogError> {
        let checkpoints = &node.channel_checkpoints;
        let Some(source) = checkpoints.source_binding(channel)? else {
            return Ok(Progress::Pending);
        };
        let authority = checkpoints.control_stamp(&source)?;
        let metadata = checkpoints.profile().database().read(|c| {
            checkpoints.check_source_on(c, &source)?;
            Ok(c.query_row(
                "SELECT rank FROM channel_event_authority WHERE job=?1 AND channel=?2 AND kind=?3 ORDER BY rank DESC LIMIT 1",
                params![source.job.as_slice(), channel.as_bytes().as_slice(), EventKind::ChannelMetadata as i64],
                |r| r.get::<_, i64>(0),
            ).optional()?.unwrap_or(-1))
        })?;
        let stamp = Stamp {
            job: source.job,
            authority,
            metadata,
        };
        let account = node.account_id();
        let identity = identity_key(&node.identity.public());
        let allowance = budget.rows.min(budget.bytes / 256);
        let (job, changed) = checkpoints.profile().database().transaction(|c| {
            checkpoints.check_source_on(c, &source)?;
            type StoredMetadataIdentity = (Vec<u8>,Vec<u8>,i64,String,Vec<u8>);
            let old: Option<StoredMetadataIdentity> = c.query_row(
                "SELECT job,source_job,authority_stamp,account,identity FROM group_metadata_index WHERE channel=?1",
                [channel.as_bytes().as_slice()],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            ).optional()?;
            let mut changed = 0;
            if let Some((job, source_job, old_stamp, old_account, old_identity)) = old {
                if source_job == source.job && old_stamp == authority && old_account == account && old_identity == identity {
                    return Ok((Some(job), 0));
                }
                if allowance == 0 { return Ok((None, 0)); }
                changed += retire_rows_on(c, &job, allowance)?;
                if changed == allowance { return Ok((None, changed)); }
                changed += c.execute("DELETE FROM group_metadata_index WHERE channel=?1", [channel.as_bytes().as_slice()])?;
            }
            if changed == allowance { return Ok((None, changed)); }
            let job = uuid::Uuid::new_v4();
            changed += c.execute("INSERT INTO group_metadata_index VALUES(?1,?2,?3,?4,?5,?6,-1)",
                params![channel.as_bytes().as_slice(),job.as_bytes().as_slice(),source.job.as_slice(),authority,&account,identity.as_slice()])?;
            Ok((Some(job.as_bytes().to_vec()), changed))
        })?;
        budget.rows -= changed;
        budget.bytes -= changed * 256;
        if changed != 0 {
            node.channel_recovery_notify.notify_one();
        }
        Ok(match job {
            Some(job) => Progress::Ready(Self {
                node,
                repository,
                source,
                stamp,
                job: job
                    .try_into()
                    .map_err(|_| invalid("invalid metadata job"))?,
            }),
            None => Progress::Pending,
        })
    }
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, LogError>) -> Result<T, LogError> {
        self.node
            .channel_checkpoints
            .profile()
            .database()
            .read(|c| {
                self.check(c)?;
                f(c)
            })
    }
    fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T, LogError>) -> Result<T, LogError> {
        self.node
            .channel_checkpoints
            .profile()
            .database()
            .transaction(|c| {
                self.check(c)?;
                f(c)
            })
    }
    fn check(&self, c: &Connection) -> Result<(), LogError> {
        self.node
            .channel_checkpoints
            .check_source_on(c, &self.source)?;
        let current: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM group_metadata_index WHERE channel=?1 AND job=?2)",
            params![
                self.source.channel.as_bytes().as_slice(),
                self.job.as_slice()
            ],
            |r| r.get(0),
        )?;
        if !current {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    fn authority(&self) -> crate::node::join_history::indexed::IndexedView<'_> {
        crate::node::join_history::indexed::IndexedView::new(
            &self.node.channel_checkpoints,
            self.repository,
            &self.node.identity,
            self.source,
        )
    }
    pub fn advance(&self, budget: &mut Budget) -> Result<bool, LogError> {
        let mut cache = self
            .node
            .group_metadata_cache
            .lock()
            .expect("metadata cache lock");
        let before = *budget;
        let ready = cache.preparations.advance(self, budget)?;
        if !ready && *budget != before {
            self.node.channel_recovery_notify.notify_one();
        }
        Ok(ready)
    }
    fn release_is_current(
        &self,
        header: &EventHeader,
        publication: EventId,
    ) -> Result<bool, LogError> {
        let field: Option<i64> = self.read(|c| {
            c.query_row(
                "SELECT field FROM group_metadata_publications WHERE job=?1 AND event=?2",
                params![self.job.as_slice(), publication.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })?;
        let Some(field) = field else {
            return Ok(false);
        };
        let latest: Option<Vec<u8>> = self.read(|c| c.query_row(
            "SELECT event FROM group_metadata_publications WHERE job=?1 AND field=?2 AND (lamport,event)<(?3,?4) ORDER BY lamport DESC,event DESC LIMIT 1",
            params![self.job.as_slice(), field, header.lamport.to_be_bytes().as_slice(), header.id.as_bytes().as_slice()],
            |r| r.get(0),
        ).optional().map_err(Into::into))?;
        Ok(latest.as_deref() == Some(publication.as_bytes().as_slice()))
    }
    pub fn publications(&self, field: Field, limit: usize) -> Result<Vec<EventId>, LogError> {
        if limit > MAX_HISTORY {
            return Err(LogError::InvalidPageBudget);
        }
        self.read(|c|{let mut stmt=c.prepare("SELECT event FROM group_metadata_publications WHERE job=?1 AND field=?2 ORDER BY rank DESC,event DESC LIMIT ?3")?;let mut rows=stmt.query(params![self.job.as_slice(),field as i64,limit as i64])?;let mut result=Vec::new();while let Some(row)=rows.next()? {result.push(event_id(row.get(0)?)?);}Ok(result)})
    }
    pub fn load(&self, id: EventId, budget: &mut Budget) -> Result<Progress<Selected>, LogError> {
        let row: (Vec<u8>, String) = self.read(|c| {
            c.query_row(
                "SELECT author,account FROM group_metadata_publications WHERE job=?1 AND event=?2",
                params![self.job.as_slice(), id.as_bytes().as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        let Some(event) = budget.load(self.repository, id)? else {
            return Ok(Progress::Pending);
        };
        let Some(Control::Publish {
            field,
            keys,
            ciphertext,
        }) = decode(&event)
        else {
            return Err(invalid("indexed metadata publication changed"));
        };
        Ok(Progress::Ready(Selected {
            id,
            field,
            keys,
            ciphertext,
            author: public(row.0)?,
            account: row.1,
            wall_clock: event.wall_clock,
        }))
    }
    pub fn key(
        &self,
        publication: &Selected,
        budget: &mut Budget,
    ) -> Result<Progress<Option<GroupKey>>, LogError> {
        if let Some(bytes) = publication
            .keys
            .open(&self.node.identity, &publication.author.x25519_pub)
        {
            let bytes = zeroize::Zeroizing::new(bytes);
            if let Some(key) = key(&bytes, &publication.ciphertext) {
                return Ok(Progress::Ready(Some(key)));
            }
        }
        let saved:Option<(i64,Option<Vec<u8>>)>=self.read(|c|c.query_row("SELECT after_rank,selected FROM group_metadata_key_search WHERE job=?1 AND publication=?2",params![self.job.as_slice(),publication.id.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(Into::into))?;
        let after = saved.as_ref().map_or(-1, |r| r.0);
        if let Some((_, Some(id))) = saved {
            let id = event_id(id)?;
            let Some(event) = budget.load(self.repository, id)? else {
                return Ok(Progress::Pending);
            };
            let author: Vec<u8> = self.read(|c| {
                c.query_row(
                    "SELECT author FROM group_metadata_releases WHERE job=?1 AND event=?2",
                    params![self.job.as_slice(), id.as_bytes().as_slice()],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })?;
            return Ok(Progress::Ready(self.release_key(
                &event,
                &public(author)?,
                publication,
            )));
        }
        let releases:Vec<(i64,EventId,PublicIdentity)>=self.read(|c|{let mut stmt=c.prepare("SELECT rank,event,author FROM group_metadata_releases WHERE job=?1 AND publication=?2 AND recipient=?3 AND rank>?4 ORDER BY rank,event LIMIT 32")?;let mut rows=stmt.query(params![self.job.as_slice(),publication.id.as_bytes().as_slice(),identity_key(&self.node.identity.public()).as_slice(),after])?;let mut page=Vec::new();while let Some(row)=rows.next()?{page.push((row.get(0)?,event_id(row.get(1)?)?,public(row.get(2)?)?));}Ok(page)})?;
        let full = releases.len() == 32;
        for (rank, id, author) in releases {
            if !budget.row() {
                return Ok(Progress::Pending);
            }
            let Some(event) = budget.load(self.repository, id)? else {
                return Ok(Progress::Pending);
            };
            let result = self.release_key(&event, &author, publication);
            self.write(|c|{c.execute("INSERT INTO group_metadata_key_search VALUES(?1,?2,?3,?4) ON CONFLICT(job,publication) DO UPDATE SET after_rank=excluded.after_rank,selected=excluded.selected",params![self.job.as_slice(),publication.id.as_bytes().as_slice(),rank,result.as_ref().map(|_|id.as_bytes().as_slice())])?;Ok(())})?;
            if result.is_some() {
                return Ok(Progress::Ready(result));
            }
        }
        Ok(if full {
            Progress::Pending
        } else {
            Progress::Ready(None)
        })
    }
    fn release_key(
        &self,
        event: &Event,
        author: &PublicIdentity,
        publication: &Selected,
    ) -> Option<GroupKey> {
        let Control::Release {
            publication: id,
            recipient,
            sealed_key,
        } = decode(event)?
        else {
            return None;
        };
        if id != publication.id || recipient != self.node.identity.public() {
            return None;
        }
        let bytes = zeroize::Zeroizing::new(
            crate::dm::open(&self.node.identity, &author.x25519_pub, &sealed_key).ok()?,
        );
        key(&bytes, &publication.ciphertext)
    }
    pub fn already_released(
        &self,
        publication: EventId,
        recipient: &PublicIdentity,
    ) -> Result<bool, LogError> {
        self.read(|c|c.query_row("SELECT EXISTS(SELECT 1 FROM group_metadata_releases WHERE job=?1 AND publication=?2 AND recipient=?3 AND author=?4)",params![self.job.as_slice(),publication.as_bytes().as_slice(),identity_key(recipient).as_slice(),identity_key(&self.node.identity.public()).as_slice()],|r|r.get(0)).map_err(Into::into))
    }
    pub fn value(
        &self,
        publication: &Selected,
        budget: &mut Budget,
    ) -> Result<Progress<Option<ChannelMetadataValue>>, LogError> {
        let key = match self.key(publication, budget)? {
            Progress::Pending => return Ok(Progress::Pending),
            Progress::Ready(key) => key,
        };
        Ok(Progress::Ready(key.and_then(|key| publication.value(&key))))
    }
    pub fn check_source(&self) -> Result<(), LogError> {
        self.read(|_| Ok(()))
    }
}
pub(super) struct Selected {
    pub id: EventId,
    field: Field,
    pub keys: SealedPayload,
    ciphertext: Vec<u8>,
    author: PublicIdentity,
    account: String,
    wall_clock: u64,
}
impl Selected {
    fn value(&self, key: &GroupKey) -> Option<ChannelMetadataValue> {
        let bytes = zeroize::Zeroizing::new(open_channel_message(key, &self.ciphertext).ok()?);
        let content: Option<String> = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit((MAX_COVER + 32) as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .ok()?;
        validate_content(self.field, content.as_deref()).ok()?;
        Some(ChannelMetadataValue {
            revision: hex::encode(self.id.as_bytes()),
            content,
            author: self.account.clone(),
            authored_at: self.wall_clock,
        })
    }
}
fn key(bytes: &[u8], ciphertext: &[u8]) -> Option<GroupKey> {
    let key = GroupKey::from_bytes(bytes.try_into().ok()?);
    open_channel_message(&key, ciphertext).ok()?;
    Some(key)
}
