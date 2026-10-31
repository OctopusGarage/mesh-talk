//! Causal membership queries share the channel reducer and its exact indexed
//! facts. A serialized, disposable query never retains an ancestor ID set in RAM.
use super::*;
use crate::{
    eventlog::repository::EventRepository,
    identity::device::DeviceIdentity,
    node::channel::{graph::IndexedGraph, projection_facts::ProjectionFacts},
};

mod query;
pub(in crate::node) use query::{CausalProjectionQuery, CausalProjectionStep};

const SCHEMA: &str = "
CREATE TABLE channel_projection_queries(job BLOB PRIMARY KEY CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),retired INTEGER NOT NULL CHECK(retired IN(0,1))) WITHOUT ROWID;
CREATE INDEX channel_projection_queries_retired ON channel_projection_queries(retired,job);
CREATE TABLE channel_legacy_cuts(source_job BLOB NOT NULL,channel BLOB NOT NULL,event BLOB NOT NULL,selection BLOB NOT NULL,query BLOB NOT NULL,after_rank INTEGER NOT NULL,PRIMARY KEY(source_job,channel,event)) WITHOUT ROWID;
CREATE TABLE channel_causal_states(job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),event BLOB NOT NULL CHECK(length(event)=32),state BLOB,PRIMARY KEY(job,channel,event)) WITHOUT ROWID;
";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    c.execute_batch("CREATE INDEX IF NOT EXISTS channel_projection_queries_retired ON channel_projection_queries(retired,job);")?;
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    // At most one serialized query can be active. Its potentially large facts
    // remain deferred cleanup, never an all-row warm-start replay.
    c.execute("UPDATE channel_projection_queries SET retired=1 WHERE job NOT IN (SELECT query FROM channel_legacy_cuts)", [])?;
    Ok(())
}

pub(in crate::node) struct Query {
    profile: Arc<ProfileStorage>,
    job: [u8; 16],
}
impl Drop for Query {
    fn drop(&mut self) {
        if let Err(error) = self.profile.database().transaction(|c| {
            c.execute(
                "UPDATE channel_projection_queries SET retired=1 WHERE job=?1",
                [self.job.as_slice()],
            )?;
            Ok(())
        }) {
            log::warn!("channel causal query cleanup deferred: {error}");
        }
    }
}
fn cleanup_on(c: &Connection) -> Result<bool, LogError> {
    Ok(cleanup_step_on(c, 128)? != 0)
}
pub(super) fn cleanup_step_on(c: &Connection, limit: usize) -> Result<usize, LogError> {
    if limit == 0 {
        return Err(LogError::InvalidPageBudget);
    }
    let row: Option<(Vec<u8>, Vec<u8>)> = c
        .query_row(
            "SELECT job,channel FROM channel_projection_queries WHERE retired=1 LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((job, channel)) = row else {
        return Ok(0);
    };
    for (table, key) in [
        ("channel_decision_answers", "key"),
        ("channel_decision_work", "channel"),
        ("channel_event_authority", "event"),
        ("channel_departure_facts", "member,event"),
        ("channel_membership_snapshots", "rank"),
        ("channel_authority_facts", "kind,key"),
        ("channel_authority_current", "channel"),
        ("channel_projection_work", "channel"),
    ] {
        if table == "channel_decision_work" {
            super::super::graph::preparing::retire_selection_on(c, &job, &channel)?;
        }
        let sql = format!(
            "DELETE FROM {table} WHERE job=?1 AND channel=?2 AND ({key}) IN (SELECT {key} FROM {table} WHERE job=?1 AND channel=?2 LIMIT ?3)"
        );
        let removed = c.execute(
            &sql,
            params![
                &job,
                &channel,
                i64::try_from(limit).map_err(|_| LogError::InvalidPageBudget)?
            ],
        )?;
        if removed != 0 {
            return Ok(removed);
        }
    }
    c.execute("DELETE FROM channel_projection_queries WHERE job=?1", [job])?;
    Ok(1)
}

impl ChannelCheckpointStore {
    /// Membership established by strict signed ancestors, excluding concurrent
    /// controls that merely appear earlier in canonical order. Safe tail appends
    /// reuse the immutable result; changed canonical generations do not.
    pub(in crate::node) fn causal_state(
        &self,
        repository: &EventRepository,
        me: &DeviceIdentity,
        channel: ConversationId,
        event: EventId,
    ) -> Result<Option<ChannelState>, LogError> {
        let _serial = self.causal_query.lock().expect("channel causal query lock");
        let binding = self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let row: Option<(Vec<u8>,i64)> = c.query_row("SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((job,revision)) = row else { return Ok(None); };
            let job: [u8;16] = job.try_into().map_err(|_|invalid("invalid channel current job"))?;
            replay::require_source_on(c,channel,job,revision)?;
            let cached: Option<Option<Vec<u8>>> = c.query_row("SELECT state FROM channel_causal_states WHERE job=?1 AND channel=?2 AND event=?3",params![job.as_slice(),channel.as_bytes().as_slice(),event.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            Ok(Some((job,revision,cached)))
        })?;
        let Some((source_job, revision, cached)) = binding else {
            return Ok(None);
        };
        if let Some(cached) = cached {
            return cached
                .map(|bytes| {
                    ChannelState::decode_current(&bytes)
                        .map_err(|error| invalid(&error.to_string()))
                })
                .transpose();
        }
        while self.profile.database().transaction(|c| cleanup_on(c))? {}
        let Some(selection) = repository.select_ancestors(channel, event)? else {
            return Ok(None);
        };
        if selection.generation() != (source_job, revision) {
            return Err(LogError::StaleCursor);
        }
        let query = Query {
            profile: self.profile.clone(),
            job: *uuid::Uuid::new_v4().as_bytes(),
        };
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c, channel, source_job, revision)?;
            c.execute(
                "INSERT INTO channel_projection_queries VALUES(?1,?2,0)",
                params![query.job.as_slice(), channel.as_bytes().as_slice()],
            )?;
            Ok(())
        })?;
        let (mut reducer, _) =
            ProjectionFacts::load_bound(self.profile.clone(), query.job, source_job, channel)?;
        let graph = IndexedGraph {
            repository,
            conversation: channel,
        };
        let own = me.user_id();
        let mut after = -1;
        loop {
            let page = repository.ancestor_headers_page(&selection, after, 64)?;
            for (rank, header) in page.items {
                let allowed = if super::super::projection_facts::is_membership_control(header.kind)
                {
                    let event = repository
                        .get(&header.id)?
                        .ok_or_else(|| invalid("causal channel control disappeared"))?;
                    reducer.step(&event, &graph)?
                } else {
                    reducer.step_header(&header, &graph)?
                };
                let facts = reducer
                    .history
                    .as_ref()
                    .ok_or_else(|| invalid("missing causal channel fact owner"))?
                    .clone();
                let after_accept = facts.after_accept(&reducer, &header, &graph)?;
                self.profile.database().transaction(|c| {
                    self.check_account_on(c)?;
                    replay::require_source_on(c, channel, source_job, revision)?;
                    facts.checkpoint_on(
                        c,
                        &mut reducer,
                        &header,
                        rank,
                        (allowed, after_accept),
                        &own,
                    )?;
                    Ok(())
                })?;
            }
            let Some(next) = page.next else {
                break;
            };
            after = next;
        }
        let state = reducer.prior;
        let bytes = state
            .as_ref()
            .map(ChannelState::encode_current)
            .transpose()
            .map_err(|error| invalid(&error.to_string()))?;
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c, channel, source_job, revision)?;
            c.execute(
                "INSERT INTO channel_causal_states VALUES(?1,?2,?3,?4)",
                params![
                    source_job.as_slice(),
                    channel.as_bytes().as_slice(),
                    event.as_bytes().as_slice(),
                    bytes
                ],
            )?;
            c.execute(
                "UPDATE channel_projection_queries SET retired=1 WHERE job=?1",
                [query.job.as_slice()],
            )?;
            Ok(())
        })?;
        Ok(state)
    }
}

impl ChannelCheckpointStore {
    /// The caller retains this exact query's disk facts with the returned
    /// projection. Causal authorization checks never outlive their fact lease.
    pub(in crate::node) fn causal_projection(
        &self,
        repository: &EventRepository,
        channel: ConversationId,
        event: EventId,
    ) -> Result<Option<super::super::ChannelMembershipProjection>, LogError> {
        self.causal_projection_roots(repository, channel, &[event], false)
    }

    pub(in crate::node) fn causal_projection_basis(
        &self,
        repository: &EventRepository,
        channel: ConversationId,
        basis: &[EventId],
    ) -> Result<Option<super::super::ChannelMembershipProjection>, LogError> {
        self.causal_projection_roots(repository, channel, basis, true)
    }
    fn causal_projection_roots(
        &self,
        repository: &EventRepository,
        channel: ConversationId,
        roots: &[EventId],
        include_roots: bool,
    ) -> Result<Option<super::super::ChannelMembershipProjection>, LogError> {
        let _serial = self.causal_query.lock().expect("channel causal query lock");
        let Some(mut selection) =
            repository.start_ancestor_selection_roots(channel, roots, include_roots)?
        else {
            return Ok(None);
        };
        selection.retire_with_owner();
        while !repository.ancestor_selection_step(&selection)? {}
        let (source_job, revision) = selection.generation();
        let query = Arc::new(Query {
            profile: self.profile.clone(),
            job: *uuid::Uuid::new_v4().as_bytes(),
        });
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c, channel, source_job, revision)?;
            cleanup_on(c)?;
            c.execute(
                "INSERT INTO channel_projection_queries VALUES(?1,?2,0)",
                params![query.job.as_slice(), channel.as_bytes().as_slice()],
            )?;
            Ok(())
        })?;
        let (mut reducer, _) =
            ProjectionFacts::load_bound(self.profile.clone(), query.job, source_job, channel)?;
        reducer.authority.retain_query_lease(query.clone())?;
        Arc::get_mut(
            reducer
                .history
                .as_mut()
                .ok_or_else(|| invalid("causal projection facts missing"))?,
        )
        .ok_or_else(|| invalid("causal projection already shared"))?
        ._query_lease = Some(query.clone());
        reducer.legacy_checks = true;
        let graph = IndexedGraph {
            repository,
            conversation: channel,
        };
        let mut after = -1;
        loop {
            let page = repository.ancestor_headers_page(&selection, after, 64)?;
            for (rank, header) in page.items {
                let allowed = if super::super::projection_facts::is_membership_control(header.kind)
                {
                    let event = repository
                        .get(&header.id)?
                        .ok_or_else(|| invalid("causal channel control disappeared"))?;
                    reducer.step(&event, &graph)?
                } else {
                    reducer.step_header(&header, &graph)?
                };
                let facts = reducer
                    .history
                    .as_ref()
                    .ok_or_else(|| invalid("missing causal fact owner"))?
                    .clone();
                let after_accept = facts.after_accept(&reducer, &header, &graph)?;
                self.profile.database().transaction(|c| {
                    self.check_account_on(c)?;
                    replay::require_source_on(c, channel, source_job, revision)?;
                    facts.checkpoint_on(
                        c,
                        &mut reducer,
                        &header,
                        rank,
                        (allowed, after_accept),
                        "",
                    )?;
                    Ok(())
                })?;
            }
            let Some(next) = page.next else {
                break;
            };
            after = next;
        }
        repository.retire_ancestor_selection(selection.id())?;
        let mut projection = reducer.finish();
        projection._query_lease = Some(query);
        Ok(Some(projection))
    }
}

pub(super) enum LegacyCut {
    Pending,
    Ready(Option<crate::node::channel_account_authority::LegacyDeparture>),
}
impl ChannelCheckpointStore {
    pub(super) fn legacy_cut_step(
        &self,
        repository: &EventRepository,
        target: &crate::eventlog::repository::EventHeader,
        source_job: [u8; 16],
        revision: i64,
        budget: crate::eventlog::sync::network::PreparationBudget,
    ) -> Result<(LegacyCut, crate::eventlog::sync::network::PreparationStep), LogError> {
        use crate::eventlog::sync::network::{PreparationBudget, PreparationStep};
        use crate::node::channel::graph::{
            AuthorityGraph,
            preparing::{DecisionBinding, PreparingGraph},
        };
        budget.validate()?;
        let _serial = self.causal_query.lock().expect("channel causal query lock");
        let channel = target.conversation;
        let saved:Option<(Vec<u8>,Vec<u8>,i64)>=self.profile.database().read(|c| {
            self.check_account_on(c)?;replay::require_source_on(c,channel,source_job,revision)?;
            c.query_row("SELECT selection,query,after_rank FROM channel_legacy_cuts WHERE source_job=?1 AND channel=?2 AND event=?3",params![source_job.as_slice(),channel.as_bytes().as_slice(),target.id.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(Into::into)
        })?;
        let Some((selection, query, after)) = saved else {
            if budget.bytes < 64 {
                return Err(LogError::PageBudgetTooSmall { required: 64 });
            }
            let Some(selection) = repository.start_ancestor_selection(channel, target.id)? else {
                return Err(LogError::StaleCursor);
            };
            let query = *uuid::Uuid::new_v4().as_bytes();
            let installed = self.profile.database().transaction(|c| {
                self.check_account_on(c)?;
                replay::require_source_on(c, channel, source_job, revision)?;
                c.execute(
                    "INSERT INTO channel_projection_queries VALUES(?1,?2,0)",
                    params![query.as_slice(), channel.as_bytes().as_slice()],
                )?;
                c.execute(
                    "INSERT INTO channel_legacy_cuts VALUES(?1,?2,?3,?4,?5,-1)",
                    params![
                        source_job.as_slice(),
                        channel.as_bytes().as_slice(),
                        target.id.as_bytes().as_slice(),
                        selection.id().as_slice(),
                        query.as_slice()
                    ],
                )?;
                Ok(())
            });
            if let Err(error) = installed {
                repository.retire_ancestor_selection(selection.id())?;
                return Err(error);
            }
            return Ok((
                LegacyCut::Pending,
                PreparationStep {
                    rows: 1,
                    bytes: 64,
                    ..Default::default()
                },
            ));
        };
        let id = selection
            .try_into()
            .map_err(|_| invalid("invalid legacy ancestry lease"))?;
        let query = query
            .try_into()
            .map_err(|_| invalid("invalid legacy projection lease"))?;
        let selection = match repository.resume_ancestor_selection(id) {
            Ok(Some(selection)) => selection,
            Ok(None) | Err(LogError::StaleCursor) => {
                repository.retire_ancestor_selection(id)?;
                self.finish_legacy_cut(channel, source_job, target.id, query)?;
                return Ok((
                    LegacyCut::Pending,
                    PreparationStep {
                        rows: 1,
                        bytes: 64,
                        ..Default::default()
                    },
                ));
            }
            Err(error) => return Err(error),
        };
        let ancestry = repository.ancestor_selection_step_with_budget(
            &selection,
            crate::eventlog::repository::order::OrderWorkBudget {
                vertices: budget.rows,
                edges: budget.edges,
                bytes: budget.bytes,
            },
        )?;
        if !ancestry.complete || ancestry.vertices > 0 || ancestry.edges > 0 {
            return Ok((
                LegacyCut::Pending,
                PreparationStep {
                    rows: ancestry.vertices,
                    edges: ancestry.edges,
                    bytes: ancestry.charged_bytes,
                    ..Default::default()
                },
            ));
        }
        let row_bytes = std::mem::size_of::<crate::eventlog::repository::EventHeader>();
        if budget.rows < 2 {
            return Err(LogError::InvalidPageBudget);
        }
        if budget.bytes < row_bytes + 256 {
            return Err(LogError::PageBudgetTooSmall {
                required: row_bytes + 256,
            });
        }
        let page = repository.ancestor_headers_page(&selection, after, 1)?;
        let mut work = PreparationStep {
            rows: page.examined,
            bytes: page.examined * row_bytes,
            ..Default::default()
        };
        let replaying = page.items.first().copied();
        if replaying.is_none()
            && let Some(next) = page.next
        {
            self.profile.database().transaction(|c| {
                self.check_account_on(c)?; replay::require_source_on(c,channel,source_job,revision)?;
                c.execute("UPDATE channel_legacy_cuts SET after_rank=?4 WHERE source_job=?1 AND channel=?2 AND event=?3",params![source_job.as_slice(),channel.as_bytes().as_slice(),target.id.as_bytes().as_slice(),next])?;
                Ok(())
            })?;
            return Ok((LegacyCut::Pending, work));
        }
        let decision = replaying
            .as_ref()
            .map_or(target.id, |(_, header)| header.id);
        let binding = DecisionBinding {
            job: query,
            source_job,
            revision,
            channel,
            event: decision,
        };
        let available = PreparationBudget {
            rows: budget.rows - work.rows,
            edges: budget.edges,
            bytes: budget.bytes - work.bytes,
        };
        let graph = PreparingGraph::open(self.profile.clone(), repository, binding, available)?;
        let prepared = graph.prepare()?;
        let graph_work = graph.work();
        if !prepared || graph_work.rows > 0 || graph_work.edges > 0 || graph_work.bytes > 0 {
            work.rows += graph_work.rows;
            work.edges += graph_work.edges;
            work.bytes += graph_work.bytes;
            return Ok((LegacyCut::Pending, work));
        }
        let snapshot = ProjectionFacts::snapshot_charge(&self.profile, query, channel)?;
        let required = work
            .bytes
            .checked_add(snapshot)
            .and_then(|n| n.checked_add(256))
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if required > budget.bytes {
            return Err(LogError::PageBudgetTooSmall { required });
        }
        let (mut reducer, _) =
            ProjectionFacts::load_bound(self.profile.clone(), query, source_job, channel)?;
        reducer.legacy_checks = false;
        work.bytes += snapshot;
        let graph = PreparingGraph::open(
            self.profile.clone(),
            repository,
            binding,
            PreparationBudget {
                rows: budget.rows - work.rows,
                edges: budget.edges,
                bytes: budget.bytes - work.bytes,
            },
        )?;
        if let Some((rank, header)) = replaying {
            let allowed = if super::super::projection_facts::is_membership_control(header.kind) {
                let event = graph.event(header.id)?;
                if graph.pending() {
                    false
                } else {
                    let event = event.ok_or_else(|| invalid("legacy cut control disappeared"))?;
                    reducer.step(&event, &graph)?
                }
            } else {
                reducer.step_header(&header, &graph)?
            };
            let facts = reducer
                .history
                .as_ref()
                .ok_or_else(|| invalid("legacy cut facts missing"))?
                .clone();
            let after_accept = if graph.pending() {
                false
            } else {
                facts.after_accept(&reducer, &header, &graph)?
            };
            if !graph.pending() {
                self.profile.database().transaction(|c| {
                    self.check_account_on(c)?;replay::require_source_on(c,channel,source_job,revision)?;
                    facts.checkpoint_on(c,&mut reducer,&header,rank,(allowed,after_accept),"")?;
                    graph.complete_on(c)?;
                    c.execute("UPDATE channel_legacy_cuts SET after_rank=?4 WHERE source_job=?1 AND channel=?2 AND event=?3",params![source_job.as_slice(),channel.as_bytes().as_slice(),target.id.as_bytes().as_slice(),rank])?;
                    Ok(())
                })?;
            }
            let charged = graph.work();
            work.rows += charged.rows;
            work.edges += charged.edges;
            work.bytes += charged.bytes;
            return Ok((LegacyCut::Pending, work));
        }
        let mut candidate = None;
        if reducer.authority.current_event().is_none()
            && reducer
                .history
                .as_ref()
                .ok_or_else(|| invalid("legacy cut facts missing"))?
                .parents_authorized(target.id, &graph)?
            && let Some(state) = &reducer.prior
        {
            let author = target.author.user_id();
            if state.owner() != author
                && state.is_accepted_member(&author)
                && let Some((public, membership_event, acceptance_event, true)) =
                    reducer.roots.get(&author)
                && state.members().contains(public)
            {
                candidate = Some(crate::node::channel_account_authority::LegacyDeparture {
                    event: target.id,
                    member: crate::node::channel_account_authority::LegacyAccountMember {
                        public: public.clone(),
                        membership_event: *membership_event,
                        acceptance_event: *acceptance_event,
                    },
                });
            }
        }
        let charged = graph.work();
        work.rows += charged.rows;
        work.edges += charged.edges;
        work.bytes += charged.bytes;
        if graph.pending() {
            return Ok((LegacyCut::Pending, work));
        }
        self.finish_legacy_cut(channel, source_job, target.id, query)?;
        repository.retire_ancestor_selection(selection.id())?;
        Ok((LegacyCut::Ready(candidate), work))
    }
    fn finish_legacy_cut(
        &self,
        channel: ConversationId,
        source_job: [u8; 16],
        event: EventId,
        query: [u8; 16],
    ) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            c.execute(
                "DELETE FROM channel_legacy_cuts WHERE source_job=?1 AND channel=?2 AND event=?3",
                params![
                    source_job.as_slice(),
                    channel.as_bytes().as_slice(),
                    event.as_bytes().as_slice()
                ],
            )?;
            c.execute(
                "UPDATE channel_projection_queries SET retired=1 WHERE job=?1",
                [query.as_slice()],
            )?;
            cleanup_on(c)?;
            Ok(())
        })
    }
}
