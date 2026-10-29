//! Private account observations, never independently complete public DAG proofs.
use super::*;
use crate::{
    discovery::Announce,
    eventlog::{Event, EventKind},
    identity::device::DeviceIdentity,
};

const HEAD_DOMAIN: &[u8] = b"mesh-talk-account-group-head-v5";
pub(in crate::node) const MAX_HEAD_BYTES: usize = 8 * 1024 * 1024;
pub(in crate::node) const MAX_ACCESS_FRONTIER: usize = 4096;
pub(in crate::node) const MAX_OBSERVED_SOURCES: usize = 65;
pub(in crate::node) const MAX_OBSERVED_MEMBERS: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(in crate::node) struct BoundSource {
    pub source: AccountGroupSource,
    pub proof: Announce,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(in crate::node) struct AccountObservation {
    pub role: AccountGroupRole,
    pub sources: Vec<BoundSource>,
    /// Current public physical lifetimes let an individual leave preserve an
    /// unchanged other source, without treating absence as account-wide leave.
    pub members: Vec<AccountGroupSource>,
    pub owner: Option<PublicIdentity>,
    pub public_state: [u8; 32],
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(in crate::node) struct HeadReference {
    pub device: PublicIdentity,
    pub revision: u64,
    pub content_hash: [u8; 32],
}
pub(in crate::node) const MAX_PROVENANCE_CONTROLS: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(in crate::node) struct SourceAccess {
    pub source: AccountGroupSource,
    /// The actual public owner's independently signed account adoption, when known.
    pub owner_proof: Option<Announce>,
    /// Bounded original provenance anchors, not a redacted public event history.
    pub controls: Vec<Event>,
    pub observation: AccountObservation,
    pub acknowledgements: Vec<HeadReference>,
    /// A small negative replacement remains publishable when anchors exceed budget.
    pub unavailable: bool,
    pub ownership: Option<EventId>,
    pub frontier: Vec<EventId>,
    pub complete_frontier: bool,
    pub fingerprint: [u8; 32],
    pub event_count: u64,
    #[serde(skip)]
    pub local_frontier: Vec<EventId>,
}
impl SourceAccess {
    pub(in crate::node) fn make_unavailable(&mut self) {
        self.unavailable = true;
        self.controls.clear();
        self.owner_proof = None;
        self.observation.sources.clear();
        self.observation.members.clear();
        self.observation.role = AccountGroupRole::Unknown;
        self.acknowledgements.clear();
        self.complete_frontier &= self.frontier.len() <= 16;
        self.frontier.truncate(16);
    }
    /// Profile, endpoint and nonce changes do not change account adoption.
    fn same_binding(left: &Announce, right: &Announce) -> bool {
        left.public() == right.public() && left.account_id() == right.account_id()
    }
    pub(in crate::node) fn compatible(&self, other: &Self) -> bool {
        if self.unavailable
            || other.unavailable
            || self.observation.owner != other.observation.owner
            || self.ownership != other.ownership
        {
            return false;
        }
        if let (Some(left), Some(right)) = (&self.owner_proof, &other.owner_proof)
            && !Self::same_binding(left, right)
        {
            return false;
        }
        if !self.observation.sources.iter().all(|left| {
            other.observation.sources.iter().all(|right| {
                left.source.device != right.source.device
                    || left.source == right.source && Self::same_binding(&left.proof, &right.proof)
            })
        }) {
            return false;
        }
        if self.observation.public_state == other.observation.public_state {
            return true;
        }
        if self.source.role != AccountGroupRole::Left && other.source.role != AccountGroupRole::Left
        {
            return false;
        }
        let retained = |left: &Self, right: &Self| {
            left.observation.sources.iter().all(|bound| {
                if right.source.device == bound.source.device
                    && right.source.role == AccountGroupRole::Left
                {
                    return right.source.lifetime == bound.source.lifetime
                        && right.source.authority.authority == bound.source.authority.authority;
                }
                right.observation.members.iter().any(|current| {
                    current.device == bound.source.device
                        && current.role == bound.source.role
                        && current.lifetime == bound.source.lifetime
                        && current.authority.authority == bound.source.authority.authority
                })
            })
        };
        retained(self, other) && retained(other, self)
    }
    pub(in crate::node) fn same_canonical_observation(&self, other: &Self) -> bool {
        self.compatible(other)
            && self.source == other.source
            && self.observation.role == other.observation.role
            && self.observation.sources.len() == other.observation.sources.len()
            && self.owner_proof.is_some() == other.owner_proof.is_some()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct GroupControlHead {
    pub version: u8,
    pub group: ConversationId,
    pub revision: u64,
    pub proof: Announce,
    pub access: SourceAccess,
    signature: Vec<u8>,
    #[serde(skip)]
    content_hash: [u8; 32],
}
impl GroupControlHead {
    fn message(&self) -> Vec<u8> {
        let mut bytes = HEAD_DOMAIN.to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                self.version,
                self.group,
                self.revision,
                &self.proof,
                &self.access,
            ))
            .expect("head serializes"),
        );
        bytes
    }
    pub(in crate::node) fn new(
        identity: &DeviceIdentity,
        group: ConversationId,
        revision: u64,
        proof: Announce,
        access: SourceAccess,
    ) -> Self {
        let mut value = Self {
            version: 5,
            group,
            revision,
            proof,
            access,
            signature: Vec::new(),
            content_hash: [0; 32],
        };
        value.signature = identity.sign(&value.message()).to_vec();
        value.content_hash = crate::node::account_documents::content_hash(&value.encode());
        value
    }
    pub(in crate::node) fn reference(&self) -> HeadReference {
        HeadReference {
            device: self.proof.public(),
            revision: self.revision,
            content_hash: self.content_hash,
        }
    }
    pub(in crate::node) fn valid(&self, account: &[u8; 32]) -> bool {
        self.version == 5
            && self.revision > 0
            && own_binding(&self.proof, account)
            && self.access.source.device == self.proof.public()
            && self.access.source.execution == AccountGroupExecution::Local
            && <[u8; 64]>::try_from(self.signature.as_slice()).is_ok_and(|signature| {
                DeviceIdentity::verify(&self.proof.ed25519_pub, &self.message(), &signature)
            })
            && !self.access.frontier.is_empty()
            && self.access.frontier.len() <= MAX_ACCESS_FRONTIER
            && self
                .access
                .frontier
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && self.access.event_count > 0
            && self.access.acknowledgements.len() <= MAX_OBSERVED_SOURCES
            && self.access.acknowledgements.iter().all(|reference| {
                reference.revision > 0
                    && reference.content_hash != [0; 32]
                    && reference.device != self.proof.public()
            })
            && self
                .access
                .acknowledgements
                .windows(2)
                .all(|pair| pair[0].device.user_id() < pair[1].device.user_id())
            && if self.access.unavailable {
                self.access.controls.is_empty()
                    && self.access.observation.sources.is_empty()
                    && self.access.observation.role == AccountGroupRole::Unknown
                    && self.access.owner_proof.is_none()
            } else {
                self.available_valid(account)
            }
    }
    fn available_valid(&self, account: &[u8; 32]) -> bool {
        let access = &self.access;
        if access.controls.len() > MAX_PROVENANCE_CONTROLS
            || !access.controls.iter().all(|event| {
                event.conversation_id == self.group
                    && provenance_anchor(event.kind)
                    && event.verify_integrity()
                    && event.verify_signature()
            })
            || access.observation.sources.len() > MAX_OBSERVED_SOURCES
            || access.observation.members.len() > MAX_OBSERVED_MEMBERS
            || !access
                .observation
                .sources
                .windows(2)
                .all(|pair| pair[0].source.device.user_id() < pair[1].source.device.user_id())
            || !access.observation.sources.iter().all(|bound| {
                own_binding(&bound.proof, account)
                    && bound.source.device == bound.proof.public()
                    && bound.source.execution == AccountGroupExecution::Local
                    && matches!(
                        bound.source.role,
                        AccountGroupRole::Owner
                            | AccountGroupRole::Member
                            | AccountGroupRole::Invited
                    )
                    && (bound.source.authority.authority.is_some()
                        || bound.source.role != AccountGroupRole::Owner
                        || Some(bound.source.device.clone()) == access.observation.owner
                            && access
                                .owner_proof
                                .as_ref()
                                .is_some_and(|proof| Self::same_source_proof(proof, &bound.proof)))
            })
            || access.owner_proof.as_ref().is_some_and(|proof| {
                !proof.verify() || Some(proof.public()) != access.observation.owner
            })
        {
            return false;
        }
        let lifetime = &access.source.lifetime;
        if access.source.authority.authority.is_none() && lifetime.member != self.proof.public() {
            return false;
        }
        let membership = access
            .controls
            .iter()
            .find(|event| event.id == lifetime.membership);
        if !membership.is_some_and(|event| {
            event.kind == EventKind::MembershipChange
                && crate::channel::ChannelMeta::decode(&event.ciphertext).is_some_and(|meta| {
                    meta.members.contains(&lifetime.member)
                        && (access.source.authority.authority.is_some()
                            || meta.owner == event.author.user_id())
                })
        }) {
            return false;
        }
        if lifetime.acceptance.is_some_and(|id| {
            !access.controls.iter().any(|event| {
                event.id == id
                    && event.kind == EventKind::ChannelAccept
                    && (access.source.authority.authority.is_some()
                        || event.author.ed25519_pub() == &lifetime.member.ed25519_pub)
            })
        }) {
            return false;
        }
        if access.ownership.is_some_and(|id| {
            !access.controls.iter().any(|event| {
                event.id == id
                    && matches!(
                        event.kind,
                        EventKind::MembershipChange
                            | EventKind::ChannelOwnerTransfer
                            | EventKind::ChannelAccountAuthority
                    )
            })
        }) {
            return false;
        }
        if matches!(
            access.source.role,
            AccountGroupRole::Owner | AccountGroupRole::Member | AccountGroupRole::Invited
        ) && !access.observation.sources.iter().any(|bound| {
            bound.source == access.source && Self::same_source_proof(&bound.proof, &self.proof)
        }) {
            return false;
        }
        let expected = access
            .observation
            .sources
            .iter()
            .map(|bound| bound.source.role)
            .max_by_key(|role| super::access::rank(*role));
        match access.observation.role {
            AccountGroupRole::Owner | AccountGroupRole::Member | AccountGroupRole::Invited => {
                expected == Some(access.observation.role)
            }
            AccountGroupRole::Left => {
                expected.is_none() && access.source.role == AccountGroupRole::Left
            }
            AccountGroupRole::Dissolved => {
                access.source.role == AccountGroupRole::Dissolved
                    && access
                        .controls
                        .iter()
                        .any(|event| event.kind == EventKind::ChannelDissolve)
            }
            AccountGroupRole::Unknown => false,
        }
    }
    fn same_source_proof(left: &Announce, right: &Announce) -> bool {
        SourceAccess::same_binding(left, right)
    }
    pub(in crate::node) fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("head serializes")
    }
    pub(in crate::node) fn decode(bytes: &[u8], account: &[u8; 32]) -> Option<Self> {
        if bytes.len() > MAX_HEAD_BYTES {
            return None;
        }
        let mut value: Self = serde_json::from_slice(bytes).ok()?;
        value.content_hash = crate::node::account_documents::content_hash(bytes);
        value.valid(account).then_some(value)
    }
}
fn own_binding(proof: &Announce, account: &[u8; 32]) -> bool {
    proof.verify()
        && proof
            .account_cert
            .as_ref()
            .is_some_and(|certificate| certificate.account_ed25519_pub == *account)
}
pub(in crate::node) fn provenance_anchor(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::MembershipChange
            | EventKind::ChannelOwnerTransfer
            | EventKind::ChannelLeave
            | EventKind::ChannelAccept
            | EventKind::ChannelDissolve
            | EventKind::ChannelAccountAuthority
    )
}

/// Identifier-only causal metadata includes inheritance, without exporting its
/// sealed bootstrap payload as a public provenance anchor.
pub(in crate::node) fn authorization_control(kind: EventKind) -> bool {
    provenance_anchor(kind) || kind == EventKind::ChannelDeviceInheritance
}
