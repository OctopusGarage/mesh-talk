//! Dispatch for the existing authenticated mailbox delta protocol. The host
//! owns authentication, per-session negotiation and the transaction/permit.

use crate::rpc::{
    BODY_BUDGET, Frame, MailboxCursor, MailboxDeltaPage, PAGE_IDS, RpcError, decode_frame,
    encode_frame,
};
use crate::{Record, RecordId, ScopeId};

/// A capability probe for this generic mailbox profile. Hosts negotiate on an
/// authenticated session before dispatch; advertised support grants no access.
pub fn make_probe(scope: ScopeId) -> Result<Vec<u8>, RpcError> {
    encode_frame(&Frame::Probe { scope })
}
pub fn probe_scope(bytes: &[u8]) -> Option<ScopeId> {
    match decode_frame(bytes).ok()? {
        Frame::Probe { scope } => Some(scope),
        _ => None,
    }
}
pub fn read_probe_response(bytes: &[u8], scope: ScopeId) -> Result<bool, RpcError> {
    match decode_frame(bytes)? {
        Frame::Supported { scope: actual } if actual == scope => Ok(true),
        Frame::Unsupported { scope: actual } if actual == scope => Ok(false),
        _ => Err(RpcError::UnexpectedMessage),
    }
}

/// Current authorized relay store. Implementations must recheck retention,
/// cursor identity, and current authority under the same read transaction.
pub trait MailboxStore {
    type Error;

    fn delta(
        &self,
        scope: &ScopeId,
        prior: Option<&MailboxCursor>,
        limit: usize,
    ) -> Result<MailboxDeltaPage, Self::Error>;

    fn bodies(
        &self,
        scope: &ScopeId,
        cursor: &MailboxCursor,
        ids: &[RecordId],
        max_bytes: usize,
    ) -> Result<Vec<Record>, Self::Error>;
}

pub struct MailboxReply {
    pub scope: ScopeId,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum DispatchError<E> {
    Wire(RpcError),
    Store(E),
}

/// Decode one negotiated request and encode its response. Open no store
/// selection before checking the exact connection scope and frame budget.
pub fn dispatch<S: MailboxStore>(
    store: &S,
    authorized: Option<ScopeId>,
    bytes: &[u8],
) -> Result<MailboxReply, DispatchError<S::Error>> {
    let request = decode_frame(bytes).map_err(DispatchError::Wire)?;
    let (scope, response) = match request {
        Frame::Delta { scope, prior } if authorized == Some(scope) => {
            let page = store
                .delta(&scope, prior.as_ref(), PAGE_IDS)
                .map_err(DispatchError::Store)?;
            (scope, Frame::Page(page))
        }
        Frame::Get { scope, cursor, ids }
            if authorized == Some(scope) && cursor.scope == scope && ids.len() <= PAGE_IDS =>
        {
            let records = store
                .bodies(&scope, &cursor, &ids, BODY_BUDGET)
                .map_err(DispatchError::Store)?;
            (scope, Frame::Records { scope, records })
        }
        _ => return Err(DispatchError::Wire(RpcError::UnexpectedMessage)),
    };
    Ok(MailboxReply {
        scope,
        bytes: encode_frame(&response).map_err(DispatchError::Wire)?,
    })
}

impl<E: std::fmt::Display> std::fmt::Display for DispatchError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(error) => write!(f, "mailbox frame: {error}"),
            Self::Store(error) => write!(f, "mailbox store: {error}"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for DispatchError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Wire(error) => Some(error),
            Self::Store(error) => Some(error),
        }
    }
}
