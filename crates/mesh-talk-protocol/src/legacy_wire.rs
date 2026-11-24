//! Historical Mesh Talk synchronization frame schemas.
//! Keep enum variant order and bincode options stable across mixed deployments.

use crate::legacy_event::{ConversationId, Event, EventId};
use bincode::Options;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_PLAINTEXT: usize = 65_519;

/// Sent by the peer initiating sync: "for this conversation, here are the ids I have."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub conversation: ConversationId,
    pub have: Vec<EventId>,
}

/// The responder's reply: the events the requester is missing (topological
/// order), plus the responder's own id-set so the requester can reciprocate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
    /// The responder's OWN event ids (note: `SyncRequest::have` is the
    /// requester's). The requester uses these to compute the events the
    /// responder is missing, returned via [`SyncFollowup`].
    pub have: Vec<EventId>,
}

/// The requester's final message: the events the responder is missing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncFollowup {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
}

/// A whole-conversation fingerprint probe sent before the id-set exchange: "here is a
/// digest of my ENTIRE event-id set for this conversation." If the responder's digest
/// matches, the two logs are identical and the O(N) have-set streaming is skipped — the
/// common case on an idle drain tick (nothing changed since last sync).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpRequest {
    pub conversation: ConversationId,
    pub fingerprint: [u8; 32],
}

/// The responder's verdict on an [`FpRequest`]: `true` ⇒ our id-sets are identical, skip
/// reconciliation; `false` ⇒ fall through to the full id-set exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpResponse {
    pub matched: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FileConsentStatus {
    Pending,
    Accepted,
    Declined,
}

/// One framed sync message on the wire.
///
/// The requester's and responder's `have` id-sets are streamed as a sequence of
/// `ReqHave`/`RespHave` chunk frames (terminated by `more == false`) rather than
/// inlined into `Request`/`Response`, so a conversation with more event-ids than
/// fit one frame (~2040) still reconciles. `Request`/`Response` therefore carry an
/// empty `have` on this networked path (the in-process `reconcile` keeps inlining it).
#[derive(Debug, Serialize, Deserialize)]
pub enum SyncWire {
    Request(SyncRequest),
    Response(SyncResponse),
    Followup(SyncFollowup),
    ReqHave(HaveChunk),
    RespHave(HaveChunk),
    // Appended (stable discriminants) for the whole-conversation fingerprint short-circuit.
    FpRequest(FpRequest),
    FpResponse(FpResponse),
    // Optional final custody probe. Legacy endpoints close this disposable
    // connection; ordinary reconciliation remains byte-for-byte compatible.
    DurableHaveRequest {
        conversation: ConversationId,
        event_id: EventId,
    },
    DurableHaveResponse {
        conversation: ConversationId,
        event_id: EventId,
        stored: bool,
    },
    FileConsentRequest {
        file: ConversationId,
        manifest: EventId,
    },
    FileConsentResponse {
        file: ConversationId,
        manifest: EventId,
        status: FileConsentStatus,
    },
    /// Appended variants gate kind-14 events from pre-replica builds. Older
    /// decoders close on these before receiving an unsupported event.
    FpRequestV2(FpRequest),
    RequestV2(SyncRequest),
    FollowupV2(SyncFollowup),
    UpgradeRequired {
        conversation: ConversationId,
    },
    SignedFileConsentRequest {
        file: ConversationId,
        manifest: EventId,
    },
    SignedFileConsentResponse {
        file: ConversationId,
        manifest: EventId,
        control: Option<Event>,
    },
    /// History policy and key release require support for kinds 15 and 16.
    /// Keep these appended so every earlier envelope retains its wire tag.
    FpRequestV3(FpRequest),
    RequestV3(SyncRequest),
    FollowupV3(SyncFollowup),
    /// Account-principal membership and certified-device inheritance (17/18).
    FpRequestV4(FpRequest),
    RequestV4(SyncRequest),
    FollowupV4(SyncFollowup),
    /// Optional local-artwork reuse; old decoders close this disposable session.
    StickerFileConsentRequest {
        file: ConversationId,
        manifest: EventId,
        headers: Vec<Event>,
    },
    StickerFileConsentResponse {
        file: ConversationId,
        manifest: EventId,
        control: Option<Event>,
    },
    StickerFileProofRequest {
        file: ConversationId,
        manifest: EventId,
    },
    StickerFileProofResponse {
        file: ConversationId,
        manifest: EventId,
        headers: Option<Vec<Event>>,
    },
    /// Appended optional metadata exchange; MFM3 and history card bytes stay frozen.
    FileMessageIdentitiesRequest {
        requests: Vec<(ConversationId, EventId)>,
        offers: Vec<Event>,
    },
    FileMessageIdentitiesResponse {
        bundles: Vec<Event>,
    },
    /// Encrypted owner group metadata requires kind 19.
    FpRequestV5(FpRequest),
    RequestV5(SyncRequest),
    FollowupV5(SyncFollowup),
}

/// One chunk of a streamed `have` id-set.
#[derive(Debug, Serialize, Deserialize)]
pub struct HaveChunk {
    pub conversation: ConversationId,
    pub ids: Vec<EventId>,
    /// `true` if more chunks of this id-set follow.
    pub more: bool,
}

pub fn encode(wire: &SyncWire) -> Result<Vec<u8>, String> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .serialize(wire)
        .map_err(|e| e.to_string())
}

pub fn decode(bytes: &[u8]) -> Result<SyncWire, String> {
    if bytes.len() > MAX_PLAINTEXT {
        return Err("oversized sync frame".into());
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_request_and_fingerprint_reply_retain_their_tags() {
        let conversation = ConversationId::new([1; 32]);
        let request = encode(&SyncWire::Request(SyncRequest {
            conversation,
            have: Vec::new(),
        }))
        .unwrap();
        assert_eq!(&request[..4], &[0, 0, 0, 0]);
        assert_eq!(&request[4..36], &[1; 32]);
        assert_eq!(&request[36..], &[0; 8]);
        assert_eq!(
            encode(&SyncWire::FpResponse(FpResponse { matched: true })).unwrap(),
            [6, 0, 0, 0, 1]
        );
        assert!(
            matches!(decode(&request).unwrap(), SyncWire::Request(r) if r.conversation == conversation)
        );
        let mut trailing = request;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        assert!(decode(&vec![0; MAX_PLAINTEXT + 1]).is_err());
    }
}
