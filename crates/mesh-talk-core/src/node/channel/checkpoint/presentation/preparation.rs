//! Resumable presentation selectors retain bounded current policy queries.
//! Completed rejected controls and departure scans advance durable cursors.
use super::*;
use crate::{
    eventlog::{
        repository::{EventHeader, EventRepository},
        sync::network::{PreparationBudget, PreparationStep},
    },
    identity::device::DeviceIdentity,
    node::{
        channel::projection_facts::ProjectionFacts,
        join_history::{
            self,
            indexed::{IndexedView, OwnerAuthorizationQuery, OwnerAuthorizationStep},
        },
    },
};

const WORK_SCHEMA:&str="CREATE TABLE channel_book_work(
 channel BLOB PRIMARY KEY CHECK(length(channel)=32),owner_account BLOB NOT NULL CHECK(length(owner_account)=32),generation INTEGER NOT NULL,
 job BLOB NOT NULL CHECK(length(job)=16),stamp INTEGER NOT NULL,replica_stamp INTEGER NOT NULL,progress BLOB NOT NULL
) WITHOUT ROWID;";
#[derive(Default, Serialize, Deserialize)]
struct Progress {
    policy_after: Option<i64>,
    policy: Option<crate::channel::HistoryPolicy>,
    departure_after: Option<EventId>,
    departed: bool,
    departure_done: bool,
    member_index: usize,
    preferences: Vec<(String, u64, EventId, bool)>,
}
pub(in crate::node::channel::checkpoint) struct PolicyCandidate {
    source: ChannelSource,
    rank: i64,
    policy: crate::channel::HistoryPolicy,
    query: OwnerAuthorizationQuery,
}
pub(super) fn initialize(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_book_work')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(WORK_SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, WORK_SCHEMA)
}
impl ChannelCheckpointStore {
    fn load_presentation_progress(
        &self,
        source: &ChannelSource,
        stamps: (i64, i64),
        budget: usize,
    ) -> Result<(Progress, usize), LogError> {
        self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            let length:Option<i64>=c.query_row("SELECT length(progress) FROM channel_book_work WHERE channel=?1 AND owner_account=?2 AND generation=?3 AND job=?4 AND stamp=?5 AND replica_stamp=?6",params![source.channel.as_bytes().as_slice(),self.account.as_slice(),self.generation,source.job.as_slice(),stamps.0,stamps.1],|r|r.get(0)).optional()?;
            let Some(length)=length else{return Ok((Progress::default(),256));};
            let charge=usize::try_from(length).ok().and_then(|n|n.checked_mul(3)).and_then(|n|n.checked_add(256)).ok_or(LogError::RepositoryCounterExhausted)?;
            if charge>budget {return Err(LogError::PageBudgetTooSmall{required:charge});}
            let bytes:Vec<u8>=c.query_row("SELECT progress FROM channel_book_work WHERE channel=?1",[source.channel.as_bytes().as_slice()],|r|r.get(0))?;
            let progress=bincode::DefaultOptions::new().with_fixint_encoding().with_limit(bytes.len() as u64).reject_trailing_bytes().deserialize(&bytes).map_err(|e|invalid(&e.to_string()))?;
            Ok((progress,charge))
        })
    }
    fn save_presentation_progress(
        &self,
        source: &ChannelSource,
        stamps: (i64, i64),
        progress: &Progress,
    ) -> Result<(), LogError> {
        let bytes = encode(progress)?;
        self.profile.database().transaction(|c| {
            self.check_source_on(c,source)?;
            c.execute("INSERT INTO channel_book_work VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(channel) DO UPDATE SET owner_account=excluded.owner_account,generation=excluded.generation,job=excluded.job,stamp=excluded.stamp,replica_stamp=excluded.replica_stamp,progress=excluded.progress",params![source.channel.as_bytes().as_slice(),self.account.as_slice(),self.generation,source.job.as_slice(),stamps.0,stamps.1,bytes])?;
            Ok(())
        })
    }
    /// Yield after a selector, owner query, departure fact or current member.
    /// Existing cache keys survive ordinary message tails; incomplete causal
    /// candidates keep their stricter source fence until safe-tail integration.
    pub(in crate::node) fn prepare_presentation_step(
        &self,
        repository: &EventRepository,
        me: &DeviceIdentity,
        channel: ConversationId,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        if budget.rows < 3 {
            return Err(LogError::InvalidPageBudget);
        }
        if budget.bytes < 512 {
            return Err(LogError::PageBudgetTooSmall { required: 512 });
        }
        let mut candidate = self
            .presentation_query
            .lock()
            .expect("channel presentation work lock");
        let source = self.source_binding(channel)?.ok_or(LogError::StaleCursor)?;
        let stamps = self.presentation_stamps(&source)?;
        let cached:bool=self.profile.database().read(|c| {
            self.check_source_on(c,&source)?;
            c.query_row("SELECT EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?1 AND owner_account=?2 AND generation=?3 AND job=?4 AND stamp=?5 AND replica_stamp=?6)",params![channel.as_bytes().as_slice(),self.account.as_slice(),self.generation,source.job.as_slice(),stamps.0,stamps.1],|r|r.get(0)).map_err(Into::into)
        })?;
        if cached {
            candidate.remove(&channel);
            self.profile.database().transaction(|c| {
                self.check_source_on(c, &source)?;
                c.execute(
                    "DELETE FROM channel_book_work WHERE channel=?1",
                    [channel.as_bytes().as_slice()],
                )?;
                Ok(())
            })?;
            return Ok(PreparationStep {
                ready: true,
                ..Default::default()
            });
        }
        let (mut progress, bytes) =
            self.load_presentation_progress(&source, stamps, budget.bytes)?;
        let mut work = PreparationStep {
            rows: 1,
            bytes,
            ..Default::default()
        };
        if progress.policy.is_none() {
            let pending = candidate.remove(&channel).filter(|p| p.source == source);
            if let Some(mut pending) = pending {
                let view = IndexedView::new(self, repository, me, source);
                let remaining = remaining(budget, work)?;
                match view.owner_authorized_step(&mut pending.query, remaining)? {
                    OwnerAuthorizationStep::Pending(done) => {
                        add(&mut work, done);
                        candidate.insert(channel, pending);
                    }
                    OwnerAuthorizationStep::Ready {
                        authorized,
                        work: done,
                        ..
                    } => {
                        add(&mut work, done);
                        if authorized {
                            progress.policy = Some(pending.policy);
                        } else {
                            progress.policy_after = Some(pending.rank);
                        }
                        self.save_presentation_progress(&source, stamps, &progress)?;
                    }
                }
                return Ok(work);
            }
            let selected:Option<(i64,Vec<u8>)>=self.profile.database().read(|c| {
                self.check_source_on(c,&source)?;
                c.query_row("SELECT rank,event FROM channel_event_authority INDEXED BY channel_authority_kind WHERE job=?1 AND channel=?2 AND kind=?3 AND (?4 IS NULL OR rank<?4) ORDER BY rank DESC LIMIT 1",params![source.job.as_slice(),channel.as_bytes().as_slice(),crate::eventlog::EventKind::ChannelHistoryPolicy as i64,progress.policy_after],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(Into::into)
            })?;
            work.rows += 1;
            work.bytes += 128;
            let Some((rank, event)) = selected else {
                progress.policy = Some(Default::default());
                self.save_presentation_progress(&source, stamps, &progress)?;
                return Ok(work);
            };
            let id = EventId::new(
                event
                    .try_into()
                    .map_err(|_| invalid("invalid history policy identity"))?,
            );
            let header = repository.header(&id)?.ok_or(LogError::StaleCursor)?;
            let charge = header
                .stored_bytes
                .checked_mul(3)
                .and_then(|n| {
                    n.checked_add(
                        std::mem::size_of::<crate::eventlog::Event>()
                            + std::mem::size_of::<EventHeader>(),
                    )
                })
                .ok_or(LogError::RepositoryCounterExhausted)?;
            admit(budget, work, charge)?;
            let event = repository.get(&id)?.ok_or(LogError::StaleCursor)?;
            work.rows += 1;
            work.bytes += charge;
            // Invalid encoding cannot win regardless of authority. Decode the
            // one admitted body before scheduling its potentially deep graph.
            if let Some(policy) = join_history::decode_policy(&event.ciphertext) {
                let view = IndexedView::new(self, repository, me, source);
                if candidate.len() >= 2 * crate::node::session_pool::MAX_TCP_CONNECTIONS {
                    let mut retired = Vec::new();
                    for (channel, pending) in candidate.iter() {
                        admit(budget, work, 128)?;
                        work.rows += 1;
                        work.bytes += 128;
                        match self.check_source(&pending.source) {
                            Ok(()) => {}
                            Err(LogError::StaleCursor) => retired.push(*channel),
                            Err(error) => return Err(error),
                        }
                    }
                    for channel in retired {
                        candidate.remove(&channel);
                    }
                }
                if candidate.len() >= 2 * crate::node::session_pool::MAX_TCP_CONNECTIONS {
                    return Err(LogError::Io(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "channel presentation preparation capacity reached",
                    )));
                }
                candidate.insert(
                    channel,
                    PolicyCandidate {
                        source,
                        rank,
                        policy,
                        query: view.start_owner_authorization(&header)?,
                    },
                );
            } else {
                progress.policy_after = Some(rank);
            }
            self.save_presentation_progress(&source, stamps, &progress)?;
            return Ok(work);
        }
        if !progress.departure_done {
            let snapshot = ProjectionFacts::snapshot_charge(&self.profile, source.job, channel)?;
            admit(budget, work, snapshot + 512)?;
            let (projection, _) = ProjectionFacts::load(self.profile.clone(), source.job, channel)?;
            let history = projection
                .history
                .as_ref()
                .ok_or_else(|| invalid("missing channel presentation facts"))?;
            let row = history.pending_departure_after(
                &me.user_id(),
                &projection.authority,
                progress.departure_after,
            )?;
            match row {
                Some((event, pending)) => {
                    progress.departure_after = Some(event);
                    progress.departed = pending;
                    progress.departure_done = pending;
                }
                None => progress.departure_done = true,
            }
            self.save_presentation_progress(&source, stamps, &progress)?;
            work.rows += 1;
            work.bytes += snapshot + 512;
            return Ok(work);
        }
        let state_bytes: usize = self.profile.database().read(|c| {
            self.check_source_on(c, &source)?;
            let length: i64 = c.query_row(
                "SELECT length(state) FROM channel_current WHERE channel=?1",
                [channel.as_bytes().as_slice()],
                |r| r.get(0),
            )?;
            usize::try_from(length)
                .ok()
                .and_then(|n| n.checked_mul(3))
                .and_then(|n| n.checked_add(256))
                .ok_or(LogError::RepositoryCounterExhausted)
        })?;
        admit(budget, work, state_bytes)?;
        let state = self.current_state(channel)?.ok_or(LogError::StaleCursor)?;
        work.rows += 1;
        work.bytes += state_bytes;
        if let Some(member) = state.members().get(progress.member_index) {
            admit(budget, work, 256)?;
            let member = member.user_id();
            let row:Option<(Vec<u8>,Vec<u8>,bool)>=self.profile.database().read(|c| {
                self.check_source_on(c,&source)?;
                c.query_row("SELECT lamport,event,enabled FROM channel_replica_preferences WHERE job=?1 AND channel=?2 AND member=?3",params![source.job.as_slice(),channel.as_bytes().as_slice(),&member],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(Into::into)
            })?;
            if let Some((clock, event, enabled)) = row {
                progress.preferences.push((
                    member,
                    u64::from_be_bytes(
                        clock
                            .try_into()
                            .map_err(|_| invalid("invalid replica clock"))?,
                    ),
                    EventId::new(
                        event
                            .try_into()
                            .map_err(|_| invalid("invalid replica event"))?,
                    ),
                    enabled,
                ));
            }
            progress.member_index += 1;
            work.rows += 1;
            work.bytes += 256;
            self.save_presentation_progress(&source, stamps, &progress)?;
            return Ok(work);
        }
        admit(budget, work, 128)?;
        let transferred:bool=self.profile.database().read(|c| {
            self.check_source_on(c,&source)?;
            c.query_row("SELECT EXISTS(SELECT 1 FROM channel_activities INDEXED BY channel_activity_actor WHERE job=?1 AND channel=?2 AND actor=?3 AND kind='transferred')",params![source.job.as_slice(),channel.as_bytes().as_slice(),me.user_id()],|r|r.get(0)).map_err(Into::into)
        })?;
        let presentation = Presentation {
            policy: progress
                .policy
                .ok_or_else(|| invalid("missing prepared history policy"))?,
            preferences: progress.preferences,
            departed: progress.departed,
            transferred,
        };
        self.save_presentation(&source, &state, &presentation)?;
        work.rows += 1;
        work.bytes += 128;
        work.ready = true;
        Ok(work)
    }
}
fn remaining(
    budget: PreparationBudget,
    work: PreparationStep,
) -> Result<PreparationBudget, LogError> {
    let remaining = PreparationBudget {
        rows: budget
            .rows
            .checked_sub(work.rows)
            .ok_or(LogError::InvalidPageBudget)?,
        edges: budget
            .edges
            .checked_sub(work.edges)
            .ok_or(LogError::InvalidPageBudget)?,
        bytes: budget
            .bytes
            .checked_sub(work.bytes)
            .ok_or(LogError::InvalidPageBudget)?,
    };
    remaining.validate()?;
    Ok(remaining)
}
fn admit(budget: PreparationBudget, work: PreparationStep, bytes: usize) -> Result<(), LogError> {
    if work.rows >= budget.rows {
        return Err(LogError::InvalidPageBudget);
    }
    let required = work
        .bytes
        .checked_add(bytes)
        .ok_or(LogError::RepositoryCounterExhausted)?;
    if required > budget.bytes {
        return Err(LogError::PageBudgetTooSmall { required });
    }
    Ok(())
}
fn add(work: &mut PreparationStep, done: PreparationStep) {
    work.rows += done.rows;
    work.edges += done.edges;
    work.bytes += done.bytes;
}
