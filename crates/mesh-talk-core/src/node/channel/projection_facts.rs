//! Indexed per-event channel authority and bounded resumable reducer state.
use super::{graph::AuthorityGraph, projection::MembershipReducer, *};
use crate::{
    eventlog::{LogError, repository::EventHeader},
    storage::profile::context::ProfileStorage,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;

const DOMAIN: &[u8; 16] = b"MeshPartEventsV1";
const SCHEMA: &str = "
CREATE TABLE channel_projection_work(
 job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),
 next_rank INTEGER NOT NULL CHECK(next_rank>=0),state BLOB,context BLOB NOT NULL,
 PRIMARY KEY(job,channel)
) WITHOUT ROWID;
CREATE TABLE channel_event_authority(
 job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),
 event BLOB NOT NULL CHECK(length(event)=32),rank INTEGER NOT NULL CHECK(rank>=0),
 allowed INTEGER NOT NULL,receive INTEGER NOT NULL,accept_root BLOB,after_accept INTEGER NOT NULL,
 kind INTEGER NOT NULL,author BLOB NOT NULL CHECK(length(author)=32),
 PRIMARY KEY(job,channel,event),UNIQUE(job,channel,rank)
) WITHOUT ROWID;
CREATE INDEX channel_authority_kind ON channel_event_authority(job,channel,kind,rank);
CREATE INDEX channel_authority_author_kind ON channel_event_authority(job,channel,kind,author,rank);
CREATE TABLE channel_departure_facts(
 job BLOB NOT NULL,channel BLOB NOT NULL,member TEXT NOT NULL,event BLOB NOT NULL,
 PRIMARY KEY(job,channel,member,event)
) WITHOUT ROWID;
CREATE TABLE channel_membership_snapshots(
 job BLOB NOT NULL,channel BLOB NOT NULL,rank INTEGER NOT NULL,state BLOB NOT NULL,
 PRIMARY KEY(job,channel,rank)
) WITHOUT ROWID;";

#[derive(Serialize, Deserialize, Default)]
struct Context {
    known_local: bool,
    bindings: HashMap<String, crate::discovery::Announce>,
    roots: HashMap<String, (PublicIdentity, EventId, Option<EventId>, bool)>,
}
pub(in crate::node) struct ProjectionFacts {
    pub(super) profile: Arc<ProfileStorage>,
    pub(super) job: [u8; 16],
    pub(super) channel: ConversationId,
    source_revision: i64,
    source_job: [u8; 16],
}
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(value)
        .map_err(|e| invalid(&e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| invalid(&e.to_string()))
}
impl ProjectionFacts {
    pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
        if fresh {
            c.execute_batch(SCHEMA)?;
        }
        crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
        super::super::channel_account_authority::AuthorityHistory::initialize(c, fresh)
    }
    pub(super) fn load(
        profile: Arc<ProfileStorage>,
        job: [u8; 16],
        channel: ConversationId,
    ) -> Result<(MembershipReducer, i64), LogError> {
        Self::load_bound(profile, job, job, channel)
    }
    pub(super) fn load_bound(
        profile: Arc<ProfileStorage>,
        job: [u8; 16],
        source_job: [u8; 16],
        channel: ConversationId,
    ) -> Result<(MembershipReducer, i64), LogError> {
        let row:Option<(i64,Option<Vec<u8>>,Vec<u8>)>=profile.database().read(|c|
            c.query_row("SELECT next_rank,state,context FROM channel_projection_work WHERE job=?1 AND channel=?2",
                params![job.as_slice(),channel.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(Into::into))?;
        let source_revision=profile.database().read(|c| {
            let revision:i64=c.query_row("SELECT source_revision FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=0 AND job=?3",params![DOMAIN.as_slice(),channel.as_bytes().as_slice(),source_job.as_slice()],|r|r.get(0))?;
            super::graph::require_source_on(c,channel,source_job,revision)?;
            Ok(revision)
        })?;
        let mut reducer = MembershipReducer::new(channel);
        let next = if let Some((rank, state, context)) = row {
            let context: Context = decode(&context)?;
            reducer.prior = state
                .as_deref()
                .map(ChannelState::decode_current)
                .transpose()
                .map_err(|e| invalid(&e.to_string()))?;
            reducer.known_local = context.known_local;
            reducer.bindings = context.bindings;
            reducer.roots = context.roots;
            rank
        } else {
            0
        };
        reducer.authority = super::super::channel_account_authority::AuthorityHistory::load_bound(
            profile.clone(),
            job,
            source_job,
            channel,
        )?;
        reducer.history = Some(Arc::new(Self {
            profile,
            job,
            channel,
            source_revision,
            source_job,
        }));
        Ok((reducer, next))
    }
    fn guard_on(&self, c: &Connection) -> Result<(), LogError> {
        super::graph::require_source_on(c, self.channel, self.source_job, self.source_revision)
    }
    pub(super) fn is_authorized(&self, event: EventId) -> Result<bool, LogError> {
        self.profile.database().read(|c| { self.guard_on(c)?; c.query_row("SELECT EXISTS(SELECT 1 FROM channel_event_authority WHERE job=?1 AND channel=?2 AND event=?3 AND allowed=1)",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),event.as_bytes().as_slice()],|r|r.get(0)).map_err(Into::into) })
    }
    pub(super) fn parents_authorized(&self, event: EventId) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.guard_on(c)?;
            c.query_row("SELECT NOT EXISTS(SELECT 1 FROM retained_event_parents p LEFT JOIN channel_event_authority a ON a.job=?1 AND a.channel=?2 AND a.event=p.parent WHERE p.domain=?3 AND p.child=?4 AND (a.allowed IS NULL OR a.allowed=0))",
                params![self.job.as_slice(),self.channel.as_bytes().as_slice(),DOMAIN.as_slice(),event.as_bytes().as_slice()],|r|r.get(0)).map_err(Into::into)
        })
    }
    pub(super) fn departure_blocks(
        &self,
        member: &str,
        event: EventId,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, LogError> {
        let mut after: Option<Vec<u8>> = None;
        loop {
            let page:Vec<Vec<u8>>=self.profile.database().read(|c| {
                self.guard_on(c)?;
                let mut statement=c.prepare("SELECT event FROM channel_departure_facts WHERE job=?1 AND channel=?2 AND member=?3 AND (?4 IS NULL OR event>?4) ORDER BY event LIMIT 64")?;
                statement.query_map(params![self.job.as_slice(),self.channel.as_bytes().as_slice(),member,after],|r|r.get(0))?.collect::<Result<_,_>>().map_err(Into::into)
            })?;
            if page.is_empty() {
                return Ok(false);
            }
            for bytes in page {
                let id = EventId::new(
                    bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| invalid("invalid departure event identity"))?,
                );
                if !graph.descends(event, id)? {
                    return Ok(true);
                }
                after = Some(bytes);
            }
        }
    }
    pub(super) fn checkpoint_on(
        &self,
        c: &Connection,
        reducer: &mut MembershipReducer,
        header: &EventHeader,
        rank: i64,
        allowed: bool,
        own: &str,
    ) -> Result<bool, LogError> {
        let state = reducer.prior.as_mut();
        if let Some(state) = state {
            state.set_identity(own.to_owned());
            reducer.known_local |= state.is_member(own);
        }
        let accept = reducer
            .prior
            .as_ref()
            .and_then(ChannelState::accepted_event_id);
        let after_accept = if let Some(accept) = accept {
            header.id==accept || c.query_row("SELECT EXISTS(SELECT 1 FROM retained_event_parents p JOIN channel_event_authority a ON a.job=?1 AND a.channel=?2 AND a.event=p.parent AND a.accept_root=?4 AND a.after_accept=1 WHERE p.domain=?3 AND p.child=?5)",
                params![self.job.as_slice(),self.channel.as_bytes().as_slice(),DOMAIN.as_slice(),accept.as_bytes().as_slice(),header.id.as_bytes().as_slice()],|r|r.get(0))?
        } else {
            false
        };
        let receive = reducer.prior.as_ref().is_some_and(|state| {
            reducer.known_local
                && !state.is_dissolved()
                && !state.pending_invite()
                && state.is_accepted_member(&header.author.user_id())
                && header.author.user_id() != own
                && (accept.is_none() || after_accept)
        });
        c.execute(
            "INSERT INTO channel_event_authority VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                self.job.as_slice(),
                self.channel.as_bytes().as_slice(),
                header.id.as_bytes().as_slice(),
                rank,
                allowed,
                receive,
                accept.as_ref().map(|id| id.as_bytes().as_slice()),
                after_accept,
                header.kind as i64,
                header.author.ed25519_pub().as_slice()
            ],
        )?;
        for (member, event) in &reducer.departures {
            c.execute(
                "INSERT OR IGNORE INTO channel_departure_facts VALUES(?1,?2,?3,?4)",
                params![
                    self.job.as_slice(),
                    self.channel.as_bytes().as_slice(),
                    member,
                    event.as_bytes().as_slice()
                ],
            )?;
        }
        reducer.authority.checkpoint_on(c)?;
        reducer.authorized.clear();
        reducer.departures.clear();
        if let Some(state) = &reducer.prior {
            reducer.bindings.retain(|member, _| state.is_member(member));
        }
        let state = reducer
            .prior
            .as_ref()
            .map(ChannelState::encode_current)
            .transpose()
            .map_err(|e| invalid(&e.to_string()))?;
        let context = encode(&Context {
            known_local: reducer.known_local,
            bindings: reducer.bindings.clone(),
            roots: reducer.roots.clone(),
        })?;
        c.execute("INSERT INTO channel_projection_work VALUES(?1,?2,?3,?4,?5) ON CONFLICT(job,channel) DO UPDATE SET next_rank=excluded.next_rank,state=excluded.state,context=excluded.context",
            params![self.job.as_slice(),self.channel.as_bytes().as_slice(),rank.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?,state,context])?;
        if is_membership_control(header.kind)
            && let Some(state) = state
        {
            c.execute(
                "INSERT INTO channel_membership_snapshots VALUES(?1,?2,?3,?4)",
                params![
                    self.job.as_slice(),
                    self.channel.as_bytes().as_slice(),
                    rank,
                    state
                ],
            )?;
        }
        Ok(receive)
    }
}
pub(super) fn is_membership_control(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::MembershipChange
            | EventKind::ChannelRename
            | EventKind::ChannelDissolve
            | EventKind::ChannelAccept
            | EventKind::ChannelLeave
            | EventKind::ChannelOwnerTransfer
            | EventKind::ChannelAccountAuthority
            | EventKind::ChannelDeviceInheritance
    )
}
