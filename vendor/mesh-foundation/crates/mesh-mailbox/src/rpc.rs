//! Bounded opaque mailbox frames and snapshot cursors.

use crate::{Record, RecordId, ScopeId};
use bincode::Options;
use serde::{Deserialize, Serialize};

const MAX_PLAINTEXT: usize = 65_519;

#[derive(Debug)]
pub enum RpcError {
    UnexpectedMessage,
    Serialization(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedMessage => f.write_str("unexpected mailbox frame"),
            Self::Serialization(message) => write!(f, "invalid mailbox encoding: {message}"),
        }
    }
}

impl std::error::Error for RpcError {}

pub use crate::model::{MailboxCursor, MailboxDeltaPage};

const MAGIC: &[u8; 5] = b"MFOM2";
pub const PAGE_IDS: usize = 64;
pub const BODY_BUDGET: usize = MAX_PLAINTEXT - 8192;

#[derive(Debug, Serialize, Deserialize)]
pub enum Frame {
    Probe {
        scope: ScopeId,
    },
    Unsupported {
        scope: ScopeId,
    },
    Supported {
        scope: ScopeId,
    },
    Delta {
        scope: ScopeId,
        prior: Option<MailboxCursor>,
        limit: u16,
        max_bytes: u32,
    },
    Page(MailboxDeltaPage),
    Get {
        scope: ScopeId,
        cursor: MailboxCursor,
        ids: Vec<RecordId>,
        max_bytes: u32,
    },
    Records {
        scope: ScopeId,
        records: Vec<Record>,
    },
}

pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, RpcError> {
    let body = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit((MAX_PLAINTEXT - MAGIC.len()) as u64)
        .reject_trailing_bytes()
        .serialize(frame)
        .map_err(|e| RpcError::Serialization(e.to_string()))?;
    if body.len() + MAGIC.len() > MAX_PLAINTEXT {
        return Err(RpcError::Serialization("oversized mailbox frame".into()));
    }
    let mut result = Vec::with_capacity(MAGIC.len() + body.len());
    result.extend_from_slice(MAGIC);
    result.extend_from_slice(&body);
    Ok(result)
}

/// Identify the mailbox protocol prefix before selecting its decoder.
pub fn is_mailbox_frame(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

pub fn decode_frame(bytes: &[u8]) -> Result<Frame, RpcError> {
    if bytes.len() > MAX_PLAINTEXT || !bytes.starts_with(MAGIC) {
        return Err(RpcError::UnexpectedMessage);
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit((MAX_PLAINTEXT - MAGIC.len()) as u64)
        .reject_trailing_bytes()
        .deserialize(&bytes[MAGIC.len()..])
        .map_err(|e| RpcError::Serialization(e.to_string()))
}

/// Qualify a delta response before its cursor can be considered for local commit.
pub fn read_delta_response(bytes: &[u8], scope: ScopeId) -> Result<MailboxDeltaPage, RpcError> {
    match decode_frame(bytes)? {
        Frame::Page(page) if page.is_well_formed(scope, PAGE_IDS) => Ok(page),
        _ => Err(RpcError::UnexpectedMessage),
    }
}

pub fn make_get_request(
    scope: ScopeId,
    cursor: &MailboxCursor,
    ids: &[RecordId],
) -> Result<Vec<u8>, RpcError> {
    make_get_request_bounded(scope, cursor, ids, BODY_BUDGET)
}

pub fn make_get_request_bounded(
    scope: ScopeId,
    cursor: &MailboxCursor,
    ids: &[RecordId],
    max_bytes: usize,
) -> Result<Vec<u8>, RpcError> {
    if ids.is_empty()
        || ids.len() > PAGE_IDS
        || cursor.scope != scope
        || max_bytes == 0
        || max_bytes > BODY_BUDGET
    {
        return Err(RpcError::UnexpectedMessage);
    }
    encode_frame(&Frame::Get {
        scope,
        cursor: cursor.clone(),
        ids: ids.to_vec(),
        max_bytes: max_bytes as u32,
    })
}

/// IDs and scope must match in order. Callers still authenticate each body
/// and commit acceptance before advancing local progress.
pub fn read_get_response(
    bytes: &[u8],
    scope: ScopeId,
    ids: &[RecordId],
) -> Result<Vec<Record>, RpcError> {
    read_get_response_bounded(bytes, scope, ids, BODY_BUDGET)
}
pub fn read_get_response_bounded(
    bytes: &[u8],
    scope: ScopeId,
    ids: &[RecordId],
    max_bytes: usize,
) -> Result<Vec<Record>, RpcError> {
    if max_bytes == 0 || max_bytes > BODY_BUDGET || ids.len() > PAGE_IDS {
        return Err(RpcError::UnexpectedMessage);
    }
    match decode_frame(bytes)? {
        Frame::Records {
            scope: response,
            records,
        } if response == scope
            && records.len() == ids.len()
            && records.iter().zip(ids).all(|(event, id)| {
                event.id == *id && event.scope == scope && event.verify_integrity()
            })
            && records
                .iter()
                .try_fold(0usize, |sum, event| sum.checked_add(event.body.len()))
                .is_some_and(|bytes| bytes <= max_bytes) =>
        {
            Ok(records)
        }
        _ => Err(RpcError::UnexpectedMessage),
    }
}
