//! A caller-owned causal query retains one bounded current reducer and its SQL
//! fact lease. Pending work never publishes a partial membership projection.
use super::*;
use crate::eventlog::{
    repository::{
        AncestorSelection,
        order::{CausalOrder, OrderReadiness, OrderWorkBudget},
    },
    sync::network::{PreparationBudget, PreparationStep},
};
use std::collections::VecDeque;

pub(in crate::node) struct CausalProjectionQuery {
    channel: ConversationId,
    roots: Vec<EventId>,
    include_roots: bool,
    account: [u8; 32],
    generation: i64,
    source: Option<([u8; 16], i64)>,
    selection: Option<AncestorSelection>,
    lease: Option<Arc<Query>>,
    after: i64,
    ancestry_ready: bool,
    seeded: usize,
    completed: bool,
    pending: VecDeque<(i64, crate::eventlog::repository::EventHeader)>,
    page_next: Option<i64>,
    page_terminal: bool,
}

pub(in crate::node) enum CausalProjectionStep {
    Pending(PreparationStep),
    Ready {
        projection: Option<super::super::super::ChannelMembershipProjection>,
        work: PreparationStep,
    },
}

impl ChannelCheckpointStore {
    pub(in crate::node) fn start_causal_projection(
        &self,
        channel: ConversationId,
        roots: &[EventId],
        include_roots: bool,
    ) -> Result<CausalProjectionQuery, LogError> {
        if roots.is_empty() || roots.len() > 256 || (!include_roots && roots.len() != 1) {
            return Err(LogError::InvalidPageBudget);
        }
        // Legacy parent-cut facts are prepared by the canonical owner before
        // it publishes a source. A strict query must never treat unfinished
        // legacy discovery as an authoritative absence of departures.
        let source = self.source_binding(channel)?.ok_or(LogError::StaleCursor)?;
        Ok(CausalProjectionQuery {
            channel,
            roots: roots.to_vec(),
            include_roots,
            account: self.account,
            generation: self.generation,
            source: Some((source.job, source.revision)),
            selection: None,
            lease: None,
            after: -1,
            ancestry_ready: false,
            seeded: 0,
            completed: false,
            pending: VecDeque::new(),
            page_next: None,
            page_terminal: false,
        })
    }

    /// The handle remains useful as a cheap fence after Ready transfers its
    /// leased result, including when the caller waits for a fresh body budget.
    pub(in crate::node) fn check_causal_projection(
        &self,
        query: &CausalProjectionQuery,
    ) -> Result<(), LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            if query.account != self.account || query.generation != self.generation {
                return Err(LogError::StaleCursor);
            }
            if let Some((job, revision)) = query.source {
                replay::require_source_on(c, query.channel, job, revision)?;
            }
            Ok(())
        })
    }

    pub(in crate::node) fn causal_projection_step(
        &self,
        repository: &EventRepository,
        query: &mut CausalProjectionQuery,
        budget: PreparationBudget,
    ) -> Result<CausalProjectionStep, LogError> {
        budget.validate()?;
        self.check_causal_projection(query)?;
        if query.completed {
            return Err(LogError::StaleCursor);
        }
        if query.selection.is_none() {
            let rows = budget.rows_for(256)?;
            let cleaned = repository.ancestor_cleanup_step(rows)?;
            if cleaned > 0 {
                return Ok(CausalProjectionStep::Pending(PreparationStep {
                    rows: cleaned,
                    bytes: cleaned * 256,
                    ..Default::default()
                }));
            }
            let cleaned = self.profile.database().transaction(|c| {
                self.check_account_on(c)?;
                super::cleanup_step_on(c, rows)
            })?;
            if cleaned > 0 {
                return Ok(CausalProjectionStep::Pending(PreparationStep {
                    rows: cleaned,
                    bytes: cleaned * 256,
                    ..Default::default()
                }));
            }
            let work = repository.order_step(
                &query.channel,
                CausalOrder::Logical,
                OrderWorkBudget {
                    vertices: budget.rows,
                    edges: budget.edges,
                    bytes: budget.bytes,
                },
            )?;
            let charged = PreparationStep {
                ready: false,
                rows: work.seeded + work.ranked + work.cleaned,
                edges: work.edges,
                bytes: work.charged_bytes,
            };
            if work.readiness != OrderReadiness::Ready || charged.rows > 0 || charged.edges > 0 {
                return Ok(CausalProjectionStep::Pending(charged));
            }
            let Some(mut selection) = repository.begin_ancestor_selection(query.channel)? else {
                query.completed = true;
                return Ok(CausalProjectionStep::Ready {
                    projection: None,
                    work: PreparationStep {
                        ready: true,
                        ..Default::default()
                    },
                });
            };
            selection.retire_with_owner();
            query.source = Some(selection.generation());
            query.selection = Some(selection);
            return Ok(CausalProjectionStep::Pending(PreparationStep::default()));
        }
        if query.seeded < query.roots.len() {
            let count = budget
                .rows_for(64)?
                .min(128)
                .min(query.roots.len() - query.seeded);
            let through = query.seeded + count;
            let (valid, examined) = repository.seed_ancestor_selection(
                query
                    .selection
                    .as_mut()
                    .expect("query selection initialized"),
                &query.roots[query.seeded..through],
                through == query.roots.len(),
                query.include_roots,
            )?;
            let work = PreparationStep {
                rows: examined,
                bytes: examined * 64,
                ..Default::default()
            };
            if !valid {
                query.completed = true;
                return Ok(CausalProjectionStep::Ready {
                    projection: None,
                    work: PreparationStep {
                        ready: true,
                        ..work
                    },
                });
            }
            query.seeded = through;
            return Ok(CausalProjectionStep::Pending(work));
        }
        let selection = query
            .selection
            .as_ref()
            .expect("query selection initialized");
        if !query.ancestry_ready {
            let work = repository.ancestor_selection_step_with_budget(
                selection,
                OrderWorkBudget {
                    vertices: budget.rows,
                    edges: budget.edges,
                    bytes: budget.bytes,
                },
            )?;
            query.ancestry_ready = work.complete;
            return Ok(CausalProjectionStep::Pending(PreparationStep {
                ready: false,
                rows: work.vertices,
                edges: work.edges,
                bytes: work.charged_bytes,
            }));
        }
        let (source_job, revision) = query.source.expect("query source initialized");
        if query.lease.is_none() {
            let lease = Arc::new(Query {
                profile: self.profile.clone(),
                job: *uuid::Uuid::new_v4().as_bytes(),
            });
            self.profile.database().transaction(|c| {
                self.check_account_on(c)?;
                replay::require_source_on(c, query.channel, source_job, revision)?;
                c.execute(
                    "INSERT INTO channel_projection_queries VALUES(?1,?2,0)",
                    params![lease.job.as_slice(), query.channel.as_bytes().as_slice()],
                )?;
                Ok(())
            })?;
            query.lease = Some(lease);
        }
        let lease = query.lease.as_ref().expect("query fact lease initialized");
        let row_bytes = std::mem::size_of::<crate::eventlog::repository::EventHeader>() + 32;
        if query.pending.is_empty() && !query.page_terminal {
            let rows = budget.rows_for(row_bytes)?.min(64);
            let page = repository.ancestor_headers_page(selection, query.after, rows)?;
            query.pending = page.items.into();
            query.page_next = page.next;
            query.page_terminal = page.next.is_none();
            if query.pending.is_empty()
                && let Some(next) = page.next
            {
                query.after = next;
            }
            return Ok(CausalProjectionStep::Pending(PreparationStep {
                rows: page.examined,
                bytes: page.examined * row_bytes,
                ..Default::default()
            }));
        }
        let snapshot = ProjectionFacts::snapshot_charge(&self.profile, lease.job, query.channel)?;
        let minimum = snapshot
            .checked_add(256)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if minimum > budget.bytes {
            return Err(LogError::PageBudgetTooSmall { required: minimum });
        }
        if let Some((rank, _)) = query.pending.front() {
            // The SQL cursor is authoritative if a caller retries after COMMIT
            // but before its in-memory page cursor was advanced.
            let next:i64=self.profile.database().read(|c| {
                self.check_account_on(c)?;
                replay::require_source_on(c,query.channel,source_job,revision)?;
                c.query_row("SELECT COALESCE((SELECT next_rank FROM channel_projection_work WHERE job=?1 AND channel=?2),0)",params![lease.job.as_slice(),query.channel.as_bytes().as_slice()],|r|r.get(0)).map_err(Into::into)
            })?;
            if *rank < next {
                query.after = *rank;
                query.pending.pop_front();
                return Ok(CausalProjectionStep::Pending(PreparationStep {
                    rows: 1,
                    bytes: 32,
                    ..Default::default()
                }));
            }
        }
        if let Some((_, header)) = query.pending.front() {
            use crate::node::channel::graph::preparing::PreparingGraph;
            let graph = PreparingGraph::open(
                self.profile.clone(),
                repository,
                crate::node::channel::graph::preparing::DecisionBinding {
                    job: lease.job,
                    source_job,
                    revision,
                    channel: query.channel,
                    event: header.id,
                },
                budget,
            )?;
            if !graph.prepare()? {
                return Ok(CausalProjectionStep::Pending(graph.work()));
            }
            let work = graph.work();
            if work.rows > 0 || work.edges > 0 || work.bytes > 0 {
                return Ok(CausalProjectionStep::Pending(work));
            }
        }
        let (mut reducer, _) = ProjectionFacts::load_bound(
            self.profile.clone(),
            lease.job,
            source_job,
            query.channel,
        )?;
        reducer.authority.retain_query_lease(lease.clone())?;
        Arc::get_mut(
            reducer
                .history
                .as_mut()
                .ok_or_else(|| invalid("causal projection facts missing"))?,
        )
        .ok_or_else(|| invalid("causal projection already shared"))?
        ._query_lease = Some(lease.clone());
        reducer.legacy_checks = true;
        if let Some((rank, header)) = query.pending.front().copied() {
            use crate::node::channel::graph::{AuthorityGraph, preparing::PreparingGraph};
            if budget.rows < 2 {
                return Err(LogError::InvalidPageBudget);
            }
            let graph = PreparingGraph::open(
                self.profile.clone(),
                repository,
                crate::node::channel::graph::preparing::DecisionBinding {
                    job: lease.job,
                    source_job,
                    revision,
                    channel: query.channel,
                    event: header.id,
                },
                PreparationBudget {
                    rows: budget.rows - 1,
                    edges: budget.edges,
                    bytes: budget.bytes - snapshot,
                },
            )?;
            let allowed =
                if super::super::super::projection_facts::is_membership_control(header.kind) {
                    let Some(event) = graph.event(header.id)? else {
                        if graph.pending() {
                            let mut work = graph.work();
                            work.bytes += snapshot;
                            return Ok(CausalProjectionStep::Pending(work));
                        }
                        return Err(LogError::StaleCursor);
                    };
                    reducer.step(&event, &graph)?
                } else {
                    reducer.step_header(&header, &graph)?
                };
            let facts = reducer
                .history
                .as_ref()
                .ok_or_else(|| invalid("missing causal fact owner"))?
                .clone();
            let after_accept = if graph.pending() {
                false
            } else {
                facts.after_accept(&reducer, &header, &graph)?
            };
            let mut work = graph.work();
            work.bytes += snapshot;
            if graph.pending() {
                return Ok(CausalProjectionStep::Pending(work));
            }
            self.profile.database().transaction(|c| {
                self.check_account_on(c)?;
                replay::require_source_on(c, query.channel, source_job, revision)?;
                facts.checkpoint_on(c, &mut reducer, &header, rank, (allowed, after_accept), "")?;
                graph.complete_on(c)?;
                Ok(())
            })?;
            query.after = rank;
            query.pending.pop_front();
            work.rows += 1;
            if query.pending.is_empty()
                && let Some(next) = query.page_next
            {
                query.after = next;
            }
            return Ok(CausalProjectionStep::Pending(work));
        }
        if let Some(next) = query.page_next {
            query.after = next;
            return Ok(CausalProjectionStep::Pending(PreparationStep {
                bytes: snapshot,
                ..Default::default()
            }));
        }
        self.check_causal_projection(query)?;
        let mut projection = reducer.finish();
        projection._query_lease = Some(lease.clone());
        query.completed = true;
        Ok(CausalProjectionStep::Ready {
            projection: Some(projection),
            work: PreparationStep {
                ready: true,
                bytes: snapshot,
                ..Default::default()
            },
        })
    }
}
