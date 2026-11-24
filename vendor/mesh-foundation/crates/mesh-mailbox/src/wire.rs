//! Versioned MFOM2 adapter for a bounded authenticated frame session.
//! Only this profile owns the 64-ID / transport frame constraints.
use crate::{
    client::{self, MailboxProgress, MailboxTurnError, MailboxTurnLimits},
    model::{MailboxCursor, MailboxDeltaPage, Record, RecordId, ScopeId},
    ports::MailboxTransport,
    rpc::{self, BODY_BUDGET, Frame, PAGE_IDS, RpcError},
};
use mesh_replication::session::ReconciliationSession;
#[derive(Debug)]
pub enum WireError<E> {
    Transport(E),
    Codec(RpcError),
    Limit,
}
impl<E: std::fmt::Display> std::fmt::Display for WireError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "transport: {e}"),
            Self::Codec(e) => write!(f, "codec: {e}"),
            Self::Limit => f.write_str("wire profile budget exceeded"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for WireError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(e) => Some(e),
            Self::Codec(e) => Some(e),
            Self::Limit => None,
        }
    }
}
pub struct WireTransport<'a, S: ?Sized> {
    session: &'a mut S,
}
impl<'a, S: ?Sized> WireTransport<'a, S> {
    pub fn new(session: &'a mut S) -> Self {
        Self { session }
    }
}
impl<S: ReconciliationSession + Send + ?Sized> MailboxTransport for WireTransport<'_, S> {
    type Error = WireError<S::Error>;
    async fn delta(
        &mut self,
        scope: ScopeId,
        prior: Option<&MailboxCursor>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<MailboxDeltaPage, Self::Error> {
        if limit == 0 || limit > PAGE_IDS || max_bytes == 0 {
            return Err(WireError::Limit);
        }
        let request = rpc::encode_frame(&Frame::Delta {
            scope,
            prior: prior.cloned(),
            limit: limit as u16,
            max_bytes: max_bytes.min(BODY_BUDGET) as u32,
        })
        .map_err(WireError::Codec)?;
        self.session
            .send(&request)
            .await
            .map_err(WireError::Transport)?;
        let reply = self.session.recv().await.map_err(WireError::Transport)?;
        let page = rpc::read_delta_response(&reply, scope).map_err(WireError::Codec)?;
        if !page.is_well_formed(scope, limit) {
            return Err(WireError::Codec(RpcError::UnexpectedMessage));
        }
        Ok(page)
    }
    async fn bodies(
        &mut self,
        scope: ScopeId,
        cursor: &MailboxCursor,
        ids: &[RecordId],
        max_bytes: usize,
    ) -> Result<Vec<Record>, Self::Error> {
        let budget = max_bytes.min(BODY_BUDGET);
        let request =
            rpc::make_get_request_bounded(scope, cursor, ids, budget).map_err(WireError::Codec)?;
        self.session
            .send(&request)
            .await
            .map_err(WireError::Transport)?;
        let reply = self.session.recv().await.map_err(WireError::Transport)?;
        rpc::read_get_response_bounded(&reply, scope, ids, budget).map_err(WireError::Codec)
    }
}
/// Convenience composition. Custom carriers can implement MailboxTransport and
/// call the same client directly; no network profile is required by the engine.
pub async fn sync_turn<S: ReconciliationSession + Send, Commit>(
    session: &mut S,
    scope: ScopeId,
    prior: Option<MailboxCursor>,
    mut limits: MailboxTurnLimits,
    commit: impl FnMut(&[Record], &MailboxCursor) -> Result<(), Commit>,
) -> Result<MailboxProgress, MailboxTurnError<WireError<S::Error>, Commit>> {
    limits.page_records = limits.page_records.min(PAGE_IDS);
    client::sync_turn(
        &mut WireTransport::new(session),
        scope,
        prior,
        limits,
        commit,
    )
    .await
}
