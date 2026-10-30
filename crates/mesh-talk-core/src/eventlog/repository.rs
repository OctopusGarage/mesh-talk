//! Domain-scoped encrypted event retention for participant and relay profiles.
//! Runtime repositories retain the exclusive profile lease. Raw Lamport order
//! never claims causal order.
use super::{AppendOutcome, Author, ConversationId, Event, EventId, EventKind, LogError};
use crate::storage::database::EncryptedDatabase;
use bincode::Options;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::sync::Arc;

mod ancestry;
mod decode;
pub(crate) mod import;
pub(crate) mod maintenance;
mod metadata;
mod participant;
mod policy;
mod receiving;
pub(crate) mod selection;
#[cfg(test)]
pub(crate) use policy::RepositoryPolicyView;
pub(crate) mod network;
pub(crate) mod order;
pub(crate) mod pages;
pub(crate) mod retirement;
mod schema;
pub(crate) use metadata::ConversationMetadata;
pub(crate) use metadata::DomainChargeMetadata;
pub(crate) use retirement::ConversationChargeMetadata;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EventDomainId(pub [u8; 16]);

/// No parent Vec or body allocation is hidden in a header. Edges have their own pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct EventHeader {
    pub id: EventId,
    pub conversation: ConversationId,
    pub author: Author,
    pub seq: u64,
    pub lamport: u64,
    pub wall_clock: u64,
    pub kind: EventKind,
    pub parent_count: usize,
    pub encoded_bytes: usize,
    pub ciphertext_bytes: usize,
    pub signature_bytes: usize,
    pub stored_bytes: usize,
}
/// Fixed metadata discovery includes retired rows so every examined commit can
/// advance a durable cursor without scanning an unbounded retired prefix.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CommittedHeader {
    pub ordinal: i64,
    pub header: EventHeader,
    pub active: bool,
}
const HEADER_COLUMNS: &str = "id,conversation,author,seq,lamport,wall_clock,kind,parent_count,encoded_bytes,ciphertext_bytes,signature_bytes,stored_bytes";

/// Same-transaction decision after owner validation and before new graph writes.
pub(crate) enum AppendGate<B> {
    Accept,
    Block(B),
}
pub(crate) enum GatedAppend<B> {
    Accepted(AppendOutcome),
    Blocked(B),
}

pub(crate) struct EventRepository {
    database: RepositoryOwner,
    domain: EventDomainId,
    #[cfg(test)]
    before_mutation: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>,
    store_uuid: [u8; 16],
    selection_owner: [u8; 16],
    receive_owner: std::sync::OnceLock<
        crate::storage::profile::participant::event_ownership::ParticipantEventOwner,
    >,
    #[cfg(test)]
    body_decodes: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    fingerprint_ids: std::sync::atomic::AtomicUsize,
    // Candidate-selection header queries only. Selected get_on reads revalidate
    // the header independently before the counted single BLOB fetch.
    #[cfg(test)]
    network_header_fetches: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    body_fetches: std::sync::atomic::AtomicUsize,
}

#[derive(Clone)]
enum RepositoryOwner {
    Profile(Arc<crate::storage::profile::context::ProfileStorage>),
    #[cfg(test)]
    Fixture(Arc<EncryptedDatabase>),
}
impl std::ops::Deref for RepositoryOwner {
    type Target = EncryptedDatabase;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Profile(profile) => profile.database(),
            #[cfg(test)]
            Self::Fixture(database) => database,
        }
    }
}
impl EventRepository {
    #[cfg(test)]
    pub(crate) fn body_read_counts(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering;
        (
            self.body_fetches.load(Ordering::Relaxed),
            self.body_decodes.load(Ordering::Relaxed),
        )
    }
    #[cfg(test)]
    pub(crate) fn new(
        database: Arc<EncryptedDatabase>,
        domain: EventDomainId,
    ) -> Result<Self, LogError> {
        database.transaction(schema::upgrade)?;
        database.transaction(|tx| schema::register(tx, domain))?;
        Self::with_owner(RepositoryOwner::Fixture(database), domain)
    }
    pub(crate) fn initialize_unpublished_domain(
        database: &EncryptedDatabase,
        domain: EventDomainId,
    ) -> Result<(), LogError> {
        database.transaction(|tx| schema::register(tx, domain))
    }
    pub(crate) fn initialize_unpublished_on(
        tx: &rusqlite::Transaction<'_>,
        domain: EventDomainId,
    ) -> Result<(), LogError> {
        schema::register(tx, domain)
    }
    pub(crate) fn validate_existing_domain(
        database: &EncryptedDatabase,
        domain: EventDomainId,
    ) -> Result<(), LogError> {
        database.transaction(schema::upgrade)?;
        database.read(|c| schema::open_existing(c, domain))
    }
    pub(crate) fn from_profile(
        profile: Arc<crate::storage::profile::context::ProfileStorage>,
        domain: EventDomainId,
    ) -> Result<Self, LogError> {
        Self::with_owner(RepositoryOwner::Profile(profile), domain)
    }
    fn with_owner(database: RepositoryOwner, domain: EventDomainId) -> Result<Self, LogError> {
        database.transaction(schema::upgrade)?;
        let store_uuid = database.read(|tx| {
            schema::open_existing(tx, domain)?;
            let mut stmt =
                tx.prepare("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1")?;
            let mut rows = stmt.query([])?;
            let row = rows
                .next()?
                .ok_or_else(|| corrupt("missing database identity"))?;
            blob(row, 0)
        })?;
        Ok(Self {
            database,
            domain,
            #[cfg(test)]
            before_mutation: std::sync::Mutex::new(None),
            store_uuid,
            selection_owner: *uuid::Uuid::new_v4().as_bytes(),
            receive_owner: Default::default(),
            #[cfg(test)]
            body_decodes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            fingerprint_ids: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            network_header_fetches: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            body_fetches: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub(crate) fn append(&self, event: &Event) -> Result<AppendOutcome, LogError> {
        self.database.transaction(|tx| self.append_on(tx, event))
    }

    pub(crate) fn append_on(
        &self,
        c: &Connection,
        event: &Event,
    ) -> Result<AppendOutcome, LogError> {
        match self.append_with_gate_on(c, event, |_| {
            Ok(AppendGate::<std::convert::Infallible>::Accept)
        })? {
            GatedAppend::Accepted(outcome) => Ok(outcome),
            GatedAppend::Blocked(never) => match never {},
        }
    }

    /// Caller owns the connection transaction. The gate cannot outlive validation;
    /// a duplicate is still authenticated but never invokes a growth reservation.
    pub(crate) fn append_with_gate_on<B>(
        &self,
        c: &Connection,
        event: &Event,
        gate: impl FnOnce(u64) -> Result<AppendGate<B>, LogError>,
    ) -> Result<GatedAppend<B>, LogError> {
        self.require_active_on(c, &event.conversation_id)?;
        // Match the pure EventLog validation order, including invalid duplicates.
        if !event.is_canonical() {
            return Err(LogError::NonCanonical);
        }
        if !event.verify_integrity() {
            return Err(LogError::CorruptId);
        }
        if !event.verify_signature() {
            return Err(LogError::BadSignature);
        }
        if self.has_on(c, &event.id)? {
            return Ok(GatedAppend::Accepted(AppendOutcome::Duplicate));
        }
        let domain = self.domain.0.as_slice();
        let conv = event.conversation_id.as_bytes().as_slice();
        let mut missing = Vec::new();
        let mut stmt =
            c.prepare("SELECT conversation FROM retained_events WHERE domain=?1 AND id=?2")?;
        for parent in &event.parents {
            let parent_conv: Option<Vec<u8>> = stmt
                .query_row(params![domain, parent.as_bytes().as_slice()], |r| r.get(0))
                .optional()?;
            match parent_conv {
                Some(bytes) => {
                    let bytes: [u8; 32] = bytes
                        .try_into()
                        .map_err(|_| corrupt("invalid parent conversation"))?;
                    if &bytes[..] != conv {
                        missing.push(*parent);
                    }
                }
                None => missing.push(*parent),
            }
        }
        if !missing.is_empty() {
            return Err(LogError::MissingParents(missing));
        }
        let equivocation: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM retained_events WHERE domain=?1 AND conversation=?2 AND author=?3 AND seq=?4)", params![domain,conv,event.author.ed25519_pub().as_slice(),event.seq.to_be_bytes().as_slice()], |r|r.get(0))?;
        if equivocation {
            return Err(LogError::AuthorEquivocation {
                author: event.author,
                seq: event.seq,
            });
        }
        let estimate = estimated_cost(event.ciphertext.len(), event.parents.len())?;
        let commit: i64 = c.query_row(
            "SELECT next_commit FROM event_domains WHERE domain=?1",
            [domain],
            |r| r.get(0),
        )?;
        let next_commit = advance(commit)?;
        let (source_revision, domain_estimate, domain_count): (i64, i64, i64) = c.query_row(
            "SELECT source_revision,estimated_bytes,event_count FROM event_domains WHERE domain=?1",
            [domain],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let domain_count = advance(domain_count)?;
        let source_revision = advance(source_revision)?;
        let domain_estimate = checked_total(domain_estimate, estimate)?;
        // Preflight permanent arithmetic before asking a role to evict anything.
        // serialized_size computes scalar length without allocating an event body.
        let body_len = usize::try_from(
            bincode::serialized_size(event).map_err(|e| LogError::Serialization(e.to_string()))?,
        )
        .map_err(|_| LogError::RepositoryCounterExhausted)?;
        let (revision,count,stored,v2,v3,v4,max,estimated): (i64,i64,i64,i64,i64,i64,Vec<u8>,i64) = c.query_row("SELECT revision,event_count,stored_bytes,v2_count,v3_count,v4_count,max_lamport,estimated_bytes FROM event_conversations WHERE domain=?1 AND conversation=?2", params![domain,conv], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?.unwrap_or((0,0,0,0,0,0,0u64.to_be_bytes().to_vec(),0));
        let max = u64::from_be_bytes(
            max.try_into()
                .map_err(|_| corrupt("invalid maximum clock"))?,
        );
        let revision = advance(revision)?;
        let estimated = checked_total(estimated, estimate)?;
        let count = advance(count)?;
        let stored = nonnegative(stored)?
            .checked_add(integer(body_len)?)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let (flag2, flag3, flag4) = kind_flags(event.kind);
        let v2 = add_flag(v2, flag2)?;
        let v3 = add_flag(v3, flag3)?;
        let v4 = add_flag(v4, flag4)?;
        if let AppendGate::Block(reason) = gate(estimate as u64)? {
            return Ok(GatedAppend::Blocked(reason));
        }
        let body = bincode::serialize(event).map_err(|e| LogError::Serialization(e.to_string()))?;
        if body.len() != body_len {
            return Err(corrupt("event serialization length changed"));
        }
        let header = EventHeader::of(event, body_len);
        c.execute("INSERT INTO event_conversations(domain,conversation,revision,event_count,stored_bytes,max_lamport,v2_count,v3_count,v4_count) VALUES(?1,?2,0,0,0,?3,0,0,0) ON CONFLICT(domain,conversation) DO NOTHING",params![domain,conv,0_u64.to_be_bytes().as_slice()])?;
        c.execute("INSERT INTO retained_events(domain,id,conversation,author,seq,lamport,wall_clock,kind,parent_count,encoded_bytes,ciphertext_bytes,signature_bytes,stored_bytes,body,commit_ordinal) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![domain,event.id.as_bytes().as_slice(),conv,event.author.ed25519_pub().as_slice(),event.seq.to_be_bytes().as_slice(),event.lamport.to_be_bytes().as_slice(),event.wall_clock.to_be_bytes().as_slice(),event.kind as i64,integer(header.parent_count)?,integer(header.encoded_bytes)?,integer(header.ciphertext_bytes)?,integer(header.signature_bytes)?,integer(header.stored_bytes)?,body,commit])?;
        for parent in &event.parents {
            c.execute(
                "INSERT INTO retained_event_parents VALUES(?1,?2,?3,?4)",
                params![
                    domain,
                    event.id.as_bytes().as_slice(),
                    parent.as_bytes().as_slice(),
                    commit
                ],
            )?;
            if event.kind != EventKind::Profile {
                c.execute(
                    "DELETE FROM event_heads WHERE domain=?1 AND conversation=?2 AND id=?3",
                    params![domain, conv, parent.as_bytes().as_slice()],
                )?;
            }
        }
        if event.kind != EventKind::Profile {
            c.execute(
                "INSERT INTO event_heads VALUES(?1,?2,?3)",
                params![domain, conv, event.id.as_bytes().as_slice()],
            )?;
        }
        c.execute("INSERT INTO event_author_max VALUES(?1,?2,?3,?4) ON CONFLICT(domain,conversation,author) DO UPDATE SET max_seq=max(max_seq,excluded.max_seq)",params![domain,conv,event.author.ed25519_pub().as_slice(),event.seq.to_be_bytes().as_slice()])?;
        c.execute("UPDATE event_conversations SET revision=?3,event_count=?4,stored_bytes=?5,max_lamport=?6,v2_count=?7,v3_count=?8,v4_count=?9,estimated_bytes=?10 WHERE domain=?1 AND conversation=?2",params![domain,conv,revision,count,stored,max.max(event.lamport).to_be_bytes().as_slice(),v2,v3,v4,estimated])?;
        c.execute(
            "UPDATE event_domains SET next_commit=?2,source_revision=?3,estimated_bytes=?4,event_count=?5 WHERE domain=?1",
            params![domain, next_commit,source_revision,domain_estimate,domain_count],
        )?;
        self.order_appended(c, event, revision, count)?;
        Ok(GatedAppend::Accepted(AppendOutcome::Appended))
    }

    fn has_on(&self, c: &Connection, id: &EventId) -> Result<bool, LogError> {
        self.require_visible_id_on(c, id)?;
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM retained_events WHERE domain=?1 AND id=?2)",
            params![self.domain.0.as_slice(), id.as_bytes().as_slice()],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn has(&self, id: &EventId) -> Result<bool, LogError> {
        self.database.read(|c| self.has_on(c, id))
    }

    /// Inherent allocation bound: one complete signed Event, including arbitrarily
    /// many accepted parents. Use body pages when a caller needs a byte budget.
    pub(crate) fn get(&self, id: &EventId) -> Result<Option<Event>, LogError> {
        self.database.read(|c| self.get_on(c, id))
    }
    pub(crate) fn get_on(&self, c: &Connection, id: &EventId) -> Result<Option<Event>, LogError> {
        self.require_visible_id_on(c, id)?;
        // End both SQL result lifetimes before deserializing. During the BLOB
        // copy SQLite and Rust each own encoded bytes; during decode only Rust
        // owns them. This keeps the documented three-copy estimate meaningful.
        let (header, bytes) = {
            let mut stmt = c.prepare(&format!(
                "SELECT {HEADER_COLUMNS},length(body) FROM retained_events WHERE domain=?1 AND id=?2"
            ))?;
            let mut rows =
                stmt.query(params![self.domain.0.as_slice(), id.as_bytes().as_slice()])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let header = read_header(row)?;
            if size(row, 12)? != header.stored_bytes {
                return Err(corrupt("stored event length disagrees with body"));
            }
            #[cfg(test)]
            self.body_fetches
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let bytes: Vec<u8> = c.query_row(
                "SELECT body FROM retained_events WHERE domain=?1 AND id=?2",
                params![self.domain.0.as_slice(), id.as_bytes().as_slice()],
                |row| row.get(0),
            )?;
            (header, bytes)
        };
        decode::preflight(&header, &bytes)?;
        #[cfg(test)]
        self.body_decodes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let limit = u64::try_from(bytes.len())
            .map_err(|_| corrupt("event encoding exceeds decoder limit"))?;
        let event: Event = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(limit)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(|e| LogError::Serialization(e.to_string()))?;
        let body_len = bytes.len();
        // Release encoded input before integrity hashing allocates its own
        // content encoding. Body-page charges cover the decode overlap.
        drop(bytes);
        if event.id != *id
            || header != EventHeader::of(&event, body_len)
            || !event.is_canonical()
            || !event.verify_integrity()
            || !event.verify_signature()
        {
            return Err(corrupt("event body and indexed metadata disagree"));
        }
        Ok(Some(event))
    }
    pub(crate) fn header(&self, id: &EventId) -> Result<Option<EventHeader>, LogError> {
        self.database.read(|c| self.header_on(c, id))
    }
    /// Reuse the retained-event owner inside a same-profile projection transaction.
    pub(crate) fn header_on(
        &self,
        c: &Connection,
        id: &EventId,
    ) -> Result<Option<EventHeader>, LogError> {
        self.require_visible_id_on(c, id)?;
        let mut statement = c.prepare_cached(&format!(
            "SELECT {HEADER_COLUMNS} FROM retained_events WHERE domain=?1 AND id=?2"
        ))?;
        let mut rows =
            statement.query(params![self.domain.0.as_slice(), id.as_bytes().as_slice()])?;
        rows.next()?.map(read_header).transpose()
    }
}
impl EventHeader {
    pub(crate) fn of(event: &Event, stored_bytes: usize) -> Self {
        Self {
            id: event.id,
            conversation: event.conversation_id,
            author: event.author,
            seq: event.seq,
            lamport: event.lamport,
            wall_clock: event.wall_clock,
            kind: event.kind,
            parent_count: event.parents.len(),
            encoded_bytes: super::sync::encoded_event_size(event),
            ciphertext_bytes: event.ciphertext.len(),
            signature_bytes: event.sig.len(),
            stored_bytes,
        }
    }
}
fn corrupt(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn nonnegative(value: i64) -> Result<i64, LogError> {
    if value < 0 {
        Err(corrupt("negative repository counter"))
    } else {
        Ok(value)
    }
}
fn advance(value: i64) -> Result<i64, LogError> {
    nonnegative(value)?
        .checked_add(1)
        .ok_or(LogError::RepositoryCounterExhausted)
}
fn integer(value: usize) -> Result<i64, LogError> {
    i64::try_from(value).map_err(|_| LogError::RepositoryCounterExhausted)
}
fn add_flag(value: i64, flag: bool) -> Result<i64, LogError> {
    nonnegative(value)?
        .checked_add(i64::from(flag))
        .ok_or(LogError::RepositoryCounterExhausted)
}
fn blob<const N: usize>(row: &Row<'_>, column: usize) -> Result<[u8; N], LogError> {
    let value: Vec<u8> = row.get(column)?;
    value
        .try_into()
        .map_err(|_| corrupt("invalid indexed blob length"))
}
fn size(row: &Row<'_>, column: usize) -> Result<usize, LogError> {
    usize::try_from(nonnegative(row.get(column)?)?)
        .map_err(|_| corrupt("indexed length exceeds platform size"))
}
fn kind_flags(kind: EventKind) -> (bool, bool, bool) {
    (
        kind == EventKind::ChannelReplica,
        matches!(
            kind,
            EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory
        ),
        matches!(
            kind,
            EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
        ),
    )
}
fn read_header(row: &Row<'_>) -> Result<EventHeader, LogError> {
    let kind: i64 = row.get(6)?;
    // Explicit frozen discriminants; do not deserialize an enum by a new order.
    let kind = match kind {
        0 => EventKind::Message,
        1 => EventKind::Edit,
        2 => EventKind::Delete,
        3 => EventKind::React,
        4 => EventKind::ReadMarker,
        5 => EventKind::MembershipChange,
        6 => EventKind::KeyRotation,
        7 => EventKind::FileManifest,
        8 => EventKind::Profile,
        9 => EventKind::ChannelRename,
        10 => EventKind::ChannelDissolve,
        11 => EventKind::ChannelOwnerTransfer,
        12 => EventKind::ChannelLeave,
        13 => EventKind::ChannelAccept,
        14 => EventKind::ChannelReplica,
        15 => EventKind::ChannelHistoryPolicy,
        16 => EventKind::ChannelJoinHistory,
        17 => EventKind::ChannelAccountAuthority,
        18 => EventKind::ChannelDeviceInheritance,
        19 => EventKind::ChannelMetadata,
        _ => return Err(corrupt("unknown indexed event kind")),
    };
    Ok(EventHeader {
        id: EventId::new(blob(row, 0)?),
        conversation: ConversationId::new(blob(row, 1)?),
        author: Author::from_ed25519(blob(row, 2)?),
        seq: u64::from_be_bytes(blob(row, 3)?),
        lamport: u64::from_be_bytes(blob(row, 4)?),
        wall_clock: u64::from_be_bytes(blob(row, 5)?),
        kind,
        parent_count: size(row, 7)?,
        encoded_bytes: size(row, 8)?,
        ciphertext_bytes: size(row, 9)?,
        signature_bytes: size(row, 10)?,
        stored_bytes: size(row, 11)?,
    })
}

fn checked_total(value: i64, delta: i64) -> Result<i64, LogError> {
    nonnegative(value)?
        .checked_add(nonnegative(delta)?)
        .ok_or(LogError::RepositoryCounterExhausted)
}
fn estimated_cost(ciphertext: usize, parents: usize) -> Result<i64, LogError> {
    integer(ciphertext)?
        .checked_add(
            integer(parents)?
                .checked_mul(32)
                .ok_or(LogError::RepositoryCounterExhausted)?,
        )
        .and_then(|n| n.checked_add(256))
        .ok_or(LogError::RepositoryCounterExhausted)
}
