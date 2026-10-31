//! Bounded authority advancement. A reordered control replaces only derived
//! authority facts; consumed receiver checkpoints never participate in replay.
use super::*;
use crate::{
    eventlog::{
        event::EventKind,
        repository::{
            EventRepository,
            order::{CausalOrder, OrderReadState, OrderReadiness, OrderWorkBudget},
            pages::PageBudget,
        },
    },
    identity::device::DeviceIdentity,
    node::channel::{
        graph::{AuthorityGraph, preparing::PreparingGraph},
        projection_facts::{ProjectionFacts, is_membership_control},
    },
};

const ROWS: usize = 64;
const SCHEMA:&str="
CREATE TABLE channel_source_scan(singleton INTEGER PRIMARY KEY CHECK(singleton=1),ordinal INTEGER NOT NULL CHECK(ordinal>=-1));
CREATE TABLE channel_dirty(channel BLOB PRIMARY KEY CHECK(length(channel)=32)) WITHOUT ROWID;
CREATE TABLE channel_replica_preferences(job BLOB NOT NULL,channel BLOB NOT NULL,member TEXT NOT NULL,lamport BLOB NOT NULL CHECK(length(lamport)=8),event BLOB NOT NULL CHECK(length(event)=32),enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),PRIMARY KEY(job,channel,member)) WITHOUT ROWID;
";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
        c.execute("INSERT INTO channel_source_scan VALUES(1,-1)", [])?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}

pub(super) use crate::node::channel::graph::require_source_on;

impl ChannelCheckpointStore {
    /// Discover at most one committed metadata page. Even a retired/non-channel
    /// row advances the cursor, so a large unrelated history cannot force a scan.
    pub(in crate::node) fn discover_step(
        &self,
        repository: &EventRepository,
    ) -> Result<bool, LogError> {
        let after: i64 = self.profile.database().read(|c| {
            self.check_account_on(c)?;
            c.query_row(
                "SELECT ordinal FROM channel_source_scan WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })?;
        let page = repository.committed_headers_page((after >= 0).then_some(after), 128)?;
        let Some(last) = page.last().map(|row| row.ordinal) else {
            return Ok(false);
        };
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            for row in &page {
                if !row.active { continue; }
                let known:bool=row.header.kind==EventKind::MembershipChange || c.query_row("SELECT EXISTS(SELECT 1 FROM channel_current WHERE channel=?1) OR EXISTS(SELECT 1 FROM channel_dirty WHERE channel=?1)",[row.header.conversation.as_bytes().as_slice()],|r|r.get(0))?;
                if known { c.execute("INSERT OR IGNORE INTO channel_dirty VALUES(?1)",[row.header.conversation.as_bytes().as_slice()])?; }
            }
            if c.execute("UPDATE channel_source_scan SET ordinal=?2 WHERE singleton=1 AND ordinal=?1",params![after,last])?!=1 { return Err(LogError::StaleCursor); }
            Ok(())
        })?;
        Ok(true)
    }

    /// One canonical header page and at most one decoded control at a time.
    /// `false` asks the runtime to schedule another bounded work turn.
    pub(in crate::node) fn project_step(
        &self,
        repository: &EventRepository,
        me: &DeviceIdentity,
        channel: ConversationId,
    ) -> Result<bool, LogError> {
        if repository
            .order_step(&channel, CausalOrder::Logical, OrderWorkBudget::default())?
            .readiness
            != OrderReadiness::Ready
        {
            return Ok(false);
        }
        let Some(generation) = repository.order_generation(&channel, CausalOrder::Logical)? else {
            return Ok(true);
        };
        let count =
            i64::try_from(generation.count).map_err(|_| LogError::RepositoryCounterExhausted)?;
        let already:bool=self.profile.database().read(|c| {
            self.check_account_on(c)?;
            c.query_row("SELECT EXISTS(SELECT 1 FROM channel_current WHERE channel=?1 AND source_job=?2 AND source_revision=?3 AND next_rank=?4 AND owner_account=?5)",
                params![channel.as_bytes().as_slice(),generation.job.as_slice(),generation.source_revision,count,self.account.as_slice()],|r|r.get(0)).map_err(Into::into)
        })?;
        if already {
            // Discovery can catch up after an explicit consumer has projected
            // this source. Retire that redundant work under the same revision
            // fence, so an append cannot lose its newly queued work.
            self.profile.database().transaction(|c| {
                self.check_account_on(c)?;
                require_source_on(c, channel, generation.job, generation.source_revision)?;
                c.execute(
                    "DELETE FROM channel_dirty WHERE channel=?1",
                    [channel.as_bytes().as_slice()],
                )?;
                Ok(())
            })?;
            return Ok(true);
        }
        if !self.prepare_legacy_departures(
            repository,
            channel,
            generation.job,
            generation.source_revision,
            count,
        )? {
            return Ok(false);
        }
        let (mut reducer, mut next) =
            ProjectionFacts::load(self.profile.clone(), generation.job, channel)?;
        if next == 0 && self.cleanup_generation_step(channel, generation.job)? {
            return Ok(false);
        }
        if next > count {
            return Err(invalid("channel reducer cursor exceeds source"));
        }
        let own = me.public().user_id();
        if next < count {
            let OrderReadState::Ready(page) = repository.causal_headers_after_rank(
                &channel,
                CausalOrder::Logical,
                generation.job,
                next - 1,
                PageBudget {
                    rows: 1,
                    bytes: 64 * 1024,
                },
            )?
            else {
                return Ok(false);
            };
            for header in page.items {
                let graph = PreparingGraph::open(
                    self.profile.clone(),
                    repository,
                    crate::node::channel::graph::preparing::DecisionBinding {
                        job: generation.job,
                        source_job: generation.job,
                        revision: generation.source_revision,
                        channel,
                        event: header.id,
                    },
                    crate::eventlog::sync::network::PreparationBudget {
                        rows: 63,
                        edges: 512,
                        bytes: 8 * 1024 * 1024 - 256,
                    },
                )?;
                if !graph.prepare()? {
                    return Ok(false);
                }
                let processed = self
                    .profile
                    .database()
                    .read(|c| Self::processed_on(c, channel, header.id))?;
                let needs_crypto = !processed
                    && matches!(
                        header.kind,
                        EventKind::KeyRotation | EventKind::ChannelDeviceInheritance
                    );
                let event = if is_membership_control(header.kind)
                    || needs_crypto
                    || matches!(
                        header.kind,
                        EventKind::ChannelReplica | EventKind::ChannelJoinHistory
                    ) {
                    let event = graph.event(header.id)?;
                    if graph.pending() {
                        return Ok(false);
                    }
                    Some(
                        event
                            .ok_or_else(|| invalid("channel control disappeared"))?
                            .into_owned(),
                    )
                } else {
                    None
                };
                let activity_before = super::activity::Before::capture(&reducer, &header);
                let allowed = if is_membership_control(header.kind) {
                    reducer.step(
                        event
                            .as_ref()
                            .ok_or_else(|| invalid("missing channel control"))?,
                        &graph,
                    )?
                } else {
                    reducer.step_header(&header, &graph)?
                };
                if graph.pending() {
                    return Ok(false);
                }
                // Open authenticated control payloads outside the database mutex.
                // Only immutable preparation occurs before the guarded commit.
                let mut distributions = Vec::new();
                if needs_crypto
                    && let (Some(event), Some(state)) = (event.as_ref(), reducer.prior.as_ref())
                {
                    if event.kind == EventKind::KeyRotation
                        && !state.is_dissolved()
                        && state.is_accepted_member(&event.author.user_id())
                        && let Some(sealed) = super::super::SealedKeys::decode(&event.ciphertext)
                        && let Some(author) = state
                            .members()
                            .iter()
                            .find(|p| p.user_id() == event.author.user_id())
                        && let Some(distribution) =
                            super::super::open_my_skd(me, &author.x25519_pub, &sealed)
                    {
                        distributions.push((
                            crate::node::channel_account_bootstrap::ReceiveDistribution {
                                author: author.clone(),
                                epoch: sealed.epoch,
                                distribution,
                            },
                            false,
                        ));
                    }
                    if event.kind == EventKind::ChannelDeviceInheritance {
                        for distribution in
                            crate::node::channel_account_bootstrap::open_receive_distributions(
                                me,
                                state,
                                event,
                                &reducer.authority,
                                &graph,
                            )?
                        {
                            distributions.push((distribution, true));
                        }
                    }
                }
                if graph.pending() {
                    return Ok(false);
                }
                let activity = activity_before.finish(&reducer, &header, event.as_ref(), &own);
                let facts = reducer
                    .history
                    .as_ref()
                    .ok_or_else(|| invalid("missing channel projection owner"))?
                    .clone();
                let after_accept = facts.after_accept(&reducer, &header, &graph)?;
                if graph.pending() {
                    return Ok(false);
                }
                self.profile.database().transaction(|c| {
                    self.check_account_on(c)?;
                    require_source_on(c,channel,generation.job,generation.source_revision)?;
                    let actual:i64=c.query_row("SELECT COALESCE((SELECT next_rank FROM channel_projection_work WHERE job=?1 AND channel=?2),0)",params![generation.job.as_slice(),channel.as_bytes().as_slice()],|r|r.get(0))?;
                    if actual!=next {return Err(LogError::StaleCursor);}
                    for (distribution,refresh) in &distributions {
                        let author=distribution.author.user_id();
                        let epoch=distribution.epoch;
                        let distribution=&distribution.distribution;
                        if *refresh { Self::refresh_distribution_on(c,channel,&author,epoch,distribution)?; }
                        else { Self::install_distribution_on(c,channel,&author,epoch,distribution)?; }
                    }
                    if needs_crypto && !distributions.is_empty() { c.execute("INSERT OR IGNORE INTO channel_processed VALUES(?1,?2,2)",params![channel.as_bytes().as_slice(),header.id.as_bytes().as_slice()])?; }
                    super::activity::install_on(c,generation.job,channel,header.id,next,activity.as_ref())?;
                    if let Some(event)=&event {
                        super::sharing::install_on(c,generation.job,event,allowed,&own)?;
                        crate::node::join_history::index_control_on(c,generation.job,channel,event,next)?;
                    }
                    if header.kind == EventKind::ChannelReplica
                        && reducer.prior.as_ref().is_some_and(|state| !state.is_dissolved() && state.is_accepted_member(&header.author.user_id()))
                        && let Some(event)=&event
                        && let [enabled @ 0..=1]=event.ciphertext.as_slice()
                    {
                        c.execute("INSERT INTO channel_replica_preferences VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(job,channel,member) DO UPDATE SET lamport=excluded.lamport,event=excluded.event,enabled=excluded.enabled WHERE (excluded.lamport,excluded.event)>(lamport,event)",params![generation.job.as_slice(),channel.as_bytes().as_slice(),header.author.user_id(),header.lamport.to_be_bytes().as_slice(),header.id.as_bytes().as_slice(),*enabled==1])?;
                    }
                    super::recall::wake_on(c,channel,header.id)?;
                    let admitted=facts.checkpoint_on(c,&mut reducer,&header,next,(allowed,after_accept),&own)?;
                    graph.complete_on(c)?;
                    if matches!(header.kind,EventKind::Message|EventKind::FileManifest|EventKind::Delete|EventKind::ChannelDeviceInheritance) && !processed {
                        c.execute("INSERT INTO channel_pending(channel,event,ordinal,reason,author,kind,admitted) VALUES(?1,?2,?3,?7,?4,?5,?6) ON CONFLICT(channel,event) DO UPDATE SET ordinal=excluded.ordinal,admitted=excluded.admitted,reason=CASE WHEN channel_pending.attempt_job<>?8 THEN excluded.reason ELSE channel_pending.reason END",params![channel.as_bytes().as_slice(),header.id.as_bytes().as_slice(),next,header.author.user_id(),header.kind as i64,admitted,0,generation.job.as_slice()])?;
                    }
                    Ok(())
                })?;
                next += 1;
            }
        }
        if next < count {
            return Ok(false);
        }
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            require_source_on(c, channel, generation.job, generation.source_revision)?;
            if let Some(state) = &reducer.prior {
                Self::install_current_on(
                    c,
                    state,
                    generation.job,
                    generation.source_revision,
                    next,
                    &self.account,
                    reducer.known_local,
                )?;
            }
            c.execute(
                "DELETE FROM channel_dirty WHERE channel=?1",
                [channel.as_bytes().as_slice()],
            )?;
            Ok(())
        })?;
        Ok(true)
    }

    fn refresh_distribution_on(
        c: &Connection,
        channel: ConversationId,
        author: &str,
        epoch: u64,
        distribution: &SenderKeyDistribution,
    ) -> Result<(), LogError> {
        let bytes:Option<Vec<u8>>=c.query_row("SELECT checkpoint FROM channel_receivers WHERE channel=?1 AND author=?2 AND epoch=?3",params![channel.as_bytes().as_slice(),author,epoch.to_be_bytes().as_slice()],|r|r.get(0)).optional()?;
        let Some(bytes) = bytes.map(Zeroizing::new) else {
            return Self::install_distribution_on(c, channel, author, epoch, distribution);
        };
        let candidate = ChannelState::refreshed_receiver_checkpoint(&bytes, distribution)
            .map_err(|e| invalid(&e.to_string()))?;
        let mut distribution = ChannelState::checkpoint_distribution(&candidate)
            .map_err(|e| invalid(&e.to_string()))?;
        let position = distribution.n;
        zeroize::Zeroize::zeroize(&mut distribution.chain_key);
        if candidate.as_slice() != bytes.as_slice()
            && c.execute("UPDATE channel_receivers SET revision=revision+1,key_revision=key_revision+1,checkpoint=?4,next_n=?5 WHERE channel=?1 AND author=?2 AND epoch=?3 AND revision<9223372036854775807 AND key_revision<9223372036854775807",params![channel.as_bytes().as_slice(),author,epoch.to_be_bytes().as_slice(),candidate.as_slice(),i64::from(position)])?!=1 {
            return Err(LogError::RepositoryCounterExhausted);
        }
        let revision: i64 = c.query_row(
            "SELECT key_revision FROM channel_receivers WHERE channel=?1 AND author=?2 AND epoch=?3",
            params![
                channel.as_bytes().as_slice(),
                author,
                epoch.to_be_bytes().as_slice()
            ],
            |r| r.get(0),
        )?;
        Self::wake_chain_on(c, channel, author, epoch, revision, position)?;
        Ok(())
    }
}

impl ChannelCheckpointStore {
    /// Validate one bounded header page and at most one legacy parent cut per
    /// turn. Candidate facts are independent of canonical membership replay.
    fn prepare_legacy_departures(
        &self,
        repository: &EventRepository,
        channel: ConversationId,
        job: [u8; 16],
        revision: i64,
        count: i64,
    ) -> Result<bool, LogError> {
        let retired=self.profile.database().transaction(|c| {
            self.check_account_on(c)?;require_source_on(c,channel,job,revision)?;
            let old:Option<(Vec<u8>,Vec<u8>,Vec<u8>)>=c.query_row("SELECT source_job,selection,query FROM channel_legacy_cuts WHERE channel=?1 AND source_job<>?2 LIMIT 1",params![channel.as_bytes().as_slice(),job.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((source,selection,query))=old else{return Ok(false);};
            c.execute("UPDATE event_ancestry_queries SET retired=1 WHERE query=?1",[&selection])?;
            c.execute("DELETE FROM event_ancestry_selection WHERE query=?1",[&selection])?;
            c.execute("UPDATE channel_projection_queries SET retired=1 WHERE job=?1",[&query])?;
            c.execute("DELETE FROM channel_legacy_cuts WHERE source_job=?1 AND channel=?2",params![source,channel.as_bytes().as_slice()])?;
            Ok(true)
        })?;
        if retired || self.cleanup_generation_step(channel, job)? {
            return Ok(false);
        }
        let (next, rebuilding): (i64, bool) = self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            require_source_on(c, channel, job, revision)?;
            c.execute(
                "INSERT OR IGNORE INTO channel_legacy_scan VALUES(?1,?2,0,0)",
                params![job.as_slice(), channel.as_bytes().as_slice()],
            )?;
            c.query_row(
                "SELECT next_rank,rebuilding FROM channel_legacy_scan WHERE job=?1 AND channel=?2",
                params![job.as_slice(), channel.as_bytes().as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        if rebuilding {
            self.profile.database().transaction(|c| {
                self.check_account_on(c)?;
                require_source_on(c,channel,job,revision)?;
                for (table,key) in [
                    ("channel_event_authority","event"),("channel_departure_facts","member,event"),
                    ("channel_membership_snapshots","rank"),("channel_causal_states","event"),
                    ("channel_activities","event"),("channel_replica_preferences","member"),
                    ("channel_history_policy_cache","channel"),("channel_history_controls","event"),
                    ("channel_history_pending","event"),("channel_history_scan","channel"),
                    ("channel_history_send_scan","channel"),
                    ("channel_receive_shipments","recipient_ed,recipient_x,grant_account,grant_incarnation,epoch,author"),
                    ("channel_authority_facts","kind,key"),("channel_authority_current","channel"),
                    ("channel_projection_work","channel")
                ] {
                    let sql=format!("DELETE FROM {table} WHERE job=?1 AND channel=?2 AND ({key}) IN (SELECT {key} FROM {table} WHERE job=?1 AND channel=?2 LIMIT 128)");
                    if c.execute(&sql,params![job.as_slice(),channel.as_bytes().as_slice()])?!=0 { return Ok(()); }
                }
                c.execute("UPDATE channel_legacy_scan SET rebuilding=0 WHERE job=?1 AND channel=?2",params![job.as_slice(),channel.as_bytes().as_slice()])?;
                Ok(())
            })?;
            return Ok(false);
        }
        if next == count {
            return Ok(true);
        }
        let OrderReadState::Ready(page) = repository.causal_headers_after_rank(
            &channel,
            CausalOrder::Logical,
            job,
            next - 1,
            PageBudget {
                rows: 64,
                bytes: 64 * 1024,
            },
        )?
        else {
            return Ok(false);
        };
        let mut through = next;
        let mut candidate = None;
        for header in page.items {
            through += 1;
            if header.kind != EventKind::ChannelLeave || header.ciphertext_bytes != 0 {
                continue;
            }
            let event = repository
                .get(&header.id)?
                .ok_or_else(|| invalid("legacy departure disappeared"))?;
            if !event.is_canonical() || !event.verify_integrity() || !event.verify_signature() {
                continue;
            }
            match self.legacy_cut_step(repository, &event, job, revision)? {
                super::causal::LegacyCut::Pending => {
                    through -= 1;
                }
                super::causal::LegacyCut::Ready(value) => {
                    candidate = value.map(|value| (value.event, value.member));
                }
            }
            break;
        }
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            require_source_on(c,channel,job,revision)?;
            if let Some((event,member))=&candidate {
                use bincode::Options;
                let member_bytes=bincode::DefaultOptions::new().with_fixint_encoding().serialize(member).map_err(|e|invalid(&e.to_string()))?;
                let lifetime=bincode::DefaultOptions::new().with_fixint_encoding().serialize(&(member.membership_event,member.acceptance_event,member.public.ed25519_pub,member.public.x25519_pub)).map_err(|e|invalid(&e.to_string()))?;
                let inserted=c.execute("INSERT OR IGNORE INTO channel_legacy_candidates VALUES(?1,?2,?3,?4,?5)",params![job.as_slice(),channel.as_bytes().as_slice(),event.as_bytes().as_slice(),lifetime,member_bytes])?!=0;
                let started:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM channel_projection_work WHERE job=?1 AND channel=?2 AND next_rank>0)",params![job.as_slice(),channel.as_bytes().as_slice()],|r|r.get(0))?;
                if inserted && started {
                    c.execute("UPDATE channel_legacy_scan SET rebuilding=1 WHERE job=?1 AND channel=?2",params![job.as_slice(),channel.as_bytes().as_slice()])?;
                    c.execute("UPDATE channel_current SET source_revision=-1 WHERE channel=?1 AND source_job=?2",params![channel.as_bytes().as_slice(),job.as_slice()])?;
                }
            }
            if c.execute("UPDATE channel_legacy_scan SET next_rank=?3 WHERE job=?1 AND channel=?2 AND next_rank=?4",params![job.as_slice(),channel.as_bytes().as_slice(),through,next])?!=1 {return Err(LogError::StaleCursor);}
            Ok(())
        })?;
        Ok(false)
    }
}
