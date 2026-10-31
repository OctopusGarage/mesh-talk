//! Current metadata access never materializes historical applied/authorized IDs.
use super::*;
impl ChannelCheckpointStore {
    /// Frozen history copies do not consume a sender chain. Their validated
    /// release identity still commits with transcript/effect installation.
    pub(in crate::node) fn install_history_receive_on(
        &self,
        c: &Connection,
        source: &ChannelSource,
        event: EventId,
    ) -> Result<bool, LogError> {
        self.check_source_on(c, source)?;
        if Self::processed_on(c, source.channel, event)? {
            return Ok(false);
        }
        c.execute(
            "INSERT INTO channel_processed VALUES(?1,?2,4)",
            params![
                source.channel.as_bytes().as_slice(),
                event.as_bytes().as_slice()
            ],
        )?;
        c.execute(
            "DELETE FROM channel_pending WHERE channel=?1 AND event=?2",
            params![
                source.channel.as_bytes().as_slice(),
                event.as_bytes().as_slice()
            ],
        )?;
        Ok(true)
    }
    pub(in crate::node) fn current_state(
        &self,
        channel: ConversationId,
    ) -> Result<Option<ChannelState>, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let row:Option<(Vec<u8>,i64,Vec<u8>)>=c.query_row("SELECT source_job,source_revision,state FROM channel_current WHERE channel=?1 AND owner_account=?2 AND known_local=1",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((job,revision,bytes))=row else{return Ok(None);};
            replay::require_source_on(c,channel,job.try_into().map_err(|_|invalid("invalid current channel job"))?,revision)?;
            ChannelState::decode_current(&bytes).map(Some).map_err(|e|invalid(&e.to_string()))
        })
    }

    /// Canonical prefix membership is distinct from causal ancestry. These
    /// selected snapshots preserve that distinction without replaying controls.
    pub(in crate::node) fn canonical_state(
        &self,
        channel: ConversationId,
        event: EventId,
        include_event: bool,
    ) -> Result<Option<ChannelState>, LogError> {
        Ok(self
            .canonical_state_budgeted(channel, event, include_event, usize::MAX)?
            .0)
    }
    pub(in crate::node) fn canonical_state_budgeted(
        &self,
        channel: ConversationId,
        event: EventId,
        include_event: bool,
        budget: usize,
    ) -> Result<(Option<ChannelState>, usize), LogError> {
        if budget < 64 {
            return Err(LogError::PageBudgetTooSmall { required: 64 });
        }
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let binding: Option<(Vec<u8>, i64)> = c.query_row(
                "SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",
                params![channel.as_bytes().as_slice(),self.account.as_slice()],
                |row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            let Some((job,revision)) = binding else { return Ok((None,0)); };
            let job: [u8;16] = job.try_into().map_err(|_|invalid("invalid channel current job"))?;
            replay::require_source_on(c, channel, job, revision)?;
            let rank: Option<i64> = c.query_row(
                "SELECT rank FROM channel_event_authority WHERE job=?1 AND channel=?2 AND event=?3",
                params![job.as_slice(),channel.as_bytes().as_slice(),event.as_bytes().as_slice()],
                |row| row.get(0)).optional()?;
            let Some(rank) = rank else { return Ok((None,0)); };
            let through = if include_event { rank } else { rank - 1 };
            let selected:Option<(i64,i64)>=c.query_row("SELECT rank,length(state) FROM channel_membership_snapshots WHERE job=?1 AND channel=?2 AND rank<=?3 ORDER BY rank DESC LIMIT 1",params![job.as_slice(),channel.as_bytes().as_slice(),through],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((rank,length))=selected else{return Ok((None,64));};
            let required=usize::try_from(length).ok().and_then(|n|n.checked_mul(3)).and_then(|n|n.checked_add(64)).ok_or(LogError::RepositoryCounterExhausted)?;
            if required>budget {return Err(LogError::PageBudgetTooSmall {required});}
            let bytes:Vec<u8>=c.query_row("SELECT state FROM channel_membership_snapshots WHERE job=?1 AND channel=?2 AND rank=?3",params![job.as_slice(),channel.as_bytes().as_slice(),rank],|r|r.get(0))?;
            let state=ChannelState::decode_current(&bytes).map_err(|error|invalid(&error.to_string()))?;
            Ok((Some(state),required))
        })
    }
    pub(in crate::node) fn published_projection(
        &self,
        channel: ConversationId,
    ) -> Result<Option<super::super::ChannelMembershipProjection>, LogError> {
        let binding:Option<(Vec<u8>,i64,i64)>=self.profile.database().read(|c| {
            self.check_account_on(c)?;
            c.query_row("SELECT source_job,source_revision,next_rank FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(Into::into)
        })?;
        let Some((job, revision, count)) = binding else {
            return Ok(None);
        };
        let job: [u8; 16] = job
            .try_into()
            .map_err(|_| invalid("invalid channel current job"))?;
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c, channel, job, revision)
        })?;
        let (reducer, next) = super::super::projection_facts::ProjectionFacts::load(
            self.profile.clone(),
            job,
            channel,
        )?;
        if next != count {
            return Err(LogError::StaleCursor);
        }
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c, channel, job, revision)
        })?;
        Ok(Some(reducer.finish()))
    }
    /// Per-source work remains on disk; only one current conversation is held
    /// while the scheduler advances its canonical reducer.
    #[cfg(test)]
    pub(in crate::node) fn next_dirty(&self) -> Result<Option<ConversationId>, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let id: Option<Vec<u8>> = c
                .query_row(
                    "SELECT d.channel FROM channel_dirty d JOIN event_conversations e ON e.domain=?1 AND e.conversation=d.channel AND e.lifecycle=0 ORDER BY d.channel LIMIT 1",
                    [b"MeshPartEventsV1".as_slice()],
                    |r| r.get(0),
                )
                .optional()?;
            id.map(|id| {
                Ok(ConversationId::new(
                    id.try_into()
                        .map_err(|_| invalid("invalid dirty channel"))?,
                ))
            })
            .transpose()
        })
    }
    pub(in crate::node) fn content_visible(
        &self,
        channel: ConversationId,
        event: EventId,
    ) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            if !self.history_read_ready_on(c, channel)? {
                return Err(LogError::StaleCursor);
            }
            let row:Option<(Vec<u8>,i64,Vec<u8>)>=c.query_row("SELECT source_job,source_revision,state FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((job,revision,bytes))=row else {return Ok(true);};
            let job:[u8;16]=job.try_into().map_err(|_|invalid("invalid channel source job"))?;
            replay::require_source_on(c,channel,job,revision)?;
            let state=ChannelState::decode_current(&bytes).map_err(|error|invalid(&error.to_string()))?;
            if state.pending_invite() {return Ok(false);}
            let Some(accept)=state.accepted_event_id() else {return Ok(true);};
            c.query_row("SELECT EXISTS(SELECT 1 FROM channel_event_authority WHERE job=?1 AND channel=?2 AND event=?3 AND accept_root=?4 AND after_accept=1)",params![job.as_slice(),channel.as_bytes().as_slice(),event.as_bytes().as_slice(),accept.as_bytes().as_slice()],|r|r.get(0)).map_err(Into::into)
        })
    }

    /// A completed membership projection is not yet safe for plaintext reads
    /// while newly admitted recalls still await their first attempt. Dormant
    /// missing-prerequisite retries remain nonblocking until their exact wake.
    pub(in crate::node) fn history_read_ready(
        &self,
        channel: ConversationId,
    ) -> Result<bool, LogError> {
        self.profile
            .database()
            .read(|c| self.history_read_ready_on(c, channel))
    }

    fn history_read_ready_on(
        &self,
        c: &Connection,
        channel: ConversationId,
    ) -> Result<bool, LogError> {
        self.check_account_on(c)?;
        let binding: Option<(Vec<u8>, i64)> = c.query_row(
            "SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",
            params![channel.as_bytes().as_slice(),self.account.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let Some((job, revision)) = binding else {
            // Retained signed metadata identifies a channel even before its
            // first checkpoint. A presentation cache cannot classify it as DM.
            return c.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM retained_events INDEXED BY retained_events_kind_author WHERE domain=?1 AND conversation=?2 AND kind=?3)",
                params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),crate::eventlog::EventKind::MembershipChange as i64],
                |row| row.get(0),
            ).map_err(Into::into);
        };
        let job = job
            .try_into()
            .map_err(|_| invalid("invalid channel source job"))?;
        match replay::require_source_on(c, channel, job, revision) {
            Ok(()) => {}
            Err(LogError::StaleCursor) => return Ok(false),
            Err(error) => return Err(error),
        }
        c.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM channel_pending INDEXED BY channel_pending_new WHERE channel=?1 AND kind=?2 AND reason=0 AND admitted=1) AND NOT EXISTS(SELECT 1 FROM channel_recall_wakes WHERE channel=?1)",
            params![channel.as_bytes().as_slice(),crate::eventlog::EventKind::Delete as i64],
            |row| row.get(0),
        ).map_err(Into::into)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::node) struct ChannelSource {
    pub(in crate::node) channel: ConversationId,
    pub(in crate::node) job: [u8; 16],
    pub(in crate::node) revision: i64,
    account: [u8; 32],
    generation: i64,
}
impl ChannelCheckpointStore {
    pub(in crate::node) fn source_binding(
        &self,
        channel: ConversationId,
    ) -> Result<Option<ChannelSource>, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let row: Option<(Vec<u8>,i64)> = c.query_row("SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let source = row.map(|(job,revision)|Ok::<_,LogError>(ChannelSource {channel,job:job.try_into().map_err(|_|invalid("invalid channel source generation"))?,revision,account:self.account,generation:self.generation})).transpose()?;
            if let Some(source)=&source {self.check_source_on(c,source)?;}
            Ok(source)
        })
    }
    pub(in crate::node) fn check_source_on(
        &self,
        c: &Connection,
        source: &ChannelSource,
    ) -> Result<(), LogError> {
        self.check_account_on(c)?;
        if source.account != self.account || source.generation != self.generation {
            return Err(LogError::StaleCursor);
        }
        let current:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM channel_current WHERE channel=?1 AND source_job=?2 AND source_revision=?3 AND owner_account=?4)",params![source.channel.as_bytes().as_slice(),source.job.as_slice(),source.revision,self.account.as_slice()],|r|r.get(0))?;
        if !current {
            return Err(LogError::StaleCursor);
        }
        replay::require_source_on(c, source.channel, source.job, source.revision)
    }
    pub(in crate::node) fn check_source(&self, source: &ChannelSource) -> Result<(), LogError> {
        self.profile
            .database()
            .read(|c| self.check_source_on(c, source))
    }
    /// Ordinary messages do not invalidate current policy/permission summaries.
    /// Every control that can change membership or history does invalidate them.
    pub(in crate::node) fn control_stamp(&self, source: &ChannelSource) -> Result<i64, LogError> {
        use crate::eventlog::event::EventKind::*;
        self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            let mut stamp=-1;
            for kind in [MembershipChange,ChannelRename,ChannelDissolve,ChannelOwnerTransfer,ChannelLeave,ChannelAccept,ChannelHistoryPolicy,ChannelJoinHistory,ChannelAccountAuthority,ChannelDeviceInheritance] {
                let rank:Option<i64>=c.query_row("SELECT rank FROM channel_event_authority INDEXED BY channel_authority_kind WHERE job=?1 AND channel=?2 AND kind=?3 ORDER BY rank DESC LIMIT 1",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),kind as i64],|r|r.get(0)).optional()?;
                stamp=stamp.max(rank.unwrap_or(-1));
            }
            Ok(stamp)
        })
    }
}
