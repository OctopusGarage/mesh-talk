//! On-demand history authority over the channel's indexed canonical and causal
//! states. Only selected controls are decoded; unchanged policy is scalar state.
use super::*;
use crate::{
    eventlog::{
        LogError,
        repository::{EventHeader, EventRepository},
    },
    node::channel::checkpoint::{ChannelCheckpointStore, ChannelSource},
};
use rusqlite::{Connection, OptionalExtension, params};

mod authority;
mod preparation;
pub(in crate::node) use preparation::{OwnerAuthorizationQuery, OwnerAuthorizationStep};
mod controls;
mod receive;
mod send;
pub(super) use controls::install_on;

const SCHEMA: &str = "CREATE TABLE channel_history_policy_cache(job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),control_stamp INTEGER NOT NULL,policy BLOB NOT NULL,PRIMARY KEY(job,channel)) WITHOUT ROWID;";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    controls::initialize(c, fresh)?;
    send::initialize(c, fresh)
}
pub(in crate::node) struct IndexedView<'a> {
    checkpoints: &'a ChannelCheckpointStore,
    repository: &'a EventRepository,
    me: &'a DeviceIdentity,
    source: ChannelSource,
}
impl<'a> IndexedView<'a> {
    pub(in crate::node) fn new(
        checkpoints: &'a ChannelCheckpointStore,
        repository: &'a EventRepository,
        me: &'a DeviceIdentity,
        source: ChannelSource,
    ) -> Self {
        Self {
            checkpoints,
            repository,
            me,
            source,
        }
    }

    fn before(&self, event: EventId) -> Result<Option<crate::channel::ChannelState>, LogError> {
        self.checkpoints.check_source(&self.source)?;
        let state =
            self.checkpoints
                .causal_state(self.repository, self.me, self.source.channel, event)?;
        self.checkpoints.check_source(&self.source)?;
        Ok(state)
    }
    fn canonical(
        &self,
        event: EventId,
        include: bool,
    ) -> Result<Option<crate::channel::ChannelState>, LogError> {
        self.checkpoints.check_source(&self.source)?;
        let state = self
            .checkpoints
            .canonical_state(self.source.channel, event, include)?;
        self.checkpoints.check_source(&self.source)?;
        Ok(state)
    }
    fn owner_authorized(&self, event: &EventHeader) -> Result<bool, LogError> {
        let author = event.author.user_id();
        let Some(current) = self.canonical(event.id, false)? else {
            return Ok(false);
        };
        let Some(causal) = self.before(event.id)? else {
            return Ok(false);
        };
        if !current.can_admin(&author)
            || current.is_dissolved()
            || !causal.can_admin(&author)
            || causal.is_dissolved()
        {
            return Ok(false);
        }
        // A later loss of ownership closes unobserved controls in that lifetime,
        // even when a malicious timestamp puts them earlier in canonical order.
        let event_rank = self.checkpoints.profile().database().read(|c| {
            self.checkpoints.check_source_on(c, &self.source)?;
            c.query_row(
                "SELECT rank FROM channel_event_authority WHERE job=?1 AND channel=?2 AND event=?3",
                params![
                    self.source.job.as_slice(),
                    self.source.channel.as_bytes().as_slice(),
                    event.id.as_bytes().as_slice()
                ],
                |r| r.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })?;
        for kind in [
            EventKind::ChannelOwnerTransfer,
            EventKind::ChannelAccountAuthority,
        ] {
            let mut after = Some(event_rank);
            loop {
                let page = self
                    .checkpoints
                    .control_page(&self.source, kind, after, false)?;
                if page.is_empty() {
                    break;
                }
                for (rank, id) in page {
                    after = Some(rank);
                    if self
                        .canonical(id, false)?
                        .is_some_and(|state| state.can_admin(&author))
                        && self
                            .canonical(id, true)?
                            .is_some_and(|state| !state.can_admin(&author))
                        && !self
                            .repository
                            .descends_from(self.source.channel, id, event.id)?
                    {
                        return Ok(false);
                    }
                }
            }
        }
        self.checkpoints.check_source(&self.source)?;
        Ok(true)
    }
    fn policy(&self, cutoff: Option<EventId>) -> Result<HistoryPolicy, LogError> {
        let mut after = None;
        loop {
            let page = self.checkpoints.control_page(
                &self.source,
                EventKind::ChannelHistoryPolicy,
                after,
                true,
            )?;
            if page.is_empty() {
                return Ok(HistoryPolicy::default());
            }
            for (rank, id) in page {
                after = Some(rank);
                if let Some(cutoff) = cutoff
                    && !self
                        .repository
                        .descends_from(self.source.channel, cutoff, id)?
                {
                    continue;
                }
                let header = self.repository.header(&id)?.ok_or_else(|| {
                    LogError::CorruptFile("history policy header disappeared".into())
                })?;
                if !self.owner_authorized(&header)? {
                    continue;
                }
                let event = self
                    .repository
                    .get(&id)?
                    .ok_or_else(|| LogError::CorruptFile("history policy disappeared".into()))?;
                if let Some(policy) = decode(&event.ciphertext) {
                    return Ok(policy);
                }
            }
        }
    }
}
pub(super) fn current_policy(
    checkpoints: &ChannelCheckpointStore,
    repository: &EventRepository,
    me: &DeviceIdentity,
    channel: ConversationId,
) -> Result<HistoryPolicy, LogError> {
    let Some(source) = checkpoints.source_binding(channel)? else {
        return Ok(HistoryPolicy::default());
    };
    let stamp = checkpoints.control_stamp(&source)?;
    let cached:Option<Vec<u8>>=checkpoints.profile().database().read(|c| {
        checkpoints.check_source_on(c,&source)?;
        c.query_row("SELECT policy FROM channel_history_policy_cache WHERE job=?1 AND channel=?2 AND control_stamp=?3",params![source.job.as_slice(),channel.as_bytes().as_slice(),stamp],|r|r.get(0)).optional().map_err(Into::into)
    })?;
    if let Some(bytes) = cached {
        return decode(&bytes)
            .ok_or_else(|| LogError::CorruptFile("invalid current channel history policy".into()));
    }
    let policy = IndexedView {
        checkpoints,
        repository,
        me,
        source,
    }
    .policy(None)?;
    checkpoints.profile().database().transaction(|c| {
        checkpoints.check_source_on(c,&source)?;
        c.execute("INSERT INTO channel_history_policy_cache VALUES(?1,?2,?3,?4) ON CONFLICT(job,channel) DO UPDATE SET control_stamp=excluded.control_stamp,policy=excluded.policy",params![source.job.as_slice(),channel.as_bytes().as_slice(),stamp,encode(&policy)])?;
        Ok(())
    })?;
    Ok(policy)
}
