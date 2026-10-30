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
        graph::IndexedGraph,
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
        let (mut reducer, mut next) =
            ProjectionFacts::load(self.profile.clone(), generation.job, channel)?;
        if next == 0 && self.cleanup_generation_step(channel, generation.job)? {
            return Ok(false);
        }
        if next > count {
            return Err(invalid("channel reducer cursor exceeds source"));
        }
        let graph = IndexedGraph {
            repository,
            conversation: channel,
        };
        let own = me.public().user_id();
        if next < count {
            let OrderReadState::Ready(page) = repository.causal_headers_after_rank(
                &channel,
                CausalOrder::Logical,
                generation.job,
                next - 1,
                PageBudget {
                    rows: ROWS,
                    bytes: 64 * 1024,
                },
            )?
            else {
                return Ok(false);
            };
            for header in page.items {
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
                    Some(
                        repository
                            .get(&header.id)?
                            .ok_or_else(|| invalid("channel control disappeared"))?,
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
                    reducer.step_header(&header)?
                };
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
                let activity = activity_before.finish(&reducer, &header, event.as_ref(), &own);
                let facts = reducer
                    .history
                    .as_ref()
                    .ok_or_else(|| invalid("missing channel projection owner"))?
                    .clone();
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
                    let admitted=facts.checkpoint_on(c,&mut reducer,&header,next,allowed,&own)?;
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
