//! Bounded private operation projections. They never establish public membership.
use super::{
    journal::{OperationReference, operation_digest},
    *,
};
use crate::{discovery::Announce, identity::device::DeviceIdentity, node::AccountDocumentMeta};
use sha2::{Digest, Sha256};

pub(in crate::node) const MAX_HEAD: usize = 4 * 1024 * 1024;
pub(super) const MAX_BODY: usize = 256 * 1024;
const MAX_REFERENCE: usize = 16 * 1024;
const MAX_STATUS: usize = 64 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct RequestProjection {
    pub reference: OperationReference,
    pub body: Option<AccountGroupOperation>,
    pub cancel: bool,
    pub nonce: [u8; 16],
}
#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct ResultProjection {
    pub reference: OperationReference,
    pub status: AccountGroupOperationStatus,
    /// The original signed ciphertext, never a second plaintext result body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_event: Option<crate::eventlog::Event>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_descriptor: Option<super::file_preparation::GroupFileDescriptor>,
    /// The original source's durable tombstone, without a recalled message body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_mark: Option<crate::node::recall::RecallMark>,
}
impl ResultProjection {
    pub(super) fn valid_message_event(&self) -> bool {
        if let Some(mark) = &self.recall_mark
            && (self.reference.kind != AccountGroupOperationKind::Recall
                || self.message_event.is_none()
                || mark.conversation != self.reference.group
                || operation_digest(
                    mark.conversation,
                    &AccountGroupOperation::Recall {
                        target: mark.target,
                    },
                ) != self.reference.digest
                || self.message_event.as_ref().is_none_or(|event| {
                    event.wall_clock.saturating_sub(mark.wall_clock)
                        > crate::node::recall::RECALL_WINDOW_MS
                }))
        {
            return false;
        }
        if let Some(descriptor) = &self.file_descriptor {
            let Some(logical) =
                super::file_preparation::GroupFileLogical::from_manifest(&descriptor.manifest)
            else {
                return false;
            };
            let payload = super::file_preparation::GroupFilePayload {
                logical,
                descriptor: None,
            };
            if self.reference.kind != AccountGroupOperationKind::File
                || self.message_event.is_none()
                || !descriptor.valid_for(&self.reference, &payload.logical)
                || payload.logical_operation().is_none_or(|body| {
                    operation_digest(self.reference.group, &body) != self.reference.digest
                })
            {
                return false;
            }
        }
        self.message_event.as_ref().is_none_or(|event| {
            matches!(
                self.reference.kind,
                AccountGroupOperationKind::Text
                    | AccountGroupOperationKind::Sticker
                    | AccountGroupOperationKind::File
                    | AccountGroupOperationKind::Reaction
                    | AccountGroupOperationKind::Recall
            ) && self.status.phase == AccountGroupOperationPhase::Committed
                && self.status.primary_event == Some(event.id)
                && self.status.committed_events.contains(&event.id)
                && event.conversation_id == self.reference.group
                && match self.reference.kind {
                    AccountGroupOperationKind::File => {
                        self.file_descriptor.is_some()
                            && event.kind == crate::eventlog::EventKind::FileManifest
                    }
                    AccountGroupOperationKind::Reaction => {
                        event.kind == crate::eventlog::EventKind::React
                    }
                    AccountGroupOperationKind::Recall => {
                        self.recall_mark.is_some()
                            && event.kind == crate::eventlog::EventKind::Delete
                    }
                    _ => event.kind == crate::eventlog::EventKind::Message,
                }
                && event.author.ed25519_pub() == &self.reference.executor.device.ed25519_pub
                && event.seq > 0
                && event.lamport > 0
                && !event.parents.is_empty()
                && event.is_canonical()
                && event.verify_integrity()
                && event.verify_signature()
                && crate::node::session::event_fits_frame(event)
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct LookupProjection {
    pub id: AccountGroupOperationId,
    pub group: ConversationId,
    pub kind: AccountGroupOperationKind,
    pub digest: [u8; 32],
    pub cancel: bool,
    pub nonce: [u8; 16],
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(in crate::node) struct OperationProjection {
    pub requests: Vec<RequestProjection>,
    pub results: Vec<ResultProjection>,
    pub lookups: Vec<LookupProjection>,
}
impl OperationProjection {
    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(serde_json::to_vec(self).expect("operation projection serializes")).into()
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct OperationFrame {
    version: u8,
    pub group: ConversationId,
    pub revision: u64,
    pub proof: Announce,
    pub projection: OperationProjection,
    signature: Vec<u8>,
}
impl OperationFrame {
    fn message(&self) -> Vec<u8> {
        let mut bytes = b"mesh-talk-account-group-operation-head-v1".to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                self.version,
                self.group,
                self.revision,
                &self.proof,
                &self.projection,
            ))
            .expect("operation head serializes"),
        );
        bytes
    }
    pub fn new(
        node: &Node,
        group: ConversationId,
        revision: u64,
        projection: OperationProjection,
    ) -> Self {
        let mut frame = Self {
            version: 1,
            group,
            revision,
            proof: node.own_group_proof(),
            projection,
            signature: Vec::new(),
        };
        frame.signature = node.identity.sign(&frame.message()).to_vec();
        frame
    }
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("operation head serializes")
    }
    pub fn decode(bytes: &[u8], account: &[u8; 32]) -> Option<Self> {
        #[cfg(test)]
        super::note_frame_decode_for_test(false);
        if bytes.len() > MAX_HEAD {
            return None;
        }
        let frame: Self = serde_json::from_slice(bytes).ok()?;
        let signature: [u8; 64] = frame.signature.as_slice().try_into().ok()?;
        if frame.version != 1
            || frame.revision == 0
            || !frame.proof.verify()
            || frame
                .proof
                .account_cert
                .as_ref()
                .is_none_or(|cert| &cert.account_ed25519_pub != account)
            || !DeviceIdentity::verify(&frame.proof.ed25519_pub, &frame.message(), &signature)
            || frame.projection.requests.len() > 8
            || frame.projection.lookups.len() > 8
            || frame.projection.results.len() > 16
        {
            return None;
        }
        let valid_reference = |reference: &OperationReference| {
            reference.group == frame.group
                && reference.valid(account)
                && serde_json::to_vec(reference).is_ok_and(|bytes| bytes.len() <= MAX_REFERENCE)
        };
        if frame.projection.requests.iter().any(|request| {
            !valid_reference(&request.reference)
                || request.body.as_ref().is_some_and(|body| {
                    body.kind() != request.reference.kind
                        || operation_digest(frame.group, body) != request.reference.digest
                        || !serde_json::to_vec(body).is_ok_and(|bytes| bytes.len() <= MAX_BODY)
                })
        }) {
            return None;
        }
        let account_id = crate::identity::account::AccountPublic {
            ed25519_pub: *account,
        }
        .account_id();
        for result in &frame.projection.results {
            let status = &result.status;
            if !valid_reference(&result.reference)
                || !result.valid_message_event()
                || result.reference.executor.device != frame.proof.public()
                || status.id != result.reference.id
                || status.group != frame.group
                || status.kind != result.reference.kind
                || status.account_id != account_id
                || status.committed_events.len() > 65
                || status.executions.len() > 65
                || status
                    .primary_event
                    .is_some_and(|id| !status.committed_events.contains(&id))
                || status.executions.iter().any(|execution| {
                    execution.device_id.len() != 32 || execution.committed_events.len() > 65
                })
                || !serde_json::to_vec(status).is_ok_and(|bytes| bytes.len() <= MAX_STATUS)
                || matches!(
                    status.phase,
                    AccountGroupOperationPhase::WaitingForExecutor
                        | AccountGroupOperationPhase::AwaitingAcceptance
                )
                || matches!(
                    status.phase,
                    AccountGroupOperationPhase::Cancelled
                        | AccountGroupOperationPhase::Rejected { .. }
                ) && (!status.committed_events.is_empty()
                    || status
                        .executions
                        .iter()
                        .any(|execution| !execution.committed_events.is_empty()))
            {
                return None;
            }
        }
        if frame
            .projection
            .lookups
            .iter()
            .any(|lookup| lookup.group != frame.group || lookup.id.as_bytes() == &[0; 32])
        {
            return None;
        }
        Some(frame)
    }
}
pub(in crate::node) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    let crate::node::AccountDocumentKey::GroupOperation {
        channel,
        source_device,
    } = &meta.key
    else {
        return false;
    };
    !meta.tombstone
        && OperationFrame::decode(payload, &meta.account_public).is_some_and(|frame| {
            hex::encode(frame.group.as_bytes()) == *channel
                && frame.proof.public().user_id() == *source_device
                && meta.writer_device == *source_device
                && meta.revision == frame.revision
        })
}
