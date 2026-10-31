//! One caller-owned causal continuation. The decode cursor stays before its
//! selected event until authority preparation and body installation both finish.
//! Different target pages in the same channel share this raw decode progress.
use super::*;
use crate::node::channel::{
    ChannelMembershipProjection,
    checkpoint::causal::{CausalProjectionQuery, CausalProjectionStep},
};

#[derive(Default)]
pub(in crate::node) struct ReactionQueryWork {
    causal: Option<CausalReaction>,
}
struct CausalReaction {
    decode: [u8; 32],
    candidate: store::Candidate,
    query: CausalProjectionQuery,
    // Some(None) is a completed empty projection, distinct from Pending.
    ready: Option<Option<ChannelMembershipProjection>>,
}
impl ReactionQueryWork {
    pub(super) fn retain_for(&mut self, scope: &Scope, decode: [u8; 32], phase: i64) {
        if phase != 0
            || !scope.channel
            || self.causal.as_ref().is_some_and(|work| {
                work.decode != decode || work.candidate.conversation != scope.conversation
            })
        {
            self.causal = None;
        }
    }
    pub(super) fn start(
        &mut self,
        node: &Node,
        decode: [u8; 32],
        candidate: store::Candidate,
    ) -> Result<(), LogError> {
        let query = node.channel_checkpoints.start_causal_projection(
            candidate.conversation,
            &[candidate.id],
            false,
        )?;
        self.causal = Some(CausalReaction {
            decode,
            candidate,
            query,
            ready: None,
        });
        Ok(())
    }
    /// A preparation turn never also performs discovery or payload hydration.
    /// Ready retains its fact lease until a later turn checks the source again.
    pub(super) fn advance(
        &mut self,
        node: &Node,
        store: &store::Store,
        scope: &Scope,
        snapshot: &Snapshot,
    ) -> Result<bool, NodeError> {
        let Some(mut pending) = self.causal.take() else {
            return Ok(false);
        };
        if pending.decode != snapshot.decode || pending.candidate.conversation != scope.conversation
        {
            return Ok(false);
        }
        node.channel_checkpoints
            .check_causal_projection(&pending.query)?;
        if let Some(projection) = pending.ready.take() {
            let decoded = node.decode_selected_reaction(
                &pending.candidate,
                true,
                &snapshot.evidence,
                projection.as_ref(),
            )?;
            store.decoded(
                Some(scope.conversation),
                snapshot.decode,
                pending.candidate.ordinal,
                &[decoded],
            )?;
            // Drop only after the body/evidence transaction committed. On error
            // the disposable query retires and the exact ordinal remains eligible.
            return Ok(true);
        }
        let budget = PreparationBudget {
            rows: 64,
            edges: 512,
            bytes: 8 * 1024 * 1024,
        };
        let step = {
            let log = node.log.lock().expect("log mutex");
            node.channel_checkpoints.causal_projection_step(
                log.repository()?,
                &mut pending.query,
                budget,
            )?
        };
        let work = match step {
            CausalProjectionStep::Pending(work) => work,
            CausalProjectionStep::Ready { projection, work } => {
                pending.ready = Some(projection);
                work
            }
        };
        if work.rows > budget.rows || work.edges > budget.edges || work.bytes > budget.bytes {
            return Err(LogError::CorruptFile(
                "causal reaction preparation exceeded its work budget".into(),
            )
            .into());
        }
        self.causal = Some(pending);
        Ok(true)
    }
}
