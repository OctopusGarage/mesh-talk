//! Current metadata access never materializes historical applied/authorized IDs.
use super::*;
use crate::eventlog::repository::EventHeader;
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

    /// Install current presentation metadata only. Historical preference actors
    /// and activities remain on disk; RAM follows current members and channels.
    pub(in crate::node) fn install_book_metadata(
        &self,
        repository: &crate::eventlog::repository::EventRepository,
        me: &crate::identity::device::DeviceIdentity,
        book: &mut super::super::ChannelBook,
        state: ChannelState,
    ) -> Result<(), LogError> {
        let channel = state.id();
        let Some(source) = self.source_binding(channel)? else {
            return Err(LogError::StaleCursor);
        };
        let policy = crate::node::join_history::indexed_policy(self, repository, me, channel)?;
        let own = me.public().user_id();
        let (preferences,_departed,transferred)=self.profile.database().read(|c| {
            self.check_source_on(c,&source)?;
            let mut preferences=Vec::new();
            for member in state.members() {
                let member=member.user_id();
                let row:Option<(Vec<u8>,Vec<u8>,bool)>=c.query_row("SELECT lamport,event,enabled FROM channel_replica_preferences WHERE job=?1 AND channel=?2 AND member=?3",params![source.job.as_slice(),channel.as_bytes().as_slice(),&member],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                if let Some((lamport,event,enabled))=row {
                    preferences.push((member,u64::from_be_bytes(lamport.try_into().map_err(|_|invalid("invalid replica clock"))?),EventId::new(event.try_into().map_err(|_|invalid("invalid replica event"))?),enabled));
                }
            }
            let flags:(bool,bool)=c.query_row("SELECT EXISTS(SELECT 1 FROM channel_activities WHERE job=?1 AND channel=?2 AND actor=?3 AND kind='left'),EXISTS(SELECT 1 FROM channel_activities WHERE job=?1 AND channel=?2 AND actor=?3 AND kind='transferred')",params![source.job.as_slice(),channel.as_bytes().as_slice(),&own],|r|Ok((r.get(0)?,r.get(1)?)))?;
            Ok((preferences,flags.0,flags.1))
        })?;
        let departed = self
            .published_projection(channel)?
            .ok_or(LogError::StaleCursor)?
            .pending_departure(&own)?;
        book.install_indexed_state(state);
        book.history_policies.insert(channel, policy);
        book.replica_preferences.retain(|(id, _), _| *id != channel);
        for (member, lamport, event, enabled) in preferences {
            book.replica_preferences
                .insert((channel, member), (lamport, event, enabled));
        }
        book.departed.retain(|(id, _)| *id != channel);
        book.transferred.retain(|(id, _)| *id != channel);
        if departed {
            book.departed.insert((channel, own.clone()));
        }
        if transferred {
            book.transferred.insert((channel, own));
        }
        Ok(())
    }
    /// Canonical prefix membership is distinct from causal ancestry. These
    /// selected snapshots preserve that distinction without replaying controls.
    pub(in crate::node) fn canonical_state(
        &self,
        channel: ConversationId,
        event: EventId,
        include_event: bool,
    ) -> Result<Option<ChannelState>, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let binding: Option<(Vec<u8>, i64)> = c.query_row(
                "SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",
                params![channel.as_bytes().as_slice(),self.account.as_slice()],
                |row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            let Some((job,revision)) = binding else { return Ok(None); };
            let job: [u8;16] = job.try_into().map_err(|_|invalid("invalid channel current job"))?;
            replay::require_source_on(c, channel, job, revision)?;
            let rank: Option<i64> = c.query_row(
                "SELECT rank FROM channel_event_authority WHERE job=?1 AND channel=?2 AND event=?3",
                params![job.as_slice(),channel.as_bytes().as_slice(),event.as_bytes().as_slice()],
                |row| row.get(0)).optional()?;
            let Some(rank) = rank else { return Ok(None); };
            let through = if include_event { rank } else { rank - 1 };
            let bytes: Option<Vec<u8>> = c.query_row(
                "SELECT state FROM channel_membership_snapshots WHERE job=?1 AND channel=?2 AND rank<=?3 ORDER BY rank DESC LIMIT 1",
                params![job.as_slice(),channel.as_bytes().as_slice(),through],|row|row.get(0)).optional()?;
            bytes.map(|bytes| ChannelState::decode_current(&bytes).map_err(|error| invalid(&error.to_string()))).transpose()
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

    /// Exact selected-header admission for file/recall consumers. The source
    /// generation is checked inside the same read as its derived flag.
    pub(in crate::node) fn content_admitted(&self, header: &EventHeader) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let row:Option<(Vec<u8>,i64,bool)>=c.query_row("SELECT s.source_job,s.source_revision,a.receive FROM channel_current s JOIN channel_event_authority a ON a.job=s.source_job AND a.channel=s.channel AND a.event=?2 WHERE s.channel=?1 AND s.owner_account=?3",params![header.conversation.as_bytes().as_slice(),header.id.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((job,revision,admitted))=row else{return Ok(false);};
            replay::require_source_on(c,header.conversation,job.try_into().map_err(|_|invalid("invalid channel job"))?,revision)?;
            Ok(admitted)
        })
    }
}

#[derive(Clone, Copy)]
pub(in crate::node) struct ChannelSource {
    pub(in crate::node) channel: ConversationId,
    pub(in crate::node) job: [u8; 16],
    pub(in crate::node) revision: i64,
    account: [u8; 32],
}
impl ChannelCheckpointStore {
    pub(in crate::node) fn source_binding(
        &self,
        channel: ConversationId,
    ) -> Result<Option<ChannelSource>, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let row: Option<(Vec<u8>,i64)> = c.query_row("SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let source = row.map(|(job,revision)|Ok::<_,LogError>(ChannelSource {channel,job:job.try_into().map_err(|_|invalid("invalid channel source generation"))?,revision,account:self.account})).transpose()?;
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
        if source.account != self.account {
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
    pub(in crate::node) fn control_page(
        &self,
        source: &ChannelSource,
        kind: crate::eventlog::event::EventKind,
        after: Option<i64>,
        reverse: bool,
    ) -> Result<Vec<(i64, EventId)>, LogError> {
        self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            let sql=if reverse {
                "SELECT rank,event FROM channel_event_authority INDEXED BY channel_authority_kind WHERE job=?1 AND channel=?2 AND kind=?3 AND rank<?4 ORDER BY rank DESC LIMIT 64"
            }else{
                "SELECT rank,event FROM channel_event_authority INDEXED BY channel_authority_kind WHERE job=?1 AND channel=?2 AND kind=?3 AND rank>?4 ORDER BY rank LIMIT 64"
            };
            let mut statement=c.prepare(sql)?;
            let mut rows=statement.query(params![source.job.as_slice(),source.channel.as_bytes().as_slice(),kind as i64,after.unwrap_or(if reverse {i64::MAX}else{-1})])?;
            let mut page=Vec::new();
            while let Some(row)=rows.next()? {
                let id:Vec<u8>=row.get(1)?;
                page.push((row.get(0)?,EventId::new(id.try_into().map_err(|_|invalid("invalid channel control identity"))?)));
            }
            Ok(page)
        })
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
