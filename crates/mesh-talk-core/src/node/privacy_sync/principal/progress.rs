//! Current preparation checkpoint only. No historical event bodies or ID sets
//! are encoded here. SQL decisions may commit before this cursor; replay is
//! idempotent and never advances the in-memory cursor after a failed save.
use super::*;
use bincode::Options;
use serde::{Deserialize, Serialize};
type SavedFileScope = (ConversationId, Author, EventId);
#[derive(Serialize, Deserialize)]
struct Progress {
    phase: Phase,
    generation: Option<([u8; 16], i64, u64)>,
    rank: i64,
    count: u64,
    capabilities: [bool; 3],
    fingerprint: Option<[u8; 32]>,
    active: Option<Active>,
    closing: Option<AncestryProgress>,
    legacy: Option<legacy::LegacyProgress>,
    modern: Option<(usize, Option<Vec<u8>>)>,
    file: Option<(usize, Option<SavedFileScope>)>,
    classified: bool,
    hash_started: bool,
    replica_shared: bool,
    restricted: bool,
    policy_local_channel: bool,
    policy_started: bool,
    policy_examined: usize,
}
impl Preparation {
    fn encode_progress(&self) -> Result<Vec<u8>, LogError> {
        let active = self.active.as_ref().map(|active| Active {
            header: active.header,
            parent_after: active.parent_after,
            parents_done: active.parents_done,
            rejected: active.rejected,
            receipt: active.receipt,
            receipt_checked: active.receipt_checked,
            receipt_candidate: active.receipt_candidate,
            body: None,
            meta: active.meta.clone(),
            original_members: active.original_members,
            ancestry: active.ancestry,
            ancestry_started: active.ancestry_started,
            ancestry_ready: active.ancestry_ready,
            member: active.member,
            departure_after: active.departure_after,
        });
        let legacy = match &self.principal {
            Some(Principal::Legacy(state)) => Some(state.progress()?),
            _ => None,
        };
        let modern = match &self.principal {
            Some(Principal::Modern { account, after, .. }) => Some((*account, after.clone())),
            _ => None,
        };
        let file = match &self.principal {
            Some(Principal::Files {
                examined, scope, ..
            }) => Some((
                *examined,
                scope.as_ref().map(|s| (s.parent, s.author, s.event)),
            )),
            _ => None,
        };
        let progress = Progress {
            phase: self.phase,
            generation: self.generation.map(|g| (g.job, g.source_revision, g.count)),
            rank: self.rank,
            count: self.metadata.count,
            capabilities: [
                self.metadata.channel_v2,
                self.metadata.channel_v3,
                self.metadata.channel_v4,
            ],
            fingerprint: self.metadata.fingerprint,
            active,
            closing: self.closing,
            legacy,
            modern,
            file,
            classified: self.principal.is_some(),
            hash_started: self.hash.is_some(),
            replica_shared: self.replica_shared,
            restricted: self.restricted,
            policy_local_channel: self.policy_local_channel,
            policy_started: self.policy_started,
            policy_examined: self.policy_examined,
        };
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&progress)
            .map_err(|e| LogError::Serialization(e.to_string()))
    }
    fn restore_progress(
        &mut self,
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        bytes: &[u8],
    ) -> Result<(), LogError> {
        let progress: Progress = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| LogError::Serialization(e.to_string()))?;
        self.phase = progress.phase;
        self.generation =
            progress
                .generation
                .map(|(job, source_revision, count)| OrderGeneration {
                    job,
                    source_revision,
                    count,
                });
        self.rank = progress.rank;
        self.metadata = SyncMetadata {
            count: progress.count,
            channel_v2: progress.capabilities[0],
            channel_v3: progress.capabilities[1],
            channel_v4: progress.capabilities[2],
            fingerprint: progress.fingerprint,
        };
        self.active = progress.active;
        self.closing = progress.closing;
        self.hash = None;
        self.hash_after = None;
        self.hash_pending = 0;
        self.hash_head = None;
        self.hash_eof = false;
        // The optional wire fingerprint is restarted as bounded ID-only pages;
        // SHA's transient partial block is never a second stored crypto format.
        if self.phase == Phase::Hash && progress.hash_started {
            self.metadata.count = self
                .metadata
                .count
                .checked_sub(view.pending.len() as u64)
                .ok_or_else(|| LogError::CorruptFile("invalid pending fingerprint count".into()))?;
        }
        self.replica_shared = progress.replica_shared;
        self.restricted = progress.restricted;
        self.policy_local_channel = progress.policy_local_channel;
        self.policy_started = progress.policy_started;
        self.policy_examined = progress.policy_examined;
        self.policy_skip = if self.phase == Phase::Policy {
            progress.policy_examined
        } else {
            0
        };
        self.policy_scopes = if self.phase == Phase::Policy && self.policy_started {
            Some(Box::new(store.node.manifest_scopes(view.conversation)))
        } else {
            None
        };
        self.principal = if progress.classified {
            Some(store.principal(view.conversation, self.replica_shared)?)
        } else {
            None
        };
        match (
            &mut self.principal,
            progress.legacy,
            progress.modern,
            progress.file,
        ) {
            (Some(Principal::Legacy(state)), Some(saved), None, None) => state.restore(saved)?,
            (
                Some(Principal::Modern { account, after, .. }),
                None,
                Some((saved_account, saved_after)),
                None,
            ) => {
                *account = saved_account;
                *after = saved_after;
            }
            (
                Some(Principal::Files {
                    examined,
                    skip,
                    scope,
                    parent,
                    ..
                }),
                None,
                None,
                Some((saved_count, saved_scope)),
            ) => {
                *examined = saved_count;
                *skip = saved_count;
                *scope = saved_scope.map(|(parent, author, event)| {
                    super::super::super::privacy_runtime::ManifestScope {
                        parent,
                        author,
                        event,
                    }
                });
                *parent = None;
            }
            (_, None, None, None) => {}
            _ => return Err(LogError::StaleCursor),
        }
        self.reload = false;
        Ok(())
    }
    pub(in crate::node::privacy_sync) fn step(
        &mut self,
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        if self.reload {
            let bytes = store
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selection_progress(self.selection()?, &view.repository)?
                .ok_or(LogError::StaleCursor)?;
            self.restore_progress(store, view, &bytes)?;
        }
        let result = (|| {
            let mut step = PreparationStep::default();
            // A turn admits multiple small transitions, not one durable commit
            // per historical header. Its row, edge and byte caps still include
            // rejected work, and the session yields after this finite turn.
            for transition in 0..budget.rows {
                if transition > 0
                    && (matches!(self.phase, Phase::Authority | Phase::Order)
                        || step.rows == budget.rows
                        || step.edges == budget.edges
                        || budget.bytes - step.bytes < 64)
                {
                    break;
                }
                let separate_turn = matches!(self.phase, Phase::Authority | Phase::Order);
                let remaining = PreparationBudget {
                    rows: budget.rows - step.rows,
                    edges: budget.edges - step.edges,
                    bytes: budget.bytes - step.bytes,
                };
                let next = match self.step_inner(store, view, remaining) {
                    Ok(next) => next,
                    Err(LogError::PageBudgetTooSmall { required })
                        if transition > 0 && required <= budget.bytes =>
                    {
                        break;
                    }
                    Err(error) => return Err(error),
                };
                step.rows += next.rows;
                step.edges += next.edges;
                step.bytes += next.bytes;
                step.ready = next.ready;
                if step.ready || separate_turn {
                    break;
                }
            }
            if let Some(selection) = &self.selection {
                let bytes = self.encode_progress()?;
                store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .save_selection_progress(selection, &view.repository, &bytes)?;
            }
            Ok(step)
        })();
        if result.is_err() {
            self.reload = self.selection.is_some();
        }
        result
    }
    #[cfg(test)]
    pub(in crate::node::privacy_sync) fn discard_transient_preparation(&mut self) {
        self.reload = true;
    }
}
