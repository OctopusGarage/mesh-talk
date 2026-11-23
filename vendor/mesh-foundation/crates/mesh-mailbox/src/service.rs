//! Dispatch for the existing authenticated mailbox delta protocol. The host
//! owns authentication, per-session negotiation and the transaction/permit.

use crate::rpc::{
    BODY_BUDGET, Frame, MailboxCursor, MailboxDeltaPage, PAGE_IDS, RpcError, decode_frame,
    encode_frame, is_mailbox_frame, marker,
};
use mesh_replication::{
    legacy_event::{ConversationId, Event, EventId},
    legacy_wire::{self, FpRequest, FpResponse, SyncWire},
};

pub fn make_probe(scope: ConversationId) -> Result<Vec<u8>, RpcError> {
    legacy_wire::encode(&SyncWire::FpRequest(FpRequest {
        conversation: scope,
        fingerprint: marker(scope),
    }))
    .map_err(RpcError::Serialization)
}

/// Identify a probe, leaving any ordinary synchronization frame to its owner.
pub fn probe_scope(bytes: &[u8]) -> Option<ConversationId> {
    let SyncWire::FpRequest(probe) = legacy_wire::decode(bytes).ok()? else {
        return None;
    };
    (probe.fingerprint == marker(probe.conversation)).then_some(probe.conversation)
}

pub fn read_probe_response(bytes: &[u8], scope: ConversationId) -> Result<bool, RpcError> {
    if is_mailbox_frame(bytes) {
        return match decode_frame(bytes)? {
            Frame::Supported { conversation } if conversation == scope => Ok(true),
            _ => Err(RpcError::UnexpectedMessage),
        };
    }
    match legacy_wire::decode(bytes).map_err(RpcError::Serialization)? {
        SyncWire::FpResponse(FpResponse { matched: false }) => Ok(false),
        _ => Err(RpcError::UnexpectedMessage),
    }
}

/// Current authorized relay store. Implementations must recheck retention,
/// cursor identity, and current authority under the same read transaction.
pub trait MailboxStore {
    type Error;

    fn delta(
        &self,
        scope: &ConversationId,
        prior: Option<&MailboxCursor>,
        limit: usize,
    ) -> Result<MailboxDeltaPage, Self::Error>;

    fn bodies(
        &self,
        scope: &ConversationId,
        cursor: &MailboxCursor,
        ids: &[EventId],
        max_bytes: usize,
    ) -> Result<Vec<Event>, Self::Error>;
}

pub struct MailboxReply {
    pub scope: ConversationId,
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
    authorized: Option<ConversationId>,
    bytes: &[u8],
) -> Result<MailboxReply, DispatchError<S::Error>> {
    let request = decode_frame(bytes).map_err(DispatchError::Wire)?;
    let (scope, response) = match request {
        Frame::Delta {
            conversation,
            prior,
        } if authorized == Some(conversation) => {
            let page = store
                .delta(&conversation, prior.as_ref(), PAGE_IDS)
                .map_err(DispatchError::Store)?;
            (conversation, Frame::Page(page))
        }
        Frame::Get {
            conversation,
            cursor,
            ids,
        } if authorized == Some(conversation)
            && cursor.conversation == conversation
            && ids.len() <= PAGE_IDS =>
        {
            let events = store
                .bodies(&conversation, &cursor, &ids, BODY_BUDGET)
                .map_err(DispatchError::Store)?;
            (
                conversation,
                Frame::Events {
                    conversation,
                    events,
                },
            )
        }
        _ => return Err(DispatchError::Wire(RpcError::UnexpectedMessage)),
    };
    Ok(MailboxReply {
        scope,
        bytes: encode_frame(&response).map_err(DispatchError::Wire)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Store(Cell<usize>);

    impl MailboxStore for Store {
        type Error = &'static str;

        fn delta(
            &self,
            _: &ConversationId,
            _: Option<&MailboxCursor>,
            _: usize,
        ) -> Result<MailboxDeltaPage, Self::Error> {
            self.0.set(self.0.get() + 1);
            Err("store unavailable")
        }

        fn bodies(
            &self,
            _: &ConversationId,
            _: &MailboxCursor,
            _: &[EventId],
            _: usize,
        ) -> Result<Vec<Event>, Self::Error> {
            self.0.set(self.0.get() + 1);
            Err("store unavailable")
        }
    }

    #[test]
    fn negotiated_scope_precedes_storage_and_errors_stay_typed() {
        let store = Store(Cell::new(0));
        let scope = ConversationId::new([7; 32]);
        let request = encode_frame(&Frame::Delta {
            conversation: scope,
            prior: None,
        })
        .unwrap();
        assert!(matches!(
            dispatch(&store, None, &request),
            Err(DispatchError::Wire(RpcError::UnexpectedMessage))
        ));
        assert!(matches!(
            dispatch(&store, Some(ConversationId::new([8; 32])), &request),
            Err(DispatchError::Wire(RpcError::UnexpectedMessage))
        ));
        assert_eq!(store.0.get(), 0);
        assert!(matches!(
            dispatch(&store, Some(scope), &request),
            Err(DispatchError::Store("store unavailable"))
        ));
        assert_eq!(store.0.get(), 1);
    }

    #[test]
    fn supported_reply_must_bind_scope_and_legacy_negative_is_the_only_fallback() {
        let scope = ConversationId::new([7; 32]);
        assert_eq!(probe_scope(&make_probe(scope).unwrap()), Some(scope));
        let supported = encode_frame(&Frame::Supported {
            conversation: scope,
        })
        .unwrap();
        assert!(read_probe_response(&supported, scope).unwrap());
        assert!(read_probe_response(&supported, ConversationId::new([8; 32])).is_err());
        let negative =
            legacy_wire::encode(&SyncWire::FpResponse(FpResponse { matched: false })).unwrap();
        assert!(!read_probe_response(&negative, scope).unwrap());
        let positive =
            legacy_wire::encode(&SyncWire::FpResponse(FpResponse { matched: true })).unwrap();
        assert!(read_probe_response(&positive, scope).is_err());
    }
}
