//! Resumable owner authorization composes the same canonical, strict-causal,
//! and later ownership-loss rules as IndexedView's collecting compatibility API.
use super::*;
use crate::eventlog::{
    repository::{AncestorSelection, order::OrderWorkBudget},
    sync::network::{PreparationBudget, PreparationStep},
};
use crate::node::channel::checkpoint::causal::{CausalProjectionQuery, CausalProjectionStep};

pub(in crate::node) struct OwnerAuthorizationQuery {
    source: ChannelSource,
    header: EventHeader,
    author: String,
    phase: OwnerPhase,
    publication_rank: i64,
    before: Option<crate::channel::ChannelState>,
}
enum OwnerPhase {
    Canonical,
    Causal(CausalProjectionQuery),
    Rank,
    Future {
        kind: usize,
        after: i64,
    },
    Before {
        kind: usize,
        rank: i64,
        id: EventId,
    },
    After {
        kind: usize,
        rank: i64,
        id: EventId,
    },
    StartAncestry {
        kind: usize,
        rank: i64,
        id: EventId,
    },
    Ancestry {
        kind: usize,
        rank: i64,
        selection: AncestorSelection,
    },
    Done,
}
pub(in crate::node) enum OwnerAuthorizationStep {
    Pending(PreparationStep),
    Ready {
        authorized: bool,
        before: Option<crate::channel::ChannelState>,
        work: PreparationStep,
    },
}
impl IndexedView<'_> {
    pub(in crate::node) fn start_owner_authorization(
        &self,
        event: &EventHeader,
    ) -> Result<OwnerAuthorizationQuery, LogError> {
        self.checkpoints.check_source(&self.source)?;
        if event.conversation != self.source.channel {
            return Err(LogError::StaleCursor);
        }
        Ok(OwnerAuthorizationQuery {
            source: self.source,
            header: *event,
            author: event.author.user_id(),
            phase: OwnerPhase::Canonical,
            publication_rank: -1,
            before: None,
        })
    }
    pub(in crate::node) fn owner_authorized_step(
        &self,
        query: &mut OwnerAuthorizationQuery,
        budget: PreparationBudget,
    ) -> Result<OwnerAuthorizationStep, LogError> {
        budget.validate()?;
        if budget.bytes < 64 {
            return Err(LogError::PageBudgetTooSmall { required: 64 });
        }
        self.checkpoints.check_source(&query.source)?;
        if self.source.channel != query.source.channel
            || self.source.job != query.source.job
            || self.source.revision != query.source.revision
        {
            return Err(LogError::StaleCursor);
        }
        let mut work = PreparationStep::default();
        let phase = std::mem::replace(&mut query.phase, OwnerPhase::Done);
        let mut verdict = None;
        query.phase = match phase {
            OwnerPhase::Done => return Err(LogError::StaleCursor),
            OwnerPhase::Canonical => {
                let (state, bytes) = self.checkpoints.canonical_state_budgeted(
                    query.source.channel,
                    query.header.id,
                    false,
                    budget.bytes,
                )?;
                work.rows = 1;
                work.bytes = bytes;
                if state.is_none_or(|state| state.is_dissolved() || !state.can_admin(&query.author))
                {
                    verdict = Some(false);
                    OwnerPhase::Done
                } else {
                    OwnerPhase::Causal(self.checkpoints.start_causal_projection(
                        query.source.channel,
                        &[query.header.id],
                        false,
                    )?)
                }
            }
            OwnerPhase::Causal(mut causal) => match self.checkpoints.causal_projection_step(
                self.repository,
                &mut causal,
                budget,
            )? {
                CausalProjectionStep::Pending(done) => {
                    work = done;
                    OwnerPhase::Causal(causal)
                }
                CausalProjectionStep::Ready {
                    projection,
                    work: done,
                } => {
                    work = done;
                    let before = projection.and_then(|p| p.state);
                    if before
                        .as_ref()
                        .is_none_or(|state| state.is_dissolved() || !state.can_admin(&query.author))
                    {
                        verdict = Some(false);
                        OwnerPhase::Done
                    } else {
                        query.before = before;
                        OwnerPhase::Rank
                    }
                }
            },
            OwnerPhase::Rank => {
                let rank=self.checkpoints.profile().database().read(|c| {
                    self.checkpoints.check_source_on(c,&query.source)?;
                    c.query_row("SELECT rank FROM channel_event_authority WHERE job=?1 AND channel=?2 AND event=?3",params![query.source.job.as_slice(),query.source.channel.as_bytes().as_slice(),query.header.id.as_bytes().as_slice()],|r|r.get(0)).map_err(Into::into)
                })?;
                work.rows = 1;
                work.bytes = 64;
                query.publication_rank = rank;
                OwnerPhase::Future {
                    kind: 0,
                    after: rank,
                }
            }
            OwnerPhase::Future { kind, after } => {
                let kinds = [
                    EventKind::ChannelOwnerTransfer,
                    EventKind::ChannelAccountAuthority,
                ];
                if kind == kinds.len() {
                    verdict = Some(true);
                    OwnerPhase::Done
                } else {
                    let row:Option<(i64,Vec<u8>)>=self.checkpoints.profile().database().read(|c| {
                        self.checkpoints.check_source_on(c,&query.source)?;
                        c.query_row("SELECT rank,event FROM channel_event_authority WHERE job=?1 AND channel=?2 AND kind=?3 AND rank>?4 ORDER BY rank LIMIT 1",params![query.source.job.as_slice(),query.source.channel.as_bytes().as_slice(),kinds[kind] as i64,after],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(Into::into)
                    })?;
                    work.rows = 1;
                    work.bytes = 64;
                    if let Some((rank, id)) = row {
                        OwnerPhase::Before {
                            kind,
                            rank,
                            id: EventId::new(id.try_into().map_err(|_| LogError::StaleCursor)?),
                        }
                    } else {
                        OwnerPhase::Future {
                            kind: kind + 1,
                            after: query.publication_rank,
                        }
                    }
                }
            }
            OwnerPhase::Before { kind, rank, id } => {
                let (state, bytes) = self.checkpoints.canonical_state_budgeted(
                    query.source.channel,
                    id,
                    false,
                    budget.bytes,
                )?;
                work.rows = 1;
                work.bytes = bytes;
                if state.is_some_and(|state| state.can_admin(&query.author)) {
                    OwnerPhase::After { kind, rank, id }
                } else {
                    OwnerPhase::Future { kind, after: rank }
                }
            }
            OwnerPhase::After { kind, rank, id } => {
                let (state, bytes) = self.checkpoints.canonical_state_budgeted(
                    query.source.channel,
                    id,
                    true,
                    budget.bytes,
                )?;
                work.rows = 1;
                work.bytes = bytes;
                if state.is_some_and(|state| !state.can_admin(&query.author)) {
                    OwnerPhase::StartAncestry { kind, rank, id }
                } else {
                    OwnerPhase::Future { kind, after: rank }
                }
            }
            OwnerPhase::StartAncestry { kind, rank, id } => {
                let mut selection = self
                    .repository
                    .begin_ancestor_selection(query.source.channel)?
                    .ok_or(LogError::StaleCursor)?;
                if !self
                    .repository
                    .seed_ancestor_selection(&mut selection, &[id], true, true)?
                    .0
                {
                    return Err(LogError::StaleCursor);
                }
                work.rows = 1;
                work.bytes = 64;
                OwnerPhase::Ancestry {
                    kind,
                    rank,
                    selection,
                }
            }
            OwnerPhase::Ancestry {
                kind,
                rank,
                selection,
            } => {
                let done = self.repository.ancestor_selection_step_with_budget(
                    &selection,
                    OrderWorkBudget {
                        vertices: budget.rows,
                        edges: budget.edges,
                        bytes: budget.bytes,
                    },
                )?;
                work.rows = done.vertices;
                work.edges = done.edges;
                work.bytes = done.charged_bytes;
                if !done.complete {
                    OwnerPhase::Ancestry {
                        kind,
                        rank,
                        selection,
                    }
                } else if self
                    .repository
                    .ancestor_selection_contains(&selection, query.header.id)?
                {
                    OwnerPhase::Future { kind, after: rank }
                } else {
                    verdict = Some(false);
                    OwnerPhase::Done
                }
            }
        };
        self.checkpoints.check_source(&query.source)?;
        if let Some(authorized) = verdict {
            work.ready = true;
            let before = query.before.take().filter(|_| authorized);
            Ok(OwnerAuthorizationStep::Ready {
                authorized,
                before,
                work,
            })
        } else {
            work.ready = false;
            Ok(OwnerAuthorizationStep::Pending(work))
        }
    }
}
