//! Selected reads and existing authority-query adapters for history continuations.
use super::continuation::{HistoryContext, Step, units};
use super::*;
use crate::eventlog::{
    repository::{AncestorSelection, order::OrderWorkBudget},
    sync::network::PreparationStep,
};
use crate::node::channel::{
    checkpoint::causal::{CausalProjectionQuery, CausalProjectionStep, Query},
    graph::{
        AuthorityGraph,
        preparing::{DecisionBinding, PreparingGraph},
    },
};
use std::sync::Arc;

impl HistoryContext {
    pub(super) async fn header(&self, id: EventId) -> Result<Option<EventHeader>, LogError> {
        self.request(move |view, _| {
            Ok(Step::Ready(
                view.repository
                    .header(&id)?
                    .filter(|h| h.conversation == view.source.channel),
                units(1, 128),
            ))
        })
        .await
    }
    pub(super) async fn body(&self, id: EventId) -> Result<Option<Event>, LogError> {
        let Some(header) = self.header(id).await? else {
            return Ok(None);
        };
        self.request(move |view, budget| {
            let required = header
                .stored_bytes
                .checked_mul(3)
                .and_then(|n| n.checked_add(std::mem::size_of::<Event>()))
                .ok_or(LogError::RepositoryCounterExhausted)?;
            if required > budget.bytes {
                return Err(LogError::PageBudgetTooSmall { required });
            }
            Ok(Step::Ready(view.repository.get(&id)?, units(1, required)))
        })
        .await
    }
    pub(super) async fn canonical(
        &self,
        id: EventId,
        include: bool,
    ) -> Result<Option<crate::channel::ChannelState>, LogError> {
        self.request(move |view, budget| {
            let (state, bytes) = view.checkpoints.canonical_state_budgeted(
                view.source.channel,
                id,
                include,
                budget.bytes,
            )?;
            Ok(Step::Ready(state, units(1, bytes)))
        })
        .await
    }
    pub(super) async fn before(
        &self,
        id: EventId,
    ) -> Result<Option<crate::channel::ChannelState>, LogError> {
        let mut query: Option<CausalProjectionQuery> = None;
        self.request(move |view, budget| {
            let Some(query) = query.as_mut() else {
                query = Some(view.checkpoints.start_causal_projection(
                    view.source.channel,
                    &[id],
                    false,
                )?);
                return Ok(Step::Pending(units(1, 128)));
            };
            match view
                .checkpoints
                .causal_projection_step(view.repository, query, budget)?
            {
                CausalProjectionStep::Pending(work) => Ok(Step::Pending(work)),
                CausalProjectionStep::Ready { projection, work } => {
                    Ok(Step::Ready(projection.and_then(|p| p.state), work))
                }
            }
        })
        .await
    }
    pub(super) async fn owner(&self, id: EventId) -> Result<bool, LogError> {
        let Some(header) = self.header(id).await? else {
            return Ok(false);
        };
        let mut query = None;
        self.request(move |view, budget| {
            let Some(query) = query.as_mut() else {
                query = Some(view.start_owner_authorization(&header)?);
                return Ok(Step::Pending(units(1, 128)));
            };
            match view.owner_authorized_step(query, budget)? {
                OwnerAuthorizationStep::Pending(work) => Ok(Step::Pending(work)),
                OwnerAuthorizationStep::Ready {
                    authorized, work, ..
                } => Ok(Step::Ready(authorized, work)),
            }
        })
        .await
    }
    pub(super) async fn descends(&self, child: EventId, parent: EventId) -> Result<bool, LogError> {
        let mut selected: Option<AncestorSelection> = None;
        self.request(move |view, budget| {
            if child == parent {
                return Ok(Step::Ready(
                    view.repository
                        .header(&child)?
                        .is_some_and(|h| h.conversation == view.source.channel),
                    units(1, 128),
                ));
            }
            let Some(selection) = selected.as_ref() else {
                let Some(mut selection) = view
                    .repository
                    .begin_ancestor_selection(view.source.channel)?
                else {
                    return Err(LogError::StaleCursor);
                };
                if !view
                    .repository
                    .seed_ancestor_selection(&mut selection, &[child], true, false)?
                    .0
                {
                    return Ok(Step::Ready(false, units(1, 128)));
                }
                selected = Some(selection);
                return Ok(Step::Pending(units(1, 128)));
            };
            let done = view.repository.ancestor_selection_step_with_budget(
                selection,
                OrderWorkBudget {
                    vertices: budget.rows - 1,
                    edges: budget.edges,
                    bytes: budget.bytes - 64,
                },
            )?;
            let mut work = PreparationStep {
                ready: done.complete,
                rows: done.vertices,
                edges: done.edges,
                bytes: done.charged_bytes,
            };
            if !done.complete {
                return Ok(Step::Pending(work));
            }
            work.rows += 1;
            work.bytes += 64;
            Ok(Step::Ready(
                view.repository
                    .ancestor_selection_contains(selection, parent)?,
                work,
            ))
        })
        .await
    }
    pub(super) async fn control_page(
        &self,
        kind: EventKind,
        after: Option<i64>,
        reverse: bool,
    ) -> Result<Vec<(i64, EventId)>, LogError> {
        self.request(move|view,budget|{
            let limit=budget.rows.min(budget.bytes/128).clamp(1, 64);
            let page=view.checkpoints.profile().database().read(|c| {
                view.checkpoints.check_source_on(c,&view.source)?;
                let sql=if reverse {"SELECT rank,event FROM channel_event_authority WHERE job=?1 AND channel=?2 AND kind=?3 AND (?4 IS NULL OR rank<?4) ORDER BY rank DESC LIMIT ?5"} else {"SELECT rank,event FROM channel_event_authority WHERE job=?1 AND channel=?2 AND kind=?3 AND rank>?4 ORDER BY rank LIMIT ?5"};
                let mut statement=c.prepare(sql)?;
                let mut rows=statement.query(params![view.source.job.as_slice(),view.source.channel.as_bytes().as_slice(),kind as i64,if reverse{after}else{Some(after.unwrap_or(-1))},limit as i64])?;
                read_ids(&mut rows)
            })?;
            let count=page.len().max(1);Ok(Step::Ready(page,units(count,count*128)))
        }).await
    }
    pub(super) async fn offer_candidates(
        &self,
        invitation: EventId,
        recipient: String,
        after: Option<i64>,
    ) -> Result<Vec<(i64, EventId)>, LogError> {
        self.request(move|view,budget|{
            let limit=budget.rows.min(budget.bytes/128).clamp(1, 64);
            let page=view.checkpoints.profile().database().read(|c| {
                view.checkpoints.check_source_on(c,&view.source)?;
                let mut statement=c.prepare("SELECT rank,event FROM channel_history_controls WHERE job=?1 AND channel=?2 AND role=1 AND invitation=?3 AND recipient=?4 AND rank>?5 ORDER BY rank LIMIT ?6")?;
                let mut rows=statement.query(params![view.source.job.as_slice(),view.source.channel.as_bytes().as_slice(),invitation.as_bytes().as_slice(),recipient,after.unwrap_or(-1),limit as i64])?;
                read_ids(&mut rows)
            })?;
            let count=page.len().max(1);Ok(Step::Ready(page,units(count,count*128)))
        }).await
    }
    pub(super) async fn release_page(
        &self,
        offer: EventId,
        after: Option<i64>,
    ) -> Result<Vec<(i64, EventId)>, LogError> {
        self.request(move|view,budget|{
            let limit=budget.rows.min(budget.bytes/128).clamp(1, 64);
            let page=view.checkpoints.profile().database().read(|c| {
                view.checkpoints.check_source_on(c,&view.source)?;
                let mut statement=c.prepare("SELECT rank,event FROM channel_history_controls WHERE job=?1 AND channel=?2 AND role IN(2,3) AND offer=?3 AND rank>?4 ORDER BY rank LIMIT ?5")?;
                let mut rows=statement.query(params![view.source.job.as_slice(),view.source.channel.as_bytes().as_slice(),offer.as_bytes().as_slice(),after.unwrap_or(-1),limit as i64])?;
                read_ids(&mut rows)
            })?;
            let count=page.len().max(1);Ok(Step::Ready(page,units(count,count*128)))
        }).await
    }
    pub(super) async fn decision(
        &self,
        event: EventId,
        mut predicate: impl FnMut(&PreparingGraph<'_>) -> Result<bool, LogError> + Send + 'static,
    ) -> Result<bool, LogError> {
        let mut lease: Option<Arc<Query>> = None;
        self.request(move |view, budget| {
            let Some(lease) = lease.as_ref() else {
                lease = Some(view.checkpoints.start_decision_query(&view.source)?);
                return Ok(Step::Pending(units(1, 128)));
            };
            let graph = PreparingGraph::open(
                view.checkpoints.profile().clone(),
                view.repository,
                DecisionBinding {
                    job: lease.job(),
                    source_job: view.source.job,
                    revision: view.source.revision,
                    channel: view.source.channel,
                    event,
                },
                budget,
            )?;
            let prepared = graph.prepare()?;
            let work = graph.work();
            if !prepared || work.rows != 0 || work.edges != 0 || work.bytes != 0 {
                return Ok(Step::Pending(work));
            }
            let result = predicate(&graph)?;
            if graph.pending() {
                return Ok(Step::Pending(graph.work()));
            }
            Ok(Step::Ready(result, graph.work()))
        })
        .await
    }
}
fn read_ids(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<(i64, EventId)>, LogError> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let id: Vec<u8> = row.get(1)?;
        out.push((
            row.get(0)?,
            EventId::new(id.try_into().map_err(|_| LogError::StaleCursor)?),
        ));
    }
    Ok(out)
}
impl HistoryContext {
    pub(super) async fn acceptance_page(
        &self,
        author: [u8; 32],
        after: i64,
        before: i64,
    ) -> Result<Vec<(i64, EventId)>, LogError> {
        self.request(move|view,budget|{
            let limit=budget.rows.min(budget.bytes/128).clamp(1, 64);
            let page=view.checkpoints.profile().database().read(|c| {
                view.checkpoints.check_source_on(c,&view.source)?;
                let mut statement=c.prepare("SELECT rank,event FROM channel_event_authority INDEXED BY channel_authority_author_kind WHERE job=?1 AND channel=?2 AND kind=?3 AND author=?4 AND rank>?5 AND rank<?6 ORDER BY rank LIMIT ?7")?;
                let mut rows=statement.query(params![view.source.job.as_slice(),view.source.channel.as_bytes().as_slice(),EventKind::ChannelAccept as i64,author.as_slice(),after,before,limit as i64])?;
                read_ids(&mut rows)
            })?;
            let count=page.len().max(1);Ok(Step::Ready(page,units(count,count*128)))
        }).await
    }
}
