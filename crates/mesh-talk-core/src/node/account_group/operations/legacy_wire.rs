//! Separately signed JSON child transport; ordinary references and WAL bytes stay unchanged.
use super::{
    journal::OperationReference,
    wire::{RequestProjection, ResultProjection},
    *,
};
use crate::{
    discovery::Announce,
    identity::device::DeviceIdentity,
    node::{AccountDocumentKey, AccountDocumentMeta},
};
use sha2::{Digest, Sha256};

pub(in crate::node) const LEGACY_LEAVE_CAPABILITY: u16 = 1 << 14;
pub(in crate::node) const MAX_LEGACY_HEAD: usize = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct LegacyLegRequest {
    pub parent: OperationReference,
    pub request: RequestProjection,
}
#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct LegacyLegResult {
    pub parent: OperationReference,
    pub result: ResultProjection,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(in crate::node) struct LegacyOperationProjection {
    pub requests: Vec<LegacyLegRequest>,
    pub results: Vec<LegacyLegResult>,
}
impl LegacyOperationProjection {
    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(serde_json::to_vec(self).expect("legacy projection serializes")).into()
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct LegacyOperationFrame {
    version: u8,
    pub group: ConversationId,
    pub revision: u64,
    pub proof: Announce,
    pub projection: LegacyOperationProjection,
    signature: Vec<u8>,
}
impl LegacyOperationFrame {
    fn message(&self) -> Vec<u8> {
        let mut bytes = b"mesh-talk-account-group-legacy-operation-head-v1".to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                self.version,
                self.group,
                self.revision,
                &self.proof,
                &self.projection,
            ))
            .expect("legacy operation head serializes"),
        );
        bytes
    }
    pub fn new(
        node: &Node,
        group: ConversationId,
        revision: u64,
        projection: LegacyOperationProjection,
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
        serde_json::to_vec(self).expect("legacy operation frame serializes")
    }
    pub fn decode(bytes: &[u8], account: &[u8; 32]) -> Option<Self> {
        if bytes.len() > MAX_LEGACY_HEAD {
            return None;
        }
        let frame: Self = serde_json::from_slice(bytes).ok()?;
        let signature: [u8; 64] = frame.signature.as_slice().try_into().ok()?;
        if frame.version != 1
            || frame.revision == 0
            || !frame.proof.verify()
            || frame.proof.post_office
            || frame
                .proof
                .account_cert
                .as_ref()
                .is_none_or(|cert| &cert.account_ed25519_pub != account)
            || !DeviceIdentity::verify(&frame.proof.ed25519_pub, &frame.message(), &signature)
            || frame.projection.requests.len() > 8
            || frame.projection.results.len() > 16
        {
            return None;
        }
        let valid_pair = |parent: &OperationReference, child: &OperationReference| {
            parent.valid(account)
                && child.valid(account)
                && parent.id != child.id
                && parent.group == frame.group
                && child.group == frame.group
                && parent.kind == AccountGroupOperationKind::LeaveAccount
                && child.kind == parent.kind
                && parent.executor.authority.authority.is_none()
                && child.executor.authority.authority.is_none()
                && parent.executor.device == child.origin.public()
                && parent.executor.lifetime.member == parent.executor.device
                && child.executor.lifetime.member == child.executor.device
                && parent.executor.role == AccountGroupRole::Member
                && child.executor.role == AccountGroupRole::Member
                && parent.digest
                    == super::journal::operation_digest(
                        frame.group,
                        &AccountGroupOperation::LeaveAccount,
                    )
                && child.digest == parent.digest
                && serde_json::to_vec(parent).is_ok_and(|v| v.len() <= 16 * 1024)
                && serde_json::to_vec(child).is_ok_and(|v| v.len() <= 16 * 1024)
        };
        let mut requests = std::collections::HashSet::new();
        for leg in &frame.projection.requests {
            if !valid_pair(&leg.parent, &leg.request.reference)
                || leg.parent.executor.device != frame.proof.public()
                || !requests.insert(leg.request.reference.id)
                || leg
                    .request
                    .body
                    .as_ref()
                    .is_some_and(|body| !matches!(body, AccountGroupOperation::LeaveAccount))
            {
                return None;
            }
        }
        let account_id = crate::identity::account::AccountPublic {
            ed25519_pub: *account,
        }
        .account_id();
        let mut results = std::collections::HashSet::new();
        for leg in &frame.projection.results {
            let result = &leg.result;
            let status = &result.status;
            if !valid_pair(&leg.parent, &result.reference)
                || result.reference.executor.device != frame.proof.public()
                || !results.insert(result.reference.id)
                || status.id != result.reference.id
                || status.group != frame.group
                || status.kind != AccountGroupOperationKind::LeaveAccount
                || status.account_id != account_id
                || status.executions.len() != 1
                || status.executions[0].device_id != frame.proof.user_id
                || status.executions[0].phase != status.phase
                || status.committed_events.len() > 1
                || status.executions[0].committed_events != status.committed_events
                || status
                    .primary_event
                    .is_some_and(|id| !status.committed_events.contains(&id))
                || result.message_event.is_some()
                || result.file_descriptor.is_some()
                || result.recall_mark.is_some()
                || matches!(
                    status.phase,
                    AccountGroupOperationPhase::WaitingForExecutor
                        | AccountGroupOperationPhase::AwaitingAcceptance
                )
                || (matches!(
                    status.phase,
                    AccountGroupOperationPhase::Cancelled
                        | AccountGroupOperationPhase::Rejected { .. }
                ) && !status.committed_events.is_empty())
            {
                return None;
            }
        }
        Some(frame)
    }
}
pub(in crate::node) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    let AccountDocumentKey::GroupLegacyOperation {
        channel,
        source_device,
    } = &meta.key
    else {
        return false;
    };
    if meta.tombstone {
        return payload.is_empty();
    }
    LegacyOperationFrame::decode(payload, &meta.account_public).is_some_and(|frame| {
        hex::encode(frame.group.as_bytes()) == *channel
            && frame.proof.user_id == *source_device
            && frame.proof.user_id == meta.writer_device
            && frame.revision == meta.revision
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::Roster,
        identity::account::Account,
        node::account_group::{AccountGroupExecution, AuthorityExpectation, MembershipLifetime},
    };
    use std::sync::{Arc, Mutex};

    fn node(path: &std::path::Path, account: &Account) -> Arc<Node> {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (files, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open_with_account(
            DeviceIdentity::generate(),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            files,
            &path.join("events.log"),
            &path.join("sent.log"),
            "pw",
        )
        .unwrap()
    }
    fn physical(node: &Node) -> AccountGroupSource {
        AccountGroupSource {
            device: node.identity.public(),
            role: AccountGroupRole::Member,
            lifetime: MembershipLifetime {
                member: node.identity.public(),
                membership: EventId::new([9; 32]),
                acceptance: None,
            },
            authority: AuthorityExpectation {
                authority: None,
                epoch: 1,
            },
            execution: AccountGroupExecution::Local,
        }
    }
    #[test]
    fn legacy_children_require_exact_parent_coordinator_and_signed_full_source() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let requester = node(&dir.path().join("requester"), &account);
        let coordinator = node(&dir.path().join("coordinator"), &account);
        let executor = node(&dir.path().join("executor"), &account);
        let group = ConversationId::new([7; 32]);
        let parent = OperationReference::new(
            &requester,
            requester
                .new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &AccountGroupOperation::LeaveAccount,
            physical(&coordinator),
        );
        let child = OperationReference::new(
            &coordinator,
            coordinator
                .new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &AccountGroupOperation::LeaveAccount,
            physical(&executor),
        );
        let projection = LegacyOperationProjection {
            requests: vec![LegacyLegRequest {
                parent: parent.clone(),
                request: RequestProjection {
                    reference: child.clone(),
                    body: Some(AccountGroupOperation::LeaveAccount),
                    cancel: false,
                    nonce: [1; 16],
                },
            }],
            results: vec![],
        };
        let frame = LegacyOperationFrame::new(&coordinator, group, 1, projection.clone());
        assert!(
            LegacyOperationFrame::decode(&frame.encode(), &account.public().ed25519_pub).is_some()
        );
        let wrong_writer = LegacyOperationFrame::new(&requester, group, 1, projection.clone());
        assert!(
            LegacyOperationFrame::decode(&wrong_writer.encode(), &account.public().ed25519_pub)
                .is_none()
        );
        let mut tampered = frame.clone();
        tampered.proof.x25519_pub[0] ^= 1;
        assert!(
            LegacyOperationFrame::decode(&tampered.encode(), &account.public().ed25519_pub)
                .is_none()
        );
        let mut duplicate = projection;
        duplicate.requests.push(duplicate.requests[0].clone());
        let duplicate = LegacyOperationFrame::new(&coordinator, group, 1, duplicate);
        assert!(
            LegacyOperationFrame::decode(&duplicate.encode(), &account.public().ed25519_pub)
                .is_none()
        );
        let effect = EventId::new([5; 32]);
        let status = AccountGroupOperationStatus {
            id: child.id,
            account_id: account.account_id(),
            group,
            kind: AccountGroupOperationKind::LeaveAccount,
            phase: AccountGroupOperationPhase::Committed,
            executions: vec![AccountGroupOperationExecution {
                device_id: executor.user_id(),
                phase: AccountGroupOperationPhase::Committed,
                committed_events: vec![effect],
            }],
            committed_events: vec![effect],
            primary_event: None,
        };
        let result = LegacyLegResult {
            parent,
            result: ResultProjection {
                reference: child,
                status,
                message_event: None,
                file_descriptor: None,
                recall_mark: None,
            },
        };
        let result_projection = LegacyOperationProjection {
            requests: vec![],
            results: vec![result],
        };
        let result_frame =
            LegacyOperationFrame::new(&executor, group, 1, result_projection.clone());
        assert!(
            LegacyOperationFrame::decode(&result_frame.encode(), &account.public().ed25519_pub)
                .is_some()
        );
        let mut retired_projection = result_projection.clone();
        let retired = &mut retired_projection.results[0].result.status;
        retired.committed_events.clear();
        retired.executions[0].committed_events.clear();
        retired.primary_event = None;
        let retired = LegacyOperationFrame::new(&executor, group, 1, retired_projection.clone());
        assert!(
            LegacyOperationFrame::decode(&retired.encode(), &account.public().ed25519_pub)
                .is_some(),
            "a signed source may attest an already retired original scope without inventing an event"
        );
        retired_projection.results[0].result.status.primary_event = Some(effect);
        let invalid = LegacyOperationFrame::new(&executor, group, 1, retired_projection);
        assert!(
            LegacyOperationFrame::decode(&invalid.encode(), &account.public().ed25519_pub)
                .is_none()
        );
        let forged = LegacyOperationFrame::new(&coordinator, group, 1, result_projection);
        assert!(
            LegacyOperationFrame::decode(&forged.encode(), &account.public().ed25519_pub).is_none()
        );
    }
}
