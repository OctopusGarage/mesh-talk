//! Certified-device participation under an owner-authorized account grant.
//! Inheritance establishes device identity; encrypted receive snapshots never
//! grant membership or install another device's sending chain.
use super::channel_account_authority::AccountGrantId;
use crate::node::NodeError;
use crate::{
    channel::sender_key::SenderKeyDistribution,
    discovery::Announce,
    eventlog::{ConversationId, Event, EventId, LogError},
    identity::device::PublicIdentity,
};
use serde::{Deserialize, Serialize};

const CONTROL_MAGIC: &[u8] = b"MTGI1";
const REQUEST_MAGIC: &[u8] = b"MTGA1";
const RESPONSE_MAGIC: &[u8] = b"MTGB1";
const MAX_CONTROL: usize = 24 * 1024;
const MAX_CHAINS: usize = 64;
const MAX_GROUPS: usize = 64;
const MAX_BINDING_PATCHES: usize = 4;
const MAX_BINDING_PROOFS: usize = 8;

fn valid_device_cursor(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn valid_binding_patch(patch: &super::peer_directory::AccountBindingPatch) -> bool {
    let public = patch.announce.public();
    !patch.transitions.is_empty()
        && patch.announce.valid_presence()
        && !patch.announce.post_office
        && patch
            .transitions
            .iter()
            .all(|transition| transition.public == public && transition.verify())
        && patch.transitions.windows(2).all(|pair| {
            pair[1].previous_transition == Some(pair[0].id())
                && pair[1].previous_certificate == pair[0].current_certificate
        })
        && patch.announce.account_cert.as_ref()
            == patch
                .transitions
                .last()
                .map(|transition| &transition.current_certificate)
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) enum DeviceControl {
    Inherit {
        authority: EventId,
        grant: AccountGrantId,
        device: Announce,
        account_signature: Vec<u8>,
    },
    ReceiveKeys {
        authority: EventId,
        inheritance: EventId,
        grant: AccountGrantId,
        recipient: PublicIdentity,
        epoch: u64,
        authors: Vec<PublicIdentity>,
        positions: Vec<u32>,
        sealed: Vec<u8>,
    },
}

#[derive(Serialize, Deserialize)]
pub(super) struct ReceiveDistribution {
    pub author: PublicIdentity,
    pub epoch: u64,
    pub distribution: SenderKeyDistribution,
}
impl Drop for ReceiveDistribution {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.distribution.chain_key);
    }
}

#[derive(Serialize, Deserialize)]
pub(super) enum AccountGroupRequest {
    Capability,
    Groups {
        account_id: String,
        after: Option<ConversationId>,
    },
    BindingTransitions {
        channel: ConversationId,
        after: Option<String>,
    },
}
#[derive(Serialize, Deserialize)]
pub(super) enum AccountGroupResponse {
    Capability {
        version: u8,
        proof: Box<Announce>,
    },
    Groups {
        channels: Vec<ConversationId>,
        next: Option<ConversationId>,
    },
    BindingTransitions {
        patches: Vec<super::peer_directory::AccountBindingPatch>,
        next: Option<String>,
    },
}

fn encode<T: Serialize>(magic: &[u8], value: &T) -> Vec<u8> {
    super::wire::frame(magic, value)
}
fn decode<T: serde::de::DeserializeOwned>(magic: &[u8], bytes: &[u8]) -> Option<T> {
    if bytes.len() > MAX_CONTROL {
        return None;
    }
    super::wire::unframe(magic, bytes)
}
pub(super) fn inheritance_account_message(
    channel: ConversationId,
    authority: EventId,
    grant: AccountGrantId,
    device: &PublicIdentity,
) -> Vec<u8> {
    let mut message = b"mesh-talk-channel-inheritance-account-v1".to_vec();
    message.extend_from_slice(channel.as_bytes());
    message.extend_from_slice(authority.as_bytes());
    message.extend_from_slice(&grant.account_pub);
    message.extend_from_slice(&grant.incarnation);
    message.extend_from_slice(&device.ed25519_pub);
    message.extend_from_slice(&device.x25519_pub);
    message
}
impl DeviceControl {
    pub(super) fn encode(&self) -> Vec<u8> {
        encode(CONTROL_MAGIC, self)
    }
    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        let value: Self = decode(CONTROL_MAGIC, bytes)?;
        match &value {
            Self::Inherit {
                device,
                account_signature,
                ..
            } if account_signature.len() != 64
                || !device.valid_presence()
                || device.post_office
                || device.account_id().is_none() =>
            {
                None
            }
            Self::ReceiveKeys {
                sealed,
                authors,
                positions,
                ..
            } if sealed.is_empty()
                || sealed.len() > MAX_CONTROL / 2
                || authors.is_empty()
                || positions.len() != authors.len()
                || authors.len() > MAX_CHAINS
                || authors
                    .iter()
                    .map(PublicIdentity::user_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != authors.len() =>
            {
                None
            }
            _ => Some(value),
        }
    }
    pub(super) fn signed_inheritance(event: &Event) -> Option<(EventId, AccountGrantId, Announce)> {
        let Self::Inherit {
            authority,
            grant,
            device,
            account_signature,
        } = Self::decode(&event.ciphertext)?
        else {
            return None;
        };
        if event.kind != crate::eventlog::EventKind::ChannelDeviceInheritance
            || !event.is_canonical()
            || !event.verify_integrity()
            || !event.verify_signature()
            || event.author.ed25519_pub() != &device.ed25519_pub
            || device.account_cert.as_ref()?.account_ed25519_pub != grant.account_pub
        {
            return None;
        }
        let signature: [u8; 64] = account_signature.as_slice().try_into().ok()?;
        if !crate::identity::device::DeviceIdentity::verify(
            &grant.account_pub,
            &inheritance_account_message(event.conversation_id, authority, grant, &device.public()),
            &signature,
        ) {
            return None;
        }
        Some((authority, grant, device))
    }
}
impl AccountGroupRequest {
    pub(super) fn encode(&self) -> Vec<u8> {
        encode(REQUEST_MAGIC, self)
    }
    pub(in crate::node) fn decode(bytes: &[u8]) -> Option<Self> {
        let value = decode(REQUEST_MAGIC, bytes)?;
        if let Self::BindingTransitions {
            after: Some(after), ..
        } = &value
            && !valid_device_cursor(after)
        {
            return None;
        }
        Some(value)
    }
}
impl AccountGroupResponse {
    pub(super) fn encode(&self) -> Vec<u8> {
        encode(RESPONSE_MAGIC, self)
    }
    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        let value: Self = decode(RESPONSE_MAGIC, bytes)?;
        match &value {
            Self::BindingTransitions { patches, next }
                if patches.len() > MAX_BINDING_PATCHES
                    || patches
                        .iter()
                        .map(|patch| patch.transitions.len())
                        .sum::<usize>()
                        > MAX_BINDING_PROOFS
                    || patches.iter().any(|patch| !valid_binding_patch(patch))
                    || patches.windows(2).any(|pair| {
                        pair[0].announce.public().user_id() >= pair[1].announce.public().user_id()
                    })
                    || next.as_ref().is_some_and(|next| {
                        !valid_device_cursor(next)
                            || patches
                                .last()
                                .is_none_or(|patch| patch.announce.public().user_id() != *next)
                    }) =>
            {
                None
            }
            Self::Groups { channels, next }
                if channels.len() > MAX_GROUPS
                    || channels
                        .windows(2)
                        .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
                    || next.is_some_and(|next| channels.last() != Some(&next)) =>
            {
                None
            }
            _ => Some(value),
        }
    }
}
impl ReceiveDistribution {
    pub(super) fn encode_batch(values: &[Self]) -> Option<Vec<u8>> {
        if values.len() > MAX_CHAINS {
            return None;
        }
        let bytes = encode(b"MTGK1", &values);
        (bytes.len() <= MAX_CONTROL / 2).then_some(bytes)
    }
    pub(super) fn decode_batch(bytes: &[u8]) -> Option<Vec<Self>> {
        let values: Vec<Self> = decode(b"MTGK1", bytes)?;
        (values.len() <= MAX_CHAINS).then_some(values)
    }
}

struct ReceiveKeyAdmission {
    source: PublicIdentity,
    recipient: PublicIdentity,
    epoch: u64,
    sealed: Vec<u8>,
    authors: Vec<PublicIdentity>,
    positions: Vec<u32>,
}
fn receive_key_admission(
    state: &crate::channel::ChannelState,
    event: &Event,
    authority: &super::channel_account_authority::AccountAuthority,
    graph: &impl super::channel::graph::AuthorityGraph,
) -> Result<Option<ReceiveKeyAdmission>, LogError> {
    let Some(DeviceControl::ReceiveKeys {
        authority: reference,
        inheritance,
        grant,
        recipient,
        epoch,
        sealed,
        authors,
        positions,
    }) = DeviceControl::decode(&event.ciphertext)
    else {
        return Ok(None);
    };
    if event.kind != crate::eventlog::EventKind::ChannelDeviceInheritance
        || !event.is_canonical()
        || !event.verify_integrity()
        || !event.verify_signature()
        || state.epoch() != epoch
        || !authority.try_knows_authority(reference)?
        || !graph.descends(event.id, reference)?
        || !graph.descends(event.id, inheritance)?
    {
        return Ok(None);
    }
    let Some(inherited) = graph.event(inheritance)? else {
        return Ok(None);
    };
    let Some((_, inherited_grant, target_proof)) = DeviceControl::signed_inheritance(&inherited)
    else {
        return Ok(None);
    };
    if target_proof.public() != recipient
        || inherited_grant != grant
        || authority
            .grant_for_device(&recipient, &target_proof)
            .and_then(|grant| grant.id())
            != Some(grant)
    {
        return Ok(None);
    }
    let author = event.author.user_id();
    let account = crate::identity::account::AccountPublic {
        ed25519_pub: grant.account_pub,
    }
    .account_id();
    if state.member_account_id(&author) != Some(account.as_str())
        || !state.is_accepted_member(&author)
    {
        return Ok(None);
    }
    let Some(source) = state
        .members()
        .iter()
        .find(|member| member.ed25519_pub == *event.author.ed25519_pub())
    else {
        return Ok(None);
    };
    Ok(Some(ReceiveKeyAdmission {
        source: source.clone(),
        recipient,
        epoch,
        sealed,
        authors,
        positions,
    }))
}
pub(super) fn receive_keys_admitted(
    state: &crate::channel::ChannelState,
    event: &Event,
    authority: &super::channel_account_authority::AccountAuthority,
    graph: &impl super::channel::graph::AuthorityGraph,
) -> Result<bool, LogError> {
    Ok(receive_key_admission(state, event, authority, graph)?.is_some())
}

/// Secret replay follows exactly the same grant admission as transport projection.
pub(super) fn open_receive_distributions(
    me: &crate::identity::device::DeviceIdentity,
    state: &crate::channel::ChannelState,
    event: &Event,
    authority: &super::channel_account_authority::AccountAuthority,
    graph: &impl super::channel::graph::AuthorityGraph,
) -> Result<Vec<ReceiveDistribution>, LogError> {
    let Some(admission) = receive_key_admission(state, event, authority, graph)? else {
        return Ok(Vec::new());
    };
    if admission.recipient != me.public() {
        return Ok(Vec::new());
    }
    let epoch = admission.epoch;
    let Ok(bytes) = crate::dm::open(me, &admission.source.x25519_pub, &admission.sealed) else {
        return Ok(Vec::new());
    };
    let bytes = zeroize::Zeroizing::new(bytes);
    let Some(distributions) = ReceiveDistribution::decode_batch(&bytes) else {
        return Ok(Vec::new());
    };
    if distributions
        .iter()
        .map(|distribution| &distribution.author)
        .ne(admission.authors.iter())
        || distributions
            .iter()
            .map(|item| item.distribution.n)
            .ne(admission.positions.iter().copied())
    {
        return Ok(Vec::new());
    }
    if distributions.iter().any(|distribution| {
        distribution.epoch != epoch
            || !state.is_accepted_member(&distribution.author.user_id())
            || !state.members().contains(&distribution.author)
    }) {
        return Ok(Vec::new());
    }
    Ok(distributions)
}

#[cfg(test)]
pub(super) fn apply_receive_keys(
    me: &crate::identity::device::DeviceIdentity,
    state: &mut crate::channel::ChannelState,
    event: &Event,
    authority: &super::channel_account_authority::AccountAuthority,
    graph: &impl super::channel::graph::AuthorityGraph,
) -> Result<(), LogError> {
    for item in open_receive_distributions(me, state, event, authority, graph)? {
        state.refresh_sender_chain(item.author.user_id(), item.epoch, &item.distribution);
    }
    Ok(())
}

impl super::Node {
    pub(super) fn binding_transition_group_member(
        &self,
        channel: ConversationId,
        peer: &PublicIdentity,
        proof: &Announce,
    ) -> Result<bool, LogError> {
        if proof.public() != *peer || !proof.valid_presence() || proof.post_office {
            return Ok(false);
        }
        let projection = self.account_group_projection(channel)?;
        let Some(state) = projection.state else {
            return Ok(false);
        };
        let own = self.identity.public();
        let accepted = |public: &PublicIdentity| {
            state.is_accepted_member(&public.user_id()) && state.members().contains(public)
        };
        Ok(!state.is_dissolved()
            && accepted(peer)
            && accepted(&own)
            && (projection.authority.current_event().is_none()
                || (projection.authority.is_member(peer, proof)
                    && projection
                        .authority
                        .is_member(&own, &self.own_group_proof()))))
    }

    fn binding_transition_page(
        &self,
        channel: ConversationId,
        peer: &PublicIdentity,
        proof: &Announce,
        after: Option<&str>,
    ) -> Result<Option<AccountGroupResponse>, LogError> {
        if !self.binding_transition_group_member(channel, peer, proof)? {
            return Ok(None);
        }
        let projection = self.account_group_projection(channel)?;
        let Some(state) = projection.state else {
            return Ok(None);
        };
        let mut candidates = self.account_binding_patches();
        candidates.retain(|patch| {
            let public = patch.announce.public();
            after.is_none_or(|after| public.user_id().as_str() > after)
                && state.is_accepted_member(&public.user_id())
                && state.members().contains(&public)
                && valid_binding_patch(patch)
        });
        candidates.sort_unstable_by_key(|patch| patch.announce.public().user_id());
        candidates.dedup_by_key(|patch| patch.announce.public().user_id());
        let mut patches = Vec::new();
        let mut proofs = 0;
        let mut more = false;
        for patch in candidates {
            if patch.transitions.len() > MAX_BINDING_PROOFS {
                continue;
            }
            if patches.len() >= MAX_BINDING_PATCHES
                || proofs + patch.transitions.len() > MAX_BINDING_PROOFS
            {
                more = true;
                break;
            }
            proofs += patch.transitions.len();
            patches.push(patch);
        }
        let next = more
            .then(|| {
                patches
                    .last()
                    .map(|patch| patch.announce.public().user_id())
            })
            .flatten();
        let response = AccountGroupResponse::BindingTransitions { patches, next };
        Ok((response.encode().len() <= MAX_CONTROL).then_some(response))
    }
    pub(super) fn account_group_projection(
        &self,
        channel: ConversationId,
    ) -> Result<super::channel::ChannelMembershipProjection, LogError> {
        let log = self.log.lock().expect("log lock not poisoned");
        self.account_group_projection_with_log(&log, channel)
    }

    pub(super) fn account_group_projection_with_log(
        &self,
        log: &crate::eventlog::persist::PersistentEventLog,
        channel: ConversationId,
    ) -> Result<super::channel::ChannelMembershipProjection, LogError> {
        let repository = log.repository()?;
        let projection = self.channel_checkpoints.published_projection(channel);
        let projection = match projection {
            Ok(Some(projection)) => return Ok(projection),
            Ok(None)
                if !repository
                    .has_kind(&channel, crate::eventlog::EventKind::MembershipChange)? =>
            {
                return Ok(super::channel::ChannelMembershipProjection::empty(channel));
            }
            Ok(None) | Err(LogError::StaleCursor) => LogError::StaleCursor,
            Err(error) => return Err(error),
        };
        self.channel_checkpoints.queue_runtime(channel)?;
        self.channel_recovery_notify.notify_one();
        Err(projection)
    }

    pub(super) fn own_group_proof(&self) -> Announce {
        Announce::new_with_account(&self.identity, &self.account, "", 1)
    }
    pub(super) async fn query_account_groups(
        &self,
        peer: &crate::discovery::PeerRecord,
        request: AccountGroupRequest,
    ) -> Option<AccountGroupResponse> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut channel = self.privacy_dial(peer.addr, &peer.public).await.ok()?;
            channel.send(&request.encode()).await.ok()?;
            AccountGroupResponse::decode(&channel.recv().await.ok()?)
        })
        .await
        .ok()
        .flatten()
    }
    pub(super) async fn query_account_groups_verified(
        &self,
        peer: &crate::discovery::PeerRecord,
        request: AccountGroupRequest,
    ) -> Option<(AccountGroupResponse, Announce)> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut channel = self.privacy_dial(peer.addr, &peer.public).await.ok()?;
            channel.send(&request.encode()).await.ok()?;
            let response = AccountGroupResponse::decode(&channel.recv().await.ok()?)?;
            let proof = channel
                .peer_announcement()
                .cloned()
                .or_else(|| match &response {
                    AccountGroupResponse::Capability { proof, .. } => Some(proof.as_ref().clone()),
                    _ => None,
                })?;
            (proof.public() == peer.public
                && proof.public() == *channel.peer_identity()
                && proof.valid_presence())
            .then_some((response, proof))
        })
        .await
        .ok()
        .flatten()
    }
    pub(in crate::node) async fn serve_account_group_request(
        self: &std::sync::Arc<Self>,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        request: AccountGroupRequest,
    ) {
        let result: Result<(), NodeError> = async {
            let _privacy = self.privacy.gate.read().await;
            if !channel.io_admitted() {
                return Ok(());
            }
            let response = match request {
                AccountGroupRequest::Capability => AccountGroupResponse::Capability {
                    version: 1,
                    proof: Box::new(self.own_group_proof()),
                },
                AccountGroupRequest::Groups { account_id, after } => {
                    if account_id != self.account_id()
                        || !self.certified_sibling(channel.peer_identity())
                        || !self
                            .account_allowed(channel.peer_identity(), channel.peer_announcement())
                    {
                        return Ok(());
                    }
                    let own_account = self.account.public().ed25519_pub;
                    let mut channels = Vec::new();
                    for row in self.list_channels()? {
                        if !after.is_none_or(|after| row.id.as_bytes() > after.as_bytes()) {
                            continue;
                        }
                        let projection = self.account_group_projection(row.id)?;
                        if projection.authority.current_event().is_some()
                            && (projection.authority.account_grant(&own_account).is_some()
                                || projection.authority.has_revocation(&own_account)?)
                        {
                            channels.push(row.id);
                        }
                    }
                    channels.sort_unstable_by_key(|channel| *channel.as_bytes());
                    channels.truncate(MAX_GROUPS + 1);
                    let next = (channels.len() > MAX_GROUPS).then(|| channels[MAX_GROUPS - 1]);
                    channels.truncate(MAX_GROUPS);
                    AccountGroupResponse::Groups { channels, next }
                }
                AccountGroupRequest::BindingTransitions {
                    channel: group,
                    after,
                } => {
                    let Some(proof) = channel.peer_announcement() else {
                        return Ok(());
                    };
                    let Some(response) = self.binding_transition_page(
                        group,
                        channel.peer_identity(),
                        proof,
                        after.as_deref(),
                    )?
                    else {
                        return Ok(());
                    };
                    response
                }
            };
            drop(_privacy);
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                channel.send(&response.encode()),
            )
            .await;

            Ok(())
        }
        .await;
        if let Err(error) = result {
            log::warn!("serve_account_group_request deferred after storage failure: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::EventKind,
        identity::{account::Account, device::DeviceIdentity},
    };

    #[test]
    fn binding_transition_pages_reject_duplicate_unsorted_oversized_or_invalid_proofs() {
        use super::super::peer_directory::AccountBindingPatch;
        use crate::identity::account_transition::AccountBindingTransition;
        let device = DeviceIdentity::generate();
        let old = Account::generate();
        let current = Account::generate();
        let patch = AccountBindingPatch {
            announce: Announce::new_with_account(&device, &current, "Member", 1234),
            transitions: vec![AccountBindingTransition::create(
                &old, &current, &device, None,
            )],
        };
        let response = |patches, next| AccountGroupResponse::BindingTransitions { patches, next };
        assert!(
            AccountGroupResponse::decode(&response(vec![patch.clone()], None).encode()).is_some()
        );
        for invalid in [
            response(vec![patch.clone(), patch.clone()], None),
            response(vec![patch.clone(); 5], None),
            response(vec![patch.clone()], Some("wrong-cursor".into())),
            response(vec![], Some(device.user_id())),
        ] {
            assert!(
                AccountGroupResponse::decode(&invalid.encode()).is_none(),
                "malformed transition pages cannot enter maintenance"
            );
        }
        let mut bad = patch.clone();
        bad.transitions[0].old_signature[0] ^= 1;
        assert!(AccountGroupResponse::decode(&response(vec![bad], None).encode()).is_none());
        let mut wrong_device = patch.clone();
        wrong_device.announce =
            Announce::new_with_account(&DeviceIdentity::generate(), &current, "other", 1234);
        assert!(
            AccountGroupResponse::decode(&response(vec![wrong_device], None).encode()).is_none()
        );
        let mut many = Vec::new();
        for _ in 0..3 {
            let device = DeviceIdentity::generate();
            let mut old = Account::generate();
            let mut transitions = Vec::new();
            for _ in 0..3 {
                let new = Account::generate();
                transitions.push(AccountBindingTransition::create(
                    &old,
                    &new,
                    &device,
                    transitions.last().map(AccountBindingTransition::id),
                ));
                old = new;
            }
            many.push(AccountBindingPatch {
                announce: Announce::new_with_account(&device, &old, "member", 1234),
                transitions,
            });
        }
        many.sort_unstable_by_key(|patch| patch.announce.public().user_id());
        let mut unsorted = many[..2].to_vec();
        unsorted.reverse();
        assert!(AccountGroupResponse::decode(&response(unsorted, None).encode()).is_none());
        assert!(
            AccountGroupResponse::decode(&response(many, None).encode()).is_none(),
            "even otherwise valid pages have an eight-proof total budget"
        );
        let bad_cursor = AccountGroupRequest::BindingTransitions {
            channel: ConversationId::new([1; 32]),
            after: Some("not-a-device-id".into()),
        };
        assert!(AccountGroupRequest::decode(&bad_cursor.encode()).is_none());
    }

    #[test]
    fn inheritance_requires_its_own_device_signature_and_account_commitment() {
        let account = Account::generate();
        let target = DeviceIdentity::generate();
        let outsider = DeviceIdentity::generate();
        let grant = AccountGrantId {
            account_pub: account.public().ed25519_pub,
            incarnation: [3; 16],
        };
        let proof = Announce::new_with_account(&target, &account, "target", 1234);
        let control = DeviceControl::Inherit {
            authority: EventId::new([1; 32]),
            grant,
            account_signature: account
                .sign(&inheritance_account_message(
                    ConversationId::new([2; 32]),
                    EventId::new([1; 32]),
                    grant,
                    &proof.public(),
                ))
                .to_vec(),
            device: proof,
        };
        let event = |author: &DeviceIdentity, kind| {
            Event::new(
                author,
                ConversationId::new([2; 32]),
                1,
                vec![],
                1,
                1,
                kind,
                control.encode(),
            )
        };
        assert!(
            DeviceControl::signed_inheritance(&event(&target, EventKind::ChannelDeviceInheritance))
                .is_some()
        );
        assert!(
            DeviceControl::signed_inheritance(&event(
                &outsider,
                EventKind::ChannelDeviceInheritance
            ))
            .is_none()
        );
        assert!(DeviceControl::signed_inheritance(&event(&target, EventKind::Message)).is_none());
        // A stolen device signing key and its public certificate cannot authorize
        // a replacement X25519 key without the account-root commitment.
        let stolen =
            DeviceIdentity::from_secret_bytes(target.secret_bytes().0, outsider.secret_bytes().1);
        let mut substituted = control.clone();
        if let DeviceControl::Inherit { device, .. } = &mut substituted {
            *device = Announce::new_with_account(&stolen, &account, "target", 1234);
        }
        let forged = Event::new(
            &stolen,
            ConversationId::new([2; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::ChannelDeviceInheritance,
            substituted.encode(),
        );
        assert!(DeviceControl::signed_inheritance(&forged).is_none());
        let mut junk = control.encode();
        junk.push(0);
        assert!(DeviceControl::decode(&junk).is_none());
    }

    #[test]
    fn inherited_device_sends_with_its_own_chain_and_account_removal_revokes_every_sibling() {
        use super::super::{
            channel::{ChannelBook, seal_keys_for},
            channel_account_authority::{AccountAuthorityControl, AccountGrant},
        };
        use crate::{
            channel::ChannelMeta,
            eventlog::{event::Author, store::EventLog},
        };
        fn append(
            log: &mut EventLog,
            author: &DeviceIdentity,
            channel: ConversationId,
            kind: EventKind,
            bytes: Vec<u8>,
        ) -> EventId {
            let (parents, lamport) = log.prepare(&channel).unwrap();
            let seq = log
                .next_sequence(&channel, &Author::from_ed25519(author.public().ed25519_pub))
                .unwrap();
            let event = Event::new(author, channel, seq, parents, lamport, 1, kind, bytes);
            let id = event.id;
            log.append(event).unwrap();
            id
        }
        let owner_account = Account::generate();
        let member_account = Account::generate();
        let owner = DeviceIdentity::generate();
        let source = DeviceIdentity::generate();
        let target = DeviceIdentity::generate();
        let owner_proof = Announce::new_with_account(&owner, &owner_account, "owner", 1001);
        let source_proof = Announce::new_with_account(&source, &member_account, "member", 1002);
        let target_proof =
            Announce::new_with_account(&target, &member_account, "linked device", 1003);
        let channel = ConversationId::new([37; 32]);
        let mut log = EventLog::default();
        let root = append(
            &mut log,
            &owner,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "account group".into(),
                members: vec![owner.public(), source.public()],
                epoch: 0,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        let owner_grant = AccountGrant {
            pending: false,
            incarnation: [1; 16],
            original: owner_proof.clone(),
            membership_event: root,
            acceptance_event: None,
        };
        let member_grant = AccountGrant {
            pending: false,
            incarnation: [2; 16],
            original: source_proof.clone(),
            membership_event: root,
            acceptance_event: None,
        };
        let authority = append(
            &mut log,
            &owner,
            channel,
            EventKind::ChannelAccountAuthority,
            AccountAuthorityControl {
                version: 1,
                previous: None,
                issuer: owner_proof.clone(),
                owner_account: owner_account.public().ed25519_pub,
                grants: vec![owner_grant.clone(), member_grant.clone()],
                revoked: vec![],
                legacy_bindings: vec![owner_proof.clone(), source_proof],
                account_signature: vec![],
            }
            .signed_encode(channel, &owner_account),
        );
        append(
            &mut log,
            &target,
            channel,
            EventKind::ChannelDeviceInheritance,
            DeviceControl::Inherit {
                authority,
                grant: member_grant.id().unwrap(),
                account_signature: member_account
                    .sign(&inheritance_account_message(
                        channel,
                        authority,
                        member_grant.id().unwrap(),
                        &target_proof.public(),
                    ))
                    .to_vec(),
                device: target_proof,
            }
            .encode(),
        );
        // No subsequent owner signature or key is involved in target participation.
        let mut target_book = ChannelBook::new();
        target_book.process(&target, channel, &log.events(&channel));
        let target_state = target_book
            .state_mut(&channel)
            .expect("active inherited channel");
        assert!(target_state.is_accepted_member(&target.public().user_id()));
        assert!(!target_state.can_admin(&target.public().user_id()));
        let distribution = target_state.my_sender_distribution();
        let sealed = seal_keys_for(&target, target_state.members(), &distribution, 0).unwrap();
        append(
            &mut log,
            &target,
            channel,
            EventKind::KeyRotation,
            sealed.encode(),
        );
        let wire = target_state
            .seal_sender_message(b"inherited member can send")
            .unwrap();
        append(&mut log, &target, channel, EventKind::Message, wire);
        let mut source_book = ChannelBook::new();
        let received = source_book.process(&source, channel, &log.events(&channel));
        assert!(
            received
                .iter()
                .any(|message| message.text == b"inherited member can send")
        );
        append(
            &mut log,
            &owner,
            channel,
            EventKind::ChannelAccountAuthority,
            AccountAuthorityControl {
                version: 1,
                previous: Some(authority),
                issuer: owner_proof,
                owner_account: owner_account.public().ed25519_pub,
                grants: vec![owner_grant],
                revoked: vec![member_grant.id().unwrap()],
                legacy_bindings: vec![],
                account_signature: vec![],
            }
            .signed_encode(channel, &owner_account),
        );
        source_book.process(&source, channel, &log.events(&channel));
        let state = source_book.state(&channel).unwrap();
        assert!(!state.is_member(&source.public().user_id()));
        assert!(!state.is_member(&target.public().user_id()));
        assert_eq!(state.epoch(), 1);
        target_book.process(&target, channel, &log.events(&channel));
        assert!(
            target_book
                .state_mut(&channel)
                .unwrap()
                .seal_sender_message(b"removed account")
                .is_err()
        );
    }

    #[test]
    fn group_route_pages_are_bounded_ordered_and_confer_no_membership() {
        let response = AccountGroupResponse::Groups {
            channels: vec![ConversationId::new([1; 32]), ConversationId::new([2; 32])],
            next: Some(ConversationId::new([2; 32])),
        };
        assert!(AccountGroupResponse::decode(&response.encode()).is_some());
        let duplicate = AccountGroupResponse::Groups {
            channels: vec![ConversationId::new([1; 32]); 2],
            next: None,
        };
        assert!(AccountGroupResponse::decode(&duplicate.encode()).is_none());
        let oversized = AccountGroupResponse::Groups {
            channels: (0..=MAX_GROUPS)
                .map(|i| {
                    let mut id = [0; 32];
                    id[0] = i as u8;
                    ConversationId::new(id)
                })
                .collect(),
            next: None,
        };
        assert!(AccountGroupResponse::decode(&oversized.encode()).is_none());
        let bad_cursor = AccountGroupResponse::Groups {
            channels: vec![ConversationId::new([1; 32])],
            next: Some(ConversationId::new([2; 32])),
        };
        assert!(AccountGroupResponse::decode(&bad_cursor.encode()).is_none());
    }
}
