//! Mesh Talk-compatible bounded mailbox delta frames and snapshot cursors.

use bincode::Options;
use mesh_replication::legacy_event::{ConversationId, Event, EventId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MailboxCursor {
    pub store_uuid: [u8; 16],
    pub conversation: ConversationId,
    pub epoch: i64,
    pub first_seen: Option<u64>,
    pub revision: i64,
    pub after: i64,
    pub window: Option<(i64, i64)>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MailboxDeltaPage {
    pub cursor: MailboxCursor,
    pub ids: Vec<EventId>,
    pub complete: bool,
    pub reset: bool,
}

impl MailboxCursor {
    pub fn accepts(&self, current: &Self) -> bool {
        self.store_uuid == current.store_uuid
            && self.conversation == current.conversation
            && self.epoch == current.epoch
            && self.first_seen == current.first_seen
            && self.revision >= 0
            && self.revision <= current.revision
            && self.after >= 0
            && self.after <= current.after
            && self.window.is_none_or(|(upper, revision)| {
                self.after <= upper
                    && upper <= current.after
                    && self.revision <= revision
                    && revision <= current.revision
            })
    }
}

const MAGIC: &[u8; 5] = b"MTOM1";
pub const PAGE_IDS: usize = 64;
pub const BODY_BUDGET: usize = MAX_PLAINTEXT - 2048;

#[derive(Debug, Serialize, Deserialize)]
pub enum Frame {
    Supported {
        conversation: ConversationId,
    },
    Delta {
        conversation: ConversationId,
        prior: Option<MailboxCursor>,
    },
    Page(MailboxDeltaPage),
    Get {
        conversation: ConversationId,
        cursor: MailboxCursor,
        ids: Vec<EventId>,
    },
    Events {
        conversation: ConversationId,
        events: Vec<Event>,
    },
}

// The legacy probe contains a domain-separated digest rather than a real set
// fingerprint. An ordinary fingerprint can coincide only by a SHA256 collision;
// even then a legacy matched=true response is an error, never positive support.
// Consequently old endpoints receive only a frame they already understand.
pub fn marker(conversation: ConversationId) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-opaque-mailbox-rpc-v1-probe\0");
    hash.update(conversation.as_bytes());
    hash.finalize().into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_frame_roundtrip_rejects_bad_magic_trailing_bytes_and_large_payloads() {
        let conversation = ConversationId::new([5; 32]);
        let mut bytes = encode_frame(&Frame::Supported { conversation }).unwrap();
        assert!(
            matches!(decode_frame(&bytes).unwrap(), Frame::Supported { conversation: actual } if actual == conversation)
        );
        bytes.push(0);
        assert!(decode_frame(&bytes).is_err());
        assert!(decode_frame(&vec![0; MAX_PLAINTEXT + 1]).is_err());
        assert!(decode_frame(b"MTOM2").is_err());
    }
}

/// Qualify a delta response before its cursor can be considered for local commit.
pub fn read_delta_response(
    bytes: &[u8],
    conversation: ConversationId,
) -> Result<MailboxDeltaPage, RpcError> {
    match decode_frame(bytes)? {
        Frame::Page(page)
            if page.cursor.conversation == conversation
                && page.ids.len() <= PAGE_IDS
                && page.complete == page.cursor.window.is_none()
                && page.cursor.after >= 0
                && page.cursor.revision >= 0
                && page.cursor.epoch >= 0 =>
        {
            Ok(page)
        }
        _ => Err(RpcError::UnexpectedMessage),
    }
}

pub fn make_get_request(
    conversation: ConversationId,
    cursor: &MailboxCursor,
    ids: &[EventId],
) -> Result<Vec<u8>, RpcError> {
    if ids.len() > PAGE_IDS || cursor.conversation != conversation {
        return Err(RpcError::UnexpectedMessage);
    }
    encode_frame(&Frame::Get {
        conversation,
        cursor: cursor.clone(),
        ids: ids.to_vec(),
    })
}

/// IDs and scope must match in order. Callers still authenticate each body
/// and commit acceptance before advancing local progress.
pub fn read_get_response(
    bytes: &[u8],
    conversation: ConversationId,
    ids: &[EventId],
    encoded_size: impl Fn(&Event) -> usize,
) -> Result<Vec<Event>, RpcError> {
    match decode_frame(bytes)? {
        Frame::Events {
            conversation: response,
            events,
        } if response == conversation
            && events.len() == ids.len()
            && events
                .iter()
                .zip(ids)
                .all(|(event, id)| event.id == *id && event.conversation_id == conversation)
            && events
                .iter()
                .try_fold(0usize, |sum, event| sum.checked_add(encoded_size(event)))
                .is_some_and(|bytes| bytes <= BODY_BUDGET) =>
        {
            Ok(events)
        }
        _ => Err(RpcError::UnexpectedMessage),
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;
    use mesh_identity::device::DeviceIdentity;
    use mesh_replication::legacy_event::EventKind;

    #[test]
    fn body_response_must_match_requested_scope_identity_and_budget() {
        let scope = ConversationId::new([7; 32]);
        let error_scope = ConversationId::new([8; 32]);
        let event = Event::new(
            &DeviceIdentity::generate(),
            scope,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![5],
        );
        let id = event.id;
        let bytes = encode_frame(&Frame::Events {
            conversation: scope,
            events: vec![event.clone()],
        })
        .unwrap();
        assert_eq!(
            read_get_response(&bytes, scope, &[id], |_| BODY_BUDGET).unwrap(),
            [event]
        );
        assert!(read_get_response(&bytes, scope, &[id], |_| BODY_BUDGET + 1).is_err());
        assert!(read_get_response(&bytes, error_scope, &[id], |_| 1).is_err());
        assert!(read_get_response(&bytes, scope, &[EventId::new([4; 32])], |_| 1).is_err());
    }
}
