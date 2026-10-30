//! Account principals for upgraded channels. Legacy membership is evidence for
//! the one-time owner-authorized commitment, never an implicit account mapping.
use super::channel::graph::AuthorityGraph;
use super::wire::{frame, unframe};
use crate::{
    discovery::Announce,
    eventlog::{
        Event, EventKind,
        event::{ConversationId, EventId},
    },
    identity::{account::AccountPublic, device::PublicIdentity},
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

mod facts;
pub(in crate::node) use facts::AuthorityHistory;
pub(crate) const MAX_AUTHORITY_BYTES: usize = 48 * 1024;
const MAX_GRANTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct AccountGrantId {
    pub account_pub: [u8; 32],
    pub incarnation: [u8; 16],
}
/// Explicit self-decline of one invitation incarnation. Older empty-only
/// ChannelLeave reducers reject this payload rather than treating it as leave.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountDeclinePayload {
    pub version: u8,
    pub grant: AccountGrantId,
}
impl AccountDeclinePayload {
    pub(crate) fn encode(&self) -> Vec<u8> {
        frame(b"MTCD1", self)
    }
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let payload: Self = unframe(b"MTCD1", bytes)?;
        (payload.version == 1).then_some(payload)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountGrant {
    pub incarnation: [u8; 16],
    pub pending: bool,
    pub original: Announce,
    pub membership_event: EventId,
    pub acceptance_event: Option<EventId>,
}
impl AccountGrant {
    pub(crate) fn id(&self) -> Option<AccountGrantId> {
        Some(AccountGrantId {
            account_pub: self.original.account_cert.as_ref()?.account_ed25519_pub,
            incarnation: self.incarnation,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountAuthorityControl {
    pub version: u8,
    pub previous: Option<EventId>,
    pub issuer: Announce,
    pub owner_account: [u8; 32],
    pub grants: Vec<AccountGrant>,
    pub revoked: Vec<AccountGrantId>,
    /// Initial owner-authorized account cutover bindings for every accepted legacy device.
    pub legacy_bindings: Vec<Announce>,
    pub account_signature: Vec<u8>,
}
impl AccountAuthorityControl {
    fn account_message(&self, channel: ConversationId) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.account_signature.clear();
        let mut message = b"mesh-talk-channel-authority-account-v1".to_vec();
        message.extend_from_slice(channel.as_bytes());
        message.extend(unsigned.encode());
        message
    }
    pub(super) fn verify_account_signature(
        &self,
        channel: ConversationId,
        account_pub: &[u8; 32],
    ) -> bool {
        let Ok(signature) = <[u8; 64]>::try_from(self.account_signature.as_slice()) else {
            return false;
        };
        crate::identity::device::DeviceIdentity::verify(
            account_pub,
            &self.account_message(channel),
            &signature,
        )
    }
    pub(crate) fn signed_encode(
        &self,
        channel: ConversationId,
        account: &crate::identity::account::Account,
    ) -> Vec<u8> {
        let mut signed = self.clone();
        signed.account_signature = account.sign(&self.account_message(channel)).to_vec();
        signed.encode()
    }
    pub(crate) fn encode(&self) -> Vec<u8> {
        frame(b"MTCA1", self)
    }
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_AUTHORITY_BYTES {
            return None;
        }
        let value: Self = unframe(b"MTCA1", bytes)?;
        (value.version == 1
            && value.grants.len() <= MAX_GRANTS
            && value.legacy_bindings.len() <= MAX_GRANTS
            && value.revoked.len() <= MAX_GRANTS)
            .then_some(value)
    }
}
/// Produced from the canonical legacy projection at this control's ancestry.
/// These are accepted grant incarnations, not arbitrary current roster members.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LegacyAccountMember {
    pub public: PublicIdentity,
    pub membership_event: EventId,
    pub acceptance_event: Option<EventId>,
}
pub(crate) struct LegacyAuthorityContext {
    pub channel: ConversationId,
    pub owner: PublicIdentity,
    pub epoch: u64,
    pub accepted: Vec<LegacyAccountMember>,
    pub invited: Vec<LegacyAccountMember>,
    pub live: Vec<PublicIdentity>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LegacyDeparture {
    pub member: LegacyAccountMember,
    pub event: EventId,
}
type LegacyLifetimeKey = (EventId, Option<EventId>, [u8; 32], [u8; 32]);
fn lifetime_key(member: &LegacyAccountMember) -> LegacyLifetimeKey {
    (
        member.membership_event,
        member.acceptance_event,
        member.public.ed25519_pub,
        member.public.x25519_pub,
    )
}
#[derive(Clone, Serialize, Deserialize)]
struct InitialLegacyCohort {
    grant: AccountGrant,
    originals: Vec<LegacyAccountMember>,
}

#[derive(Debug)]
pub(crate) enum AuthorityError {
    Invalid,
    Unsupported,
    MissingAncestry,
    Stale,
    Storage(crate::eventlog::LogError),
}
impl From<crate::eventlog::LogError> for AuthorityError {
    fn from(error: crate::eventlog::LogError) -> Self {
        Self::Storage(error)
    }
}
pub(in crate::node) fn admitted(
    result: Result<bool, AuthorityError>,
) -> Result<bool, crate::eventlog::LogError> {
    match result {
        Ok(_) => Ok(true),
        Err(AuthorityError::Storage(error)) => Err(error),
        Err(_) => Ok(false),
    }
}
#[cfg(test)]
impl PartialEq for AuthorityError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Invalid, Self::Invalid)
            | (Self::Unsupported, Self::Unsupported)
            | (Self::MissingAncestry, Self::MissingAncestry)
            | (Self::Stale, Self::Stale) => true,
            (Self::Storage(left), Self::Storage(right)) => std::ptr::eq(left, right),
            _ => false,
        }
    }
}
/// The single role decision consumed by replay, admission and channel controls.
#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct AccountAuthority {
    channel: Option<ConversationId>,
    current: Option<EventId>,
    owner: Option<[u8; 32]>,
    grants: HashMap<[u8; 32], AccountGrant>,
    #[serde(skip)]
    valid_controls: HashSet<EventId>,
    #[serde(skip)]
    revoked: HashMap<AccountGrantId, EventId>,
    genesis: Option<EventId>,
    base_epoch: Option<u64>,
    #[serde(skip)]
    added_grants: HashSet<AccountGrantId>,
    #[serde(skip)]
    pinned: HashMap<LegacyBindingKey, [u8; 32]>,
    originals: Vec<Announce>,
    device_grants: HashMap<([u8; 32], [u8; 32]), AccountGrantId>,
    activated: HashSet<AccountGrantId>,
    // Every authenticated acceptance matters to a later decline's causal view,
    // including a concurrent acceptance that was already idempotent on replay.
    #[serde(skip)]
    acceptance_events: HashMap<AccountGrantId, HashSet<EventId>>,
    // Reserve the acceptance epoch for a declined incarnation without activating
    // it. A later full view must not reuse a partial Accept view's content epoch.
    #[serde(skip)]
    declined_epoch_reservations: HashSet<AccountGrantId>,
    origins: HashMap<AccountGrantId, EventId>,
    inherited: HashMap<([u8; 32], [u8; 32]), (AccountGrantId, EventId)>,
    initial_legacy: HashMap<AccountGrantId, InitialLegacyCohort>,
    #[serde(skip)]
    legacy_candidates: Vec<LegacyDeparture>,
    #[serde(skip)]
    legacy_candidate_lifetimes: HashSet<LegacyLifetimeKey>,
    #[serde(skip)]
    legacy_seen: HashMap<LegacyLifetimeKey, EventId>,
    legacy_base: HashSet<LegacyLifetimeKey>,
    legacy_omitted: Vec<LegacyAccountMember>,
    historical_added: u64,
    historical_revoked: u64,
    #[serde(skip)]
    legacy_revoked: HashSet<AccountGrantId>,
    #[serde(skip)]
    history: Option<std::sync::Arc<AuthorityHistory>>,
}
impl AccountAuthority {
    fn cohort_departed(&self, id: AccountGrantId, all: bool) -> bool {
        self.initial_legacy.get(&id).is_some_and(|cohort| {
            !cohort.originals.is_empty()
                && cohort.originals.iter().all(|original| {
                    if all {
                        self.legacy_candidate_lifetimes
                            .contains(&lifetime_key(original))
                            || self.legacy_candidates.iter().any(|departure| {
                                lifetime_key(&departure.member) == lifetime_key(original)
                            })
                    } else {
                        self.legacy_seen.contains_key(&lifetime_key(original))
                    }
                })
        })
    }
    fn cohort_closed(&self, id: AccountGrantId, all: bool) -> bool {
        let protected_owner = Some(id.account_pub) == self.owner
            && self.grants.get(&id.account_pub).and_then(AccountGrant::id) == Some(id);
        !protected_owner && self.cohort_departed(id, all)
    }
    pub(crate) fn set_legacy_candidates(&mut self, candidates: Vec<LegacyDeparture>) {
        self.legacy_candidates = candidates;
    }
    pub(crate) fn note_legacy_departure(&mut self, departure: &LegacyDeparture) {
        self.legacy_seen
            .entry(lifetime_key(&departure.member))
            .and_modify(|event| *event = (*event).min(departure.event))
            .or_insert(departure.event);
    }
    pub(crate) fn legacy_retired_source(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Option<(AccountGrant, LegacyAccountMember, EventId)> {
        if proof.public() != *public || !proof.valid_presence() {
            return None;
        }
        let account = proof.account_cert.as_ref()?.account_ed25519_pub;
        let (id, cohort) = self
            .initial_legacy
            .iter()
            .find(|(id, _)| id.account_pub == account)?;
        if !self.cohort_closed(*id, false) {
            return None;
        }
        if self
            .origins
            .keys()
            .any(|other| other.account_pub == account && other != id)
        {
            return None;
        }
        if self
            .grants
            .get(&account)
            .is_some_and(|grant| grant.id() != Some(*id))
        {
            return None;
        }
        let original = cohort
            .originals
            .iter()
            .find(|member| member.public == *public)?;
        let event = *self.legacy_seen.get(&lifetime_key(original))?;
        Some((cohort.grant.clone(), original.clone(), event))
    }
    pub(crate) fn omitted_legacy_departure(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Option<EventId> {
        if proof.public() != *public || !proof.verify() {
            return None;
        }
        let account = proof.account_cert.as_ref()?.account_ed25519_pub;
        // A later account incarnation owns its own terminal/acceptance rules.
        if self
            .origins
            .iter()
            .any(|(id, origin)| id.account_pub == account && Some(*origin) != self.genesis)
        {
            return None;
        }
        let original = self
            .legacy_omitted
            .iter()
            .find(|member| member.public == *public)?;
        self.legacy_seen.get(&lifetime_key(original)).copied()
    }
    pub(crate) fn legacy_closing_events(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Vec<EventId> {
        let retired = self
            .legacy_retired_source(public, proof)
            .and_then(|(grant, _, _)| grant.id());
        let omitted = self.omitted_legacy_departure(public, proof).is_some();
        let account = proof
            .account_cert
            .as_ref()
            .map(|cert| cert.account_ed25519_pub);
        let mut events: Vec<_> = self
            .initial_legacy
            .iter()
            .filter(|(id, _)| {
                retired == Some(**id)
                    || omitted && Some(id.account_pub) == account && self.cohort_closed(**id, false)
            })
            .flat_map(|(_, cohort)| {
                cohort
                    .originals
                    .iter()
                    .filter_map(|member| self.legacy_seen.get(&lifetime_key(member)).copied())
            })
            .collect();
        events.sort_by_key(|event| *event.as_bytes());
        events.dedup();
        events
    }
    pub(crate) fn legacy_closing_basis(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Vec<EventId> {
        let mut events = self.legacy_closing_events(public, proof);
        // An individually retired omitted device may still observe a live
        // initial sibling; freeze a negative account cut only once that cohort
        // is wholly closed (or the initial cut omitted the account entirely).
        let account_has_initial_grant = proof.account_cert.as_ref().is_some_and(|cert| {
            self.origins
                .keys()
                .any(|id| id.account_pub == cert.account_ed25519_pub)
        });
        if !events.is_empty() || !account_has_initial_grant {
            events.extend(self.omitted_legacy_departure(public, proof));
        }
        if !events.is_empty() {
            events.extend(self.genesis);
        }
        events.sort_by_key(|event| *event.as_bytes());
        events.dedup();
        events
    }
    pub(crate) fn legacy_closed_device_proofs(&self) -> Vec<Announce> {
        self.originals
            .iter()
            .filter(|proof| {
                let Some(id) = self
                    .device_grants
                    .get(&(proof.ed25519_pub, proof.x25519_pub))
                else {
                    return false;
                };
                self.cohort_closed(*id, false)
                    && self.grants.get(&id.account_pub).and_then(AccountGrant::id) == Some(*id)
            })
            .cloned()
            .collect()
    }
    pub(crate) fn legacy_departure_account(
        &self,
        event: EventId,
    ) -> Result<Option<[u8; 32]>, crate::eventlog::LogError> {
        let Some(departure) = self.legacy_candidate(event)? else {
            return Ok(None);
        };
        Ok(self
            .initial_legacy
            .iter()
            .find(|(_, cohort)| {
                cohort
                    .originals
                    .iter()
                    .any(|member| lifetime_key(member) == lifetime_key(&departure.member))
            })
            .map(|(id, _)| id.account_pub))
    }
    pub(crate) fn legacy_departure_current(
        &self,
        event: EventId,
    ) -> Result<bool, crate::eventlog::LogError> {
        Ok(self
            .legacy_departure_account(event)?
            .is_some_and(|account| {
                self.grants
                    .get(&account)
                    .and_then(AccountGrant::id)
                    .is_some_and(|id| self.initial_legacy.contains_key(&id))
            }))
    }
    pub(crate) fn key_epoch(&self) -> Option<u64> {
        let original_keys: HashSet<_> = self
            .initial_legacy
            .values()
            .flat_map(|cohort| cohort.originals.iter().map(lifetime_key))
            .collect();
        let legacy = self
            .legacy_seen
            .keys()
            .filter(|key| original_keys.contains(*key) && !self.legacy_base.contains(*key))
            .count();
        let ordinary = self
            .revoked
            .keys()
            .filter(|id| !self.cohort_departed(**id, false))
            .count();
        self.base_epoch?
            .checked_add(self.historical_added)?
            .checked_add(
                self.historical_revoked.checked_sub(
                    self.legacy_revoked
                        .iter()
                        .filter(|id| self.cohort_departed(**id, false))
                        .count() as u64,
                )?,
            )?
            .checked_add(
                self.added_grants
                    .union(&self.declined_epoch_reservations)
                    .count() as u64,
            )?
            .checked_add(legacy as u64)?
            .checked_add(ordinary as u64)
    }
    pub(crate) fn control_grants(&self) -> Vec<AccountGrant> {
        let mut grants: Vec<_> = self.grants.values().cloned().collect();
        grants.sort_by_key(|grant| grant.id().map(|id| id.account_pub));
        grants
    }
    pub(crate) fn original_device_proofs(&self) -> Vec<Announce> {
        self.originals
            .iter()
            .filter(|proof| self.is_member(&proof.public(), proof))
            .cloned()
            .collect()
    }
    /// Current admitted sibling bindings, with the exact signed authorization.
    /// Retired grant incarnations are pruned by the current authority owner.
    pub(in crate::node) fn inherited_device_proofs(
        &self,
    ) -> Vec<(AccountGrantId, EventId, Announce)> {
        let mut bindings: Vec<_> = self
            .original_device_proofs()
            .into_iter()
            .filter_map(|proof| {
                let (grant, event) = self.inherited.get(&(proof.ed25519_pub, proof.x25519_pub))?;
                (self
                    .grant_for_device(&proof.public(), &proof)
                    .and_then(AccountGrant::id)
                    == Some(*grant))
                .then_some((*grant, *event, proof))
            })
            .collect();
        bindings.sort_by_key(|(_, event, proof)| (*event, proof.ed25519_pub));
        bindings
    }
    pub(crate) fn current_event(&self) -> Option<EventId> {
        self.current
    }
    pub(crate) fn owner_account_id(&self) -> Option<String> {
        self.owner
            .map(|ed25519_pub| AccountPublic { ed25519_pub }.account_id())
    }
    pub(crate) fn grants(&self) -> Vec<AccountGrant> {
        self.control_grants()
            .into_iter()
            .filter(|grant| grant.id().is_some_and(|id| !self.cohort_closed(id, false)))
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn revoked(&self) -> Vec<AccountGrantId> {
        let mut ids: Vec<_> = self
            .revoked
            .keys()
            .copied()
            .chain(
                self.initial_legacy
                    .keys()
                    .filter(|id| self.cohort_closed(**id, false))
                    .copied(),
            )
            .collect();
        ids.sort_by_key(|id| (id.account_pub, id.incarnation));
        ids.dedup();
        ids
    }
    pub(crate) fn explicit_revocation_events(&self, account_pub: &[u8; 32]) -> Vec<EventId> {
        self.revoked
            .iter()
            .filter(|(id, _)| &id.account_pub == account_pub)
            .map(|(_, event)| *event)
            .collect()
    }
    pub(crate) fn revocation_events(&self, account_pub: &[u8; 32]) -> Vec<EventId> {
        let mut events = self.explicit_revocation_events(account_pub);
        for (id, cohort) in &self.initial_legacy {
            if &id.account_pub == account_pub && self.cohort_closed(*id, false) {
                events.extend(
                    cohort
                        .originals
                        .iter()
                        .filter_map(|member| self.legacy_seen.get(&lifetime_key(member)).copied()),
                );
            }
        }
        events.sort_by_key(|id| *id.as_bytes());
        events.dedup();
        events
    }
    pub(crate) fn account_grant(&self, account_pub: &[u8; 32]) -> Option<&AccountGrant> {
        let grant = self.grants.get(account_pub)?;
        (!self.revoked.contains_key(&grant.id()?) && !self.cohort_closed(grant.id()?, false))
            .then_some(grant)
    }
    pub(crate) fn inherit_device(
        &mut self,
        event: &Event,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        let (authority, id, proof) =
            super::channel_account_bootstrap::DeviceControl::signed_inheritance(event)
                .ok_or(AuthorityError::Invalid)?;
        if self.channel != Some(event.conversation_id)
            || !self.known_control(authority)?
            || !descends(event.id, authority, graph)?
            || self
                .account_grant(&id.account_pub)
                .and_then(AccountGrant::id)
                != Some(id)
            || !descends(
                event.id,
                self.grant_origin(id)
                    .ok_or(AuthorityError::MissingAncestry)?,
                graph,
            )?
        {
            return Err(AuthorityError::Stale);
        }
        if self
            .originals
            .iter()
            .any(|old| old.ed25519_pub == proof.ed25519_pub && old.public() != proof.public())
            || self.conflicting_original(&proof.public())?
        {
            return Err(AuthorityError::Invalid);
        }
        if self.proof_grant(&proof.public(), &proof).is_some() {
            return Ok(false);
        }
        self.device_grants
            .insert((proof.ed25519_pub, proof.x25519_pub), id);
        self.inherited
            .insert((proof.ed25519_pub, proof.x25519_pub), (id, event.id));
        self.originals.push(proof);
        Ok(true)
    }
    pub(crate) fn registered_account(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Option<[u8; 32]> {
        if proof.public() != *public || !proof.valid_presence() {
            return None;
        }
        let account = proof.account_cert.as_ref()?.account_ed25519_pub;
        self.originals
            .iter()
            .any(|original| {
                original.public() == *public
                    && original
                        .account_cert
                        .as_ref()
                        .is_some_and(|certificate| certificate.account_ed25519_pub == account)
            })
            .then_some(account)
    }
    fn proof_grant(&self, public: &PublicIdentity, proof: &Announce) -> Option<&AccountGrant> {
        if proof.public() != *public
            || !proof.valid_presence()
            || !self.originals.iter().any(|original| {
                original.public() == *public && original.account_id() == proof.account_id()
            })
        {
            return None;
        }
        let account = proof.account_cert.as_ref()?.account_ed25519_pub;
        let grant = self.grants.get(&account)?;
        if self
            .device_grants
            .get(&(public.ed25519_pub, public.x25519_pub))
            .copied()
            != grant.id()
        {
            return None;
        }
        (!self.revoked.contains_key(&grant.id()?) && !self.cohort_closed(grant.id()?, false))
            .then_some(grant)
    }
    pub(crate) fn grant_for_device(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Option<&AccountGrant> {
        self.proof_grant(public, proof).filter(|grant| {
            !grant.pending || grant.id().is_some_and(|id| self.activated.contains(&id))
        })
    }
    pub(crate) fn invitation_for_device(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Option<&AccountGrant> {
        self.proof_grant(public, proof).filter(|grant| {
            grant.pending && grant.id().is_some_and(|id| !self.activated.contains(&id))
        })
    }
    pub(crate) fn invited_device_proofs(&self) -> Vec<Announce> {
        self.originals
            .iter()
            .filter(|proof| self.invitation_for_device(&proof.public(), proof).is_some())
            .cloned()
            .collect()
    }
    pub(super) fn device_authorization(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Option<EventId> {
        let id = self.proof_grant(public, proof)?.id()?;
        self.inherited
            .get(&(public.ed25519_pub, public.x25519_pub))
            .filter(|(registered, _)| *registered == id)
            .map(|(_, event)| *event)
            .or_else(|| self.grant_origin(id))
    }
    pub(crate) fn grant_origin(&self, id: AccountGrantId) -> Option<EventId> {
        self.origins.get(&id).copied()
    }
    pub(crate) fn accept_account(
        &mut self,
        event: &Event,
        proof: &Announce,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        self.validate_self_action(event, proof, EventKind::ChannelAccept)?;
        if let Some((_, binding)) = self.inherited.get(&(proof.ed25519_pub, proof.x25519_pub))
            && !descends(event.id, *binding, graph)?
        {
            return Err(AuthorityError::MissingAncestry);
        }
        let Some(grant) = self.proof_grant(&proof.public(), proof) else {
            return Ok(false);
        };
        let id = grant.id().ok_or(AuthorityError::Invalid)?;
        if !descends(
            event.id,
            self.grant_origin(id)
                .ok_or(AuthorityError::MissingAncestry)?,
            graph,
        )? {
            return Err(AuthorityError::Stale);
        }
        if !grant.pending {
            return Ok(false);
        }
        if !self.activated.contains(&id) {
            self.key_epoch()
                .and_then(|epoch| epoch.checked_add(1))
                .ok_or(AuthorityError::Unsupported)?;
        }
        self.acceptance_events
            .entry(id)
            .or_default()
            .insert(event.id);
        if self.activated.contains(&id) {
            return Ok(false);
        }
        self.activated.insert(id);
        if !self.had_added(id)? {
            self.added_grants.insert(id);
        }
        Ok(true)
    }
    pub(crate) fn leave_account(
        &mut self,
        event: &Event,
        proof: &Announce,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        self.validate_self_action(event, proof, EventKind::ChannelLeave)?;
        if let Some((_, binding)) = self.inherited.get(&(proof.ed25519_pub, proof.x25519_pub))
            && !descends(event.id, *binding, graph)?
        {
            return Err(AuthorityError::MissingAncestry);
        }
        let Some(grant) = self.grant_for_device(&proof.public(), proof) else {
            return Ok(false);
        };
        let id = grant.id().ok_or(AuthorityError::Invalid)?;
        if Some(id.account_pub) == self.owner {
            return Err(AuthorityError::Invalid);
        }
        if !descends(
            event.id,
            self.grant_origin(id)
                .ok_or(AuthorityError::MissingAncestry)?,
            graph,
        )? {
            return Err(AuthorityError::Stale);
        }
        self.key_epoch()
            .and_then(|epoch| epoch.checked_add(1))
            .ok_or(AuthorityError::Unsupported)?;
        if self.revoked_event(id)?.is_none() {
            self.revoked.insert(id, event.id);
        }
        self.grants.remove(&id.account_pub);
        Ok(true)
    }
    pub(crate) fn replay_leave_after_legacy_closure(
        &mut self,
        event: &Event,
        proof: &Announce,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        self.validate_self_action(event, proof, EventKind::ChannelLeave)?;
        let account = proof
            .account_cert
            .as_ref()
            .ok_or(AuthorityError::Invalid)?
            .account_ed25519_pub;
        let grant = self.grants.get(&account).ok_or(AuthorityError::Invalid)?;
        let id = grant.id().ok_or(AuthorityError::Invalid)?;
        if !self.cohort_closed(id, false)
            || if let Some((_, binding)) =
                self.inherited.get(&(proof.ed25519_pub, proof.x25519_pub))
            {
                !descends(event.id, *binding, graph)?
            } else {
                false
            }
            || Some(account) == self.owner
            || self
                .device_grants
                .get(&(proof.ed25519_pub, proof.x25519_pub))
                != Some(&id)
            || !self.originals.iter().any(|original| {
                original.public() == proof.public() && original.account_id() == proof.account_id()
            })
            || !descends(
                event.id,
                self.grant_origin(id)
                    .ok_or(AuthorityError::MissingAncestry)?,
                graph,
            )?
        {
            return Err(AuthorityError::Stale);
        }
        self.revoked.entry(id).or_insert(event.id);
        self.grants.remove(&account);
        Ok(true)
    }
    /// Revoke only an incarnation that was still pending in this action's own
    /// causal view. Concurrent Accept cannot defeat decline or reuse its epoch.
    pub(crate) fn decline_account(
        &mut self,
        event: &Event,
        proof: &Announce,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        let payload =
            AccountDeclinePayload::decode(&event.ciphertext).ok_or(AuthorityError::Invalid)?;
        if self.channel != Some(event.conversation_id)
            || event.kind != EventKind::ChannelLeave
            || !valid_event(event)
            || !proof.valid_presence()
            || proof.post_office
            || event.author.ed25519_pub() != &proof.ed25519_pub
            || Some(payload.grant.account_pub) == self.owner
        {
            return Err(AuthorityError::Invalid);
        }
        let grant = self
            .proof_grant(&proof.public(), proof)
            .ok_or(AuthorityError::Invalid)?;
        if !grant.pending || grant.id() != Some(payload.grant) {
            return Err(AuthorityError::Invalid);
        }
        if !descends(
            event.id,
            self.grant_origin(payload.grant)
                .ok_or(AuthorityError::MissingAncestry)?,
            graph,
        )? || if let Some((_, binding)) =
            self.inherited.get(&(proof.ed25519_pub, proof.x25519_pub))
        {
            !descends(event.id, *binding, graph)?
        } else {
            false
        } {
            return Err(AuthorityError::MissingAncestry);
        }
        if self.observed_acceptance(payload.grant, event.id, graph)? {
            return Err(AuthorityError::Invalid);
        }
        let increment = if self.had_added(payload.grant)?
            || self.declined_epoch_reservations.contains(&payload.grant)
        {
            1
        } else {
            2
        };
        self.key_epoch()
            .and_then(|epoch| epoch.checked_add(increment))
            .ok_or(AuthorityError::Unsupported)?;
        self.declined_epoch_reservations.insert(payload.grant);
        self.revoked.entry(payload.grant).or_insert(event.id);
        self.grants.remove(&payload.grant.account_pub);
        Ok(true)
    }
    fn validate_self_action(
        &self,
        event: &Event,
        proof: &Announce,
        kind: EventKind,
    ) -> Result<(), AuthorityError> {
        if self.channel != Some(event.conversation_id)
            || event.kind != kind
            || !event.ciphertext.is_empty()
            || !valid_event(event)
            || event.author.ed25519_pub() != &proof.ed25519_pub
            || !proof.valid_presence()
        {
            return Err(AuthorityError::Invalid);
        }
        Ok(())
    }
    pub(crate) fn is_member(&self, public: &PublicIdentity, proof: &Announce) -> bool {
        self.grant_for_device(public, proof).is_some()
    }
    pub(crate) fn can_admin(&self, public: &PublicIdentity, proof: &Announce) -> bool {
        self.grant_for_device(public, proof)
            .and_then(AccountGrant::id)
            .is_some_and(|id| Some(id.account_pub) == self.owner)
    }
    /// Applies only verified canonical controls. Stale full snapshots cannot add,
    /// transfer or resurrect; valid same-owner concurrent removals still revoke.
    pub(crate) fn apply(
        &mut self,
        event: &Event,
        legacy: &LegacyAuthorityContext,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        if event.kind != EventKind::ChannelAccountAuthority
            || event.conversation_id != legacy.channel
            || self.channel.is_some_and(|id| id != legacy.channel)
            || !valid_event(event)
        {
            return Err(AuthorityError::Invalid);
        }
        if self.known_control(event.id)? {
            return Ok(false);
        }
        let control = AccountAuthorityControl::decode(&event.ciphertext)
            .ok_or(AuthorityError::Unsupported)?;
        if control.issuer.ed25519_pub != *event.author.ed25519_pub()
            || !control.issuer.valid_presence()
        {
            return Err(AuthorityError::Invalid);
        }
        let issuer_account = control
            .issuer
            .account_cert
            .as_ref()
            .ok_or(AuthorityError::Invalid)?
            .account_ed25519_pub;
        let signing_account = self.owner.unwrap_or(issuer_account);
        if !control.verify_account_signature(legacy.channel, &signing_account) {
            return Err(AuthorityError::Invalid);
        }
        let initial = self.current.is_none();
        if initial {
            if control.previous.is_some()
                || control.issuer.public() != legacy.owner
                || issuer_account != control.owner_account
                || !control.revoked.is_empty()
            {
                return Err(AuthorityError::Invalid);
            }
        } else {
            if Some(issuer_account) != self.owner
                || !self.can_admin(&control.issuer.public(), &control.issuer)
                || !control.legacy_bindings.is_empty()
            {
                return Err(AuthorityError::Invalid);
            }
            if let Some((_, binding)) = self
                .inherited
                .get(&(control.issuer.ed25519_pub, control.issuer.x25519_pub))
                && !descends(event.id, *binding, graph)?
            {
                return Err(AuthorityError::MissingAncestry);
            }
            let previous = control.previous.ok_or(AuthorityError::MissingAncestry)?;
            if !self.known_control(previous)? || !descends(event.id, previous, graph)? {
                return Err(AuthorityError::MissingAncestry);
            }
        }
        let stale = !initial && control.previous != self.current;
        let previous_control = if let Some(previous) = control.previous {
            Some(
                AccountAuthorityControl::decode(
                    &graph
                        .event(previous)?
                        .ok_or(AuthorityError::MissingAncestry)?
                        .ciphertext,
                )
                .ok_or(AuthorityError::Invalid)?,
            )
        } else {
            None
        };
        if stale
            && previous_control.as_ref().is_none_or(|previous| {
                previous.owner_account != issuer_account || control.owner_account != issuer_account
            })
        {
            return Err(AuthorityError::Stale);
        }
        let mut pins = self.pinned.clone();
        let mut originals = self.originals.clone();
        if initial {
            if control.legacy_bindings.len() > legacy.accepted.len() + legacy.invited.len()
                || legacy
                    .accepted
                    .iter()
                    .chain(&legacy.invited)
                    .filter(|member| {
                        legacy.live.contains(&member.public)
                            || legacy
                                .invited
                                .iter()
                                .any(|invited| lifetime_key(invited) == lifetime_key(member))
                            || !self.legacy_seen.contains_key(&lifetime_key(member))
                    })
                    .any(|member| {
                        !control
                            .legacy_bindings
                            .iter()
                            .any(|proof| proof.public() == member.public)
                    })
            {
                return Err(AuthorityError::Invalid);
            }
            let mut seen = HashSet::new();
            for proof in &control.legacy_bindings {
                let member = legacy
                    .accepted
                    .iter()
                    .chain(&legacy.invited)
                    .find(|member| member.public == proof.public())
                    .ok_or(AuthorityError::MissingAncestry)?;
                if !proof.valid_presence()
                    || proof.post_office
                    || !seen.insert(member.public.user_id())
                {
                    return Err(AuthorityError::Invalid);
                }
                let account = proof
                    .account_cert
                    .as_ref()
                    .ok_or(AuthorityError::Invalid)?
                    .account_ed25519_pub;
                let binding = AccountGrant {
                    incarnation: [0; 16],
                    pending: legacy.invited.iter().any(|invited| {
                        invited.public == member.public
                            && invited.membership_event == member.membership_event
                    }),
                    original: proof.clone(),
                    membership_event: member.membership_event,
                    acceptance_event: member.acceptance_event,
                };
                validate_legacy_grant(&binding, event, legacy, graph)?;
                pins.insert(legacy_key(&binding), account);
                originals.push(proof.clone());
            }
        }
        let mut next = HashMap::new();
        let mut ids = HashSet::new();
        for grant in &control.grants {
            let id = grant.id().ok_or(AuthorityError::Invalid)?;
            if !grant.original.valid_presence()
                || grant.original.post_office
                || !ids.insert(id)
                || next.contains_key(&id.account_pub)
            {
                return Err(AuthorityError::Invalid);
            }
            if stale {
                if previous_control
                    .as_ref()
                    .is_none_or(|previous| !previous.grants.contains(grant))
                {
                    return Err(AuthorityError::Stale);
                }
            } else {
                if self.revoked_event(id)?.is_some() {
                    if previous_control
                        .as_ref()
                        .is_none_or(|previous| !previous.grants.contains(grant))
                    {
                        return Err(AuthorityError::Stale);
                    }
                    next.insert(id.account_pub, grant.clone());
                    continue;
                }
                let unchanged = self.grants.get(&id.account_pub) == Some(grant);
                if !unchanged {
                    if self
                        .grants
                        .get(&id.account_pub)
                        .is_some_and(|old| old.id() == Some(id))
                    {
                        return Err(AuthorityError::Invalid);
                    }
                    validate_legacy_grant(grant, event, legacy, graph)?;
                    match pins
                        .get(&legacy_key(grant))
                        .copied()
                        .or(self.previous_pin(legacy_key(grant))?)
                    {
                        Some(account) if account != id.account_pub => {
                            return Err(AuthorityError::Invalid);
                        }
                        Some(_) => {}
                        None => {
                            if let Some(genesis) = self.genesis
                                && !descends(grant.membership_event, genesis, graph)?
                            {
                                return Err(AuthorityError::MissingAncestry);
                            }
                            pins.insert(legacy_key(grant), id.account_pub);
                            originals.push(grant.original.clone());
                        }
                    }
                    if !self.observed_revocations(id.account_pub, grant.membership_event, graph)? {
                        return Err(AuthorityError::Stale);
                    }
                }
            }
            next.insert(id.account_pub, grant.clone());
        }
        if next
            .get(&control.owner_account)
            .is_some_and(|grant| grant.id().is_some_and(|id| self.cohort_closed(id, true)))
        {
            return Err(AuthorityError::Invalid);
        }
        let owner_grant = next
            .get(&control.owner_account)
            .ok_or(AuthorityError::Invalid)?;
        let owner_id = owner_grant.id().ok_or(AuthorityError::Invalid)?;
        if self.revoked_event(owner_id)?.is_some()
            || (owner_grant.pending && !self.activated.contains(&owner_id))
        {
            return Err(AuthorityError::Invalid);
        }
        if initial {
            let binding_accounts: HashSet<_> = pins.values().copied().collect();
            if legacy
                .accepted
                .iter()
                .filter(|member| {
                    control
                        .legacy_bindings
                        .iter()
                        .any(|proof| proof.public() == member.public)
                })
                .any(|member| {
                    control
                        .legacy_bindings
                        .iter()
                        .find(|proof| proof.public() == member.public)
                        .and_then(|proof| proof.account_cert.as_ref())
                        .and_then(|cert| next.get(&cert.account_ed25519_pub))
                        .is_none_or(|grant| grant.pending)
                })
            {
                return Err(AuthorityError::Invalid);
            }
            if binding_accounts != next.keys().copied().collect()
                || control
                    .legacy_bindings
                    .iter()
                    .find(|proof| proof.public() == legacy.owner)
                    .and_then(|proof| proof.account_cert.as_ref())
                    .is_none_or(|cert| cert.account_ed25519_pub != control.owner_account)
            {
                return Err(AuthorityError::Invalid);
            }
        }
        let revoked: HashSet<_> = control.revoked.iter().copied().collect();
        if revoked.len() != control.revoked.len() {
            return Err(AuthorityError::Invalid);
        }
        let previous_grants: HashSet<_> = if stale {
            previous_control
                .as_ref()
                .expect("stale previous")
                .grants
                .iter()
                .filter_map(AccountGrant::id)
                .collect()
        } else {
            self.grants.values().filter_map(AccountGrant::id).collect()
        };
        let next_ids: HashSet<_> = next.values().filter_map(AccountGrant::id).collect();
        let removed: HashSet<_> = previous_grants.difference(&next_ids).copied().collect();
        if removed != revoked
            || revoked
                .iter()
                .any(|id| id.account_pub == control.owner_account)
        {
            return Err(AuthorityError::Invalid);
        }
        let mut added = self.added_grants.clone();
        if !initial && !stale {
            for id in next_ids.difference(&previous_grants) {
                if self.revoked_event(*id)?.is_none()
                    && !self.had_added(*id)?
                    && next
                        .get(&id.account_pub)
                        .is_some_and(|grant| !grant.pending)
                {
                    added.insert(*id);
                }
            }
        }
        let mut tombstones = self.revoked.clone();
        for id in &revoked {
            if self.revoked_event(*id)?.is_none() {
                tombstones.entry(*id).or_insert(event.id);
            }
        }
        let base = self.base_epoch.unwrap_or(legacy.epoch);
        base.checked_add(self.historical_added)
            .and_then(|epoch| epoch.checked_add(self.historical_revoked))
            .and_then(|epoch| {
                epoch.checked_add(added.union(&self.declined_epoch_reservations).count() as u64)
            })
            .and_then(|epoch| epoch.checked_add(tombstones.len() as u64))
            .ok_or(AuthorityError::Unsupported)?;
        if !stale {
            for id in &next_ids {
                self.origins.entry(*id).or_insert(event.id);
            }
        }
        self.revoked = tombstones;
        self.added_grants = added;
        let candidate = if stale { &self.grants } else { &next };
        let mut excluded = Vec::new();
        for (account, grant) in candidate {
            if self
                .revoked_event(grant.id().ok_or(AuthorityError::Invalid)?)?
                .is_some()
            {
                excluded.push(*account);
            }
        }
        if stale {
            for account in excluded {
                self.grants.remove(&account);
            }
        } else {
            for account in excluded {
                next.remove(&account);
            }
            if initial {
                for proof in &originals {
                    if let Some(grant) = proof
                        .account_cert
                        .as_ref()
                        .and_then(|cert| next.get(&cert.account_ed25519_pub))
                    {
                        self.device_grants.insert(
                            (proof.ed25519_pub, proof.x25519_pub),
                            grant.id().expect("validated grant"),
                        );
                    }
                }
            }
            for grant in next.values() {
                let key = (grant.original.ed25519_pub, grant.original.x25519_pub);
                self.device_grants
                    .insert(key, grant.id().expect("validated grant"));
                self.inherited.remove(&key);
            }
            self.grants = next;
            self.owner = Some(control.owner_account);
            self.current = Some(event.id);
            self.channel = Some(legacy.channel);
            self.pinned = pins;
            self.originals = originals;
            if initial {
                self.genesis = Some(event.id);
                self.base_epoch = Some(legacy.epoch);
                // The initial legacy epoch already accounts for each departure in
                // its causal cut, even when another fork of that lifetime is
                // later chosen as the canonical evidence event.
                self.legacy_base = self.legacy_seen.keys().copied().collect();
                for member in &legacy.accepted {
                    if self.legacy_candidate_observed(member, event.id, graph)? {
                        self.legacy_base.insert(lifetime_key(member));
                    }
                }
                self.legacy_omitted = legacy
                    .accepted
                    .iter()
                    .filter(|member| {
                        !control
                            .legacy_bindings
                            .iter()
                            .any(|proof| proof.public() == member.public)
                            && self.legacy_seen.contains_key(&lifetime_key(member))
                    })
                    .cloned()
                    .collect();
                for grant in &control.grants {
                    let id = grant.id().expect("validated grant");
                    if grant.pending {
                        continue;
                    }
                    let originals = legacy
                        .accepted
                        .iter()
                        .filter(|member| {
                            control.legacy_bindings.iter().any(|proof| {
                                proof.public() == member.public
                                    && proof.account_cert.as_ref().is_some_and(|cert| {
                                        cert.account_ed25519_pub == id.account_pub
                                    })
                            })
                        })
                        .cloned()
                        .collect();
                    self.initial_legacy.insert(
                        id,
                        InitialLegacyCohort {
                            grant: grant.clone(),
                            originals,
                        },
                    );
                }
            }
        }
        self.valid_controls.insert(event.id);
        Ok(true)
    }
}
type LegacyBindingKey = (EventId, [u8; 32], [u8; 32]);
fn legacy_key(grant: &AccountGrant) -> LegacyBindingKey {
    (
        grant.membership_event,
        grant.original.ed25519_pub,
        grant.original.x25519_pub,
    )
}
fn validate_legacy_grant(
    grant: &AccountGrant,
    event: &Event,
    legacy: &LegacyAuthorityContext,
    graph: &impl AuthorityGraph,
) -> Result<(), AuthorityError> {
    let accepted = (if grant.pending {
        &legacy.invited
    } else {
        &legacy.accepted
    })
    .iter()
    .any(|member| {
        member.public == grant.original.public()
            && member.membership_event == grant.membership_event
            && member.acceptance_event == grant.acceptance_event
    });
    if !accepted || !descends(event.id, grant.membership_event, graph)? {
        return Err(AuthorityError::MissingAncestry);
    }
    let membership = graph
        .event(grant.membership_event)?
        .ok_or(AuthorityError::MissingAncestry)?;
    if !valid_event(&membership)
        || membership.kind != EventKind::MembershipChange
        || membership.conversation_id != event.conversation_id
    {
        return Err(AuthorityError::Invalid);
    }
    let meta = crate::channel::ChannelMeta::decode(&membership.ciphertext)
        .ok_or(AuthorityError::Invalid)?;
    if !meta.members.contains(&grant.original.public()) {
        return Err(AuthorityError::Invalid);
    }
    if grant.pending {
        if grant.acceptance_event.is_some()
            || !crate::channel::ChannelMeta::is_invitation(&membership.ciphertext)
            || grant.original.public().user_id() == meta.owner
        {
            return Err(AuthorityError::Invalid);
        }
        return Ok(());
    }
    if let Some(acceptance) = grant.acceptance_event {
        let acceptance_event = graph
            .event(acceptance)?
            .ok_or(AuthorityError::MissingAncestry)?;
        if acceptance_event.kind != EventKind::ChannelAccept
            || !acceptance_event.ciphertext.is_empty()
            || !valid_event(&acceptance_event)
            || acceptance_event.author.ed25519_pub() != &grant.original.ed25519_pub
            || acceptance_event.conversation_id != event.conversation_id
            || !descends(acceptance, membership.id, graph)?
            || !descends(event.id, acceptance, graph)?
        {
            return Err(AuthorityError::Invalid);
        }
    } else if crate::channel::ChannelMeta::is_invitation(&membership.ciphertext)
        && grant.original.public().user_id() != meta.owner
    {
        return Err(AuthorityError::Invalid);
    }
    Ok(())
}
/// Causality uses content-address ancestry, never wall clocks or roster freshness.
fn descends(
    child: EventId,
    ancestor: EventId,
    graph: &impl AuthorityGraph,
) -> Result<bool, AuthorityError> {
    graph.descends(child, ancestor).map_err(Into::into)
}

fn valid_event(event: &Event) -> bool {
    event.is_canonical() && event.verify_integrity() && event.verify_signature()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        identity::{account::Account, device::DeviceIdentity},
    };
    struct Fixture {
        owner: DeviceIdentity,
        owner_account: Account,
        a: DeviceIdentity,
        aa: Account,
        b: DeviceIdentity,
        b_account: Account,
        channel: ConversationId,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                owner: DeviceIdentity::generate(),
                owner_account: Account::generate(),
                a: DeviceIdentity::generate(),
                aa: Account::generate(),
                b: DeviceIdentity::generate(),
                b_account: Account::generate(),
                channel: ConversationId::new([19; 32]),
            }
        }
        fn genesis(&self) -> Event {
            let meta = ChannelMeta {
                name: "group".into(),
                members: vec![self.owner.public(), self.a.public(), self.b.public()],
                epoch: 7,
                owner: self.owner.public().user_id(),
            };
            self.event(
                &self.owner,
                1,
                vec![],
                EventKind::MembershipChange,
                meta.encode(),
            )
        }
        fn event(
            &self,
            signer: &DeviceIdentity,
            n: u64,
            parents: Vec<EventId>,
            kind: EventKind,
            payload: Vec<u8>,
        ) -> Event {
            let payload = if kind == EventKind::ChannelAccountAuthority {
                AccountAuthorityControl::decode(&payload)
                    .unwrap()
                    .signed_encode(self.channel, &self.owner_account)
            } else {
                payload
            };
            Event::new(signer, self.channel, n, parents, n, n, kind, payload)
        }
        fn inherit(
            &self,
            device: &DeviceIdentity,
            account: &Account,
            authority: EventId,
            grant: AccountGrantId,
        ) -> Event {
            use super::super::channel_account_bootstrap::{
                DeviceControl, inheritance_account_message,
            };
            let proof = Announce::new_with_account(device, account, "sibling", 1);
            self.event(
                device,
                99,
                vec![authority],
                EventKind::ChannelDeviceInheritance,
                DeviceControl::Inherit {
                    authority,
                    grant,
                    account_signature: account
                        .sign(&inheritance_account_message(
                            self.channel,
                            authority,
                            grant,
                            &device.public(),
                        ))
                        .to_vec(),
                    device: proof,
                }
                .encode(),
            )
        }
        fn context(&self, root: EventId) -> LegacyAuthorityContext {
            LegacyAuthorityContext {
                channel: self.channel,
                owner: self.owner.public(),
                epoch: 7,
                live: vec![self.owner.public(), self.a.public(), self.b.public()],
                invited: vec![],
                accepted: vec![self.owner.public(), self.a.public(), self.b.public()]
                    .into_iter()
                    .map(|public| LegacyAccountMember {
                        public,
                        membership_event: root,
                        acceptance_event: None,
                    })
                    .collect(),
            }
        }
        fn control(&self, root: EventId) -> AccountAuthorityControl {
            let grants: Vec<AccountGrant> = [
                (&self.owner, &self.owner_account, 1),
                (&self.a, &self.aa, 2),
                (&self.b, &self.b_account, 3),
            ]
            .into_iter()
            .map(|(device, account, n)| AccountGrant {
                incarnation: [n; 16],
                pending: false,
                original: Announce::new_with_account(device, account, "member", 1),
                membership_event: root,
                acceptance_event: None,
            })
            .collect();
            AccountAuthorityControl {
                version: 1,
                previous: None,
                issuer: Announce::new_with_account(&self.owner, &self.owner_account, "owner", 1),
                owner_account: self.owner_account.public().ed25519_pub,
                legacy_bindings: grants
                    .iter()
                    .map(|grant: &AccountGrant| grant.original.clone())
                    .collect(),
                grants,
                revoked: vec![],
                account_signature: vec![],
            }
        }
        fn remove(
            &self,
            initial: &AccountAuthorityControl,
            previous: EventId,
            account: &Account,
        ) -> AccountAuthorityControl {
            let mut control = initial.clone();
            control.previous = Some(previous);
            control.legacy_bindings.clear();
            let grant = control
                .grants
                .iter()
                .find(|grant| grant.id().unwrap().account_pub == account.public().ed25519_pub)
                .unwrap();
            control.revoked = vec![grant.id().unwrap()];
            control
                .grants
                .retain(|grant| grant.id().unwrap().account_pub != account.public().ed25519_pub);
            control
        }
    }
    fn projected(f: &Fixture, events: &[&Event]) -> AccountAuthority {
        crate::node::channel::ChannelBook::project_membership(f.channel, events).authority
    }
    fn same_account_initial(
        f: &Fixture,
        root: EventId,
        owner_cohort: bool,
    ) -> AccountAuthorityControl {
        let mut initial = f.control(root);
        let account = if owner_cohort {
            &f.owner_account
        } else {
            &f.aa
        };
        initial.legacy_bindings[2] =
            Announce::new_with_account(&f.b, account, "second original", 1);
        initial.grants.remove(2);
        initial
    }
    #[test]
    fn initial_cohort_departures_preserve_remaining_member_and_distinct_epoch_ledger() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = same_account_initial(&f, root.id, false);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let first = f.event(&f.a, 3, vec![root.id], EventKind::ChannelLeave, vec![]);
        let second = f.event(&f.b, 4, vec![root.id], EventKind::ChannelLeave, vec![]);
        let partial = projected(&f, &[&root, &upgrade, &first]);
        assert!(partial.account_grant(&f.aa.public().ed25519_pub).is_some());
        assert_eq!(partial.key_epoch(), Some(8));
        let modern = f.event(
            &f.b,
            5,
            vec![upgrade.id, first.id],
            EventKind::ChannelLeave,
            vec![],
        );
        let modern_partial = projected(&f, &[&root, &upgrade, &first, &modern]);
        assert!(
            modern_partial
                .account_grant(&f.aa.public().ed25519_pub)
                .is_none()
        );
        assert_eq!(modern_partial.key_epoch(), Some(9));
        for events in [
            vec![&root, &upgrade, &first, &second],
            vec![&second, &first, &upgrade, &root],
            vec![&root, &upgrade, &first, &modern, &second],
        ] {
            let projection =
                crate::node::channel::ChannelBook::project_membership(f.channel, &events);
            if events.iter().any(|event| event.id == modern.id) {
                assert!(
                    projection.authorized.contains(&modern.id),
                    "a valid modern departure remains admitted after concurrent legacy closure"
                );
            }
            let closed = projection.authority;
            assert!(closed.account_grant(&f.aa.public().ed25519_pub).is_none());
            assert_eq!(closed.key_epoch(), Some(9));
            assert!(
                closed
                    .account_grant(&f.owner_account.public().ed25519_pub)
                    .is_some()
            );
        }
    }
    #[test]
    fn closed_initial_account_cannot_receive_ownership_but_unrelated_controls_survive() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let leave = f.event(&f.a, 5, vec![root.id], EventKind::ChannelLeave, vec![]);
        let mut transfer = initial.clone();
        transfer.previous = Some(upgrade.id);
        transfer.legacy_bindings.clear();
        transfer.owner_account = f.aa.public().ed25519_pub;
        let invalid = f.event(
            &f.owner,
            3,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            transfer.encode(),
        );
        let rejected = projected(&f, &[&root, &upgrade, &invalid, &leave]);
        assert_eq!(
            rejected.owner_account_id(),
            Some(f.owner_account.account_id())
        );
        assert_eq!(rejected.current_event(), Some(upgrade.id));
        transfer.owner_account = f.b_account.public().ed25519_pub;
        let valid = f.event(
            &f.owner,
            4,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            transfer.encode(),
        );
        let accepted = projected(&f, &[&root, &upgrade, &valid, &leave]);
        assert_eq!(accepted.current_event(), Some(valid.id));
        assert_eq!(accepted.owner_account_id(), Some(f.b_account.account_id()));
        assert!(accepted.account_grant(&f.aa.public().ed25519_pub).is_none());
        assert_eq!(accepted.key_epoch(), Some(8));
    }
    #[test]
    fn initial_owner_sibling_departure_rotates_epoch_without_retiring_owner() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = same_account_initial(&f, root.id, true);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let leave = f.event(&f.b, 3, vec![root.id], EventKind::ChannelLeave, vec![]);
        let state = projected(&f, &[&root, &upgrade, &leave]);
        assert_eq!(state.current_event(), Some(upgrade.id));
        assert_eq!(state.key_epoch(), Some(8));
        assert!(state.can_admin(&f.owner.public(), &initial.issuer));
        assert!(
            state
                .account_grant(&f.owner_account.public().ed25519_pub)
                .is_some()
        );
    }
    #[test]
    fn fresh_account_incarnation_is_not_revoked_by_concurrent_initial_legacy_leave() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let modern_leave = f.event(&f.a, 3, vec![upgrade.id], EventKind::ChannelLeave, vec![]);
        let fresh_root = f.event(
            &f.owner,
            4,
            vec![modern_leave.id],
            EventKind::MembershipChange,
            ChannelMeta {
                name: "fresh accepted lifetime".into(),
                members: vec![f.owner.public(), f.a.public(), f.b.public()],
                epoch: 9,
                owner: f.owner.user_id(),
            }
            .encode(),
        );
        let mut regrant = initial.clone();
        regrant.previous = Some(upgrade.id);
        regrant.legacy_bindings.clear();
        regrant.grants[1].incarnation = [99; 16];
        regrant.grants[1].membership_event = fresh_root.id;
        let fresh = f.event(
            &f.owner,
            5,
            vec![fresh_root.id],
            EventKind::ChannelAccountAuthority,
            regrant.encode(),
        );
        let old_leave = f.event(&f.a, 6, vec![root.id], EventKind::ChannelLeave, vec![]);
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (modern_leave.id, &modern_leave),
            (fresh_root.id, &fresh_root),
            (fresh.id, &fresh),
            (old_leave.id, &old_leave),
        ]);
        assert!(!descends(fresh.id, old_leave.id, &graph).unwrap());
        let before = projected(&f, &[&root, &upgrade, &modern_leave, &fresh_root, &fresh]);
        assert_eq!(before.current_event(), Some(fresh.id));
        let after = projected(
            &f,
            &[
                &root,
                &upgrade,
                &modern_leave,
                &fresh_root,
                &fresh,
                &old_leave,
            ],
        );
        assert_eq!(after.current_event(), Some(fresh.id));
        let active = after
            .account_grant(&f.aa.public().ed25519_pub)
            .expect("fresh accepted grant remains active");
        assert_eq!(active.id(), regrant.grants[1].id());
        assert_ne!(active.id(), initial.grants[1].id());
        assert!(after.is_member(&f.a.public(), &initial.grants[1].original));
        assert!(after.key_epoch().unwrap() >= before.key_epoch().unwrap());
    }
    #[test]
    fn initial_cut_acknowledges_a_departure_lifetime_not_only_its_selected_fork_id() {
        let f = Fixture::new();
        let root = f.genesis();
        // Force the later imported fork to become the canonical lower ID;
        // the initial cut still causally acknowledges only the other fork.
        // Vary both signed candidates, rather than retaining a potentially
        // near-minimal fixed hash as the search target.
        let (left, fork) = (0..1024)
            .find_map(|timestamp| {
                let left = Event::new(
                    &f.a,
                    f.channel,
                    2,
                    vec![root.id],
                    2,
                    timestamp,
                    EventKind::ChannelLeave,
                    vec![],
                );
                let fork = Event::new(
                    &f.a,
                    f.channel,
                    5,
                    vec![root.id],
                    5,
                    timestamp,
                    EventKind::ChannelLeave,
                    vec![],
                );
                (fork.id < left.id).then_some((left, fork))
            })
            .expect("an ordered pair of content-addressed signed forks");
        assert!(fork.id < left.id);
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            4,
            vec![left.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let first = projected(&f, &[&root, &left, &upgrade]);
        assert_eq!(first.current_event(), Some(upgrade.id));
        assert_eq!(first.key_epoch(), Some(8));
        let duplicate = projected(&f, &[&root, &left, &upgrade, &fork]);
        assert_eq!(duplicate.key_epoch(), first.key_epoch());
        assert!(
            duplicate
                .account_grant(&f.aa.public().ed25519_pub)
                .is_none()
        );
    }
    #[test]
    fn initial_cut_cannot_omit_pending_legacy_invitation_bindings() {
        let f = Fixture::new();
        let root = f.event(
            &f.owner,
            1,
            vec![],
            EventKind::MembershipChange,
            ChannelMeta {
                name: "pending originals".into(),
                members: vec![f.owner.public(), f.a.public(), f.b.public()],
                epoch: 7,
                owner: f.owner.user_id(),
            }
            .encode_invitation(),
        );
        let mut context = f.context(root.id);
        context.invited = context.accepted.split_off(1);
        context.live = vec![f.owner.public()];
        let mut initial = f.control(root.id);
        initial.grants[1].pending = true;
        initial.grants[2].pending = true;
        let valid = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let graph = HashMap::from([(root.id, &root), (valid.id, &valid)]);
        AccountAuthority::default()
            .apply(&valid, &context, &graph)
            .unwrap();
        initial.legacy_bindings.pop();
        initial.grants.pop();
        let omission = f.event(
            &f.owner,
            3,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let graph = HashMap::from([(root.id, &root), (omission.id, &omission)]);
        assert_eq!(
            AccountAuthority::default().apply(&omission, &context, &graph),
            Err(AuthorityError::Invalid)
        );
    }
    #[test]
    fn stolen_device_signing_key_cannot_substitute_exchange_key_or_manage_account() {
        use super::super::channel_account_bootstrap::DeviceControl;
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let sibling = DeviceIdentity::generate();
        let stolen = DeviceIdentity::from_secret_bytes(
            sibling.secret_bytes().0,
            DeviceIdentity::generate().secret_bytes().1,
        );
        let legitimate = f.inherit(
            &sibling,
            &f.owner_account,
            upgrade.id,
            initial.grants[0].id().unwrap(),
        );
        let mut forged_body = DeviceControl::decode(&legitimate.ciphertext).unwrap();
        if let DeviceControl::Inherit { device, .. } = &mut forged_body {
            *device = Announce::new_with_account(&stolen, &f.owner_account, "substituted", 1);
        }
        let forged = f.event(
            &stolen,
            3,
            vec![upgrade.id],
            EventKind::ChannelDeviceInheritance,
            forged_body.encode(),
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (legitimate.id, &legitimate),
            (forged.id, &forged),
        ]);
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        assert_eq!(
            state.inherit_device(&forged, &graph),
            Err(AuthorityError::Invalid)
        );
        state.inherit_device(&legitimate, &graph).unwrap();
        let forged_proof = Announce::new_with_account(&stolen, &f.owner_account, "substituted", 1);
        assert!(!state.can_admin(&stolen.public(), &forged_proof));
        let mut control = initial.clone();
        control.previous = Some(upgrade.id);
        control.legacy_bindings.clear();
        control.issuer = forged_proof;
        let changed = f.event(
            &stolen,
            4,
            vec![legitimate.id],
            EventKind::ChannelAccountAuthority,
            control.encode(),
        );
        let mut graph = graph;
        graph.insert(changed.id, &changed);
        assert_eq!(
            state.apply(&changed, &f.context(root.id), &graph),
            Err(AuthorityError::Invalid)
        );
        // Even a pinned device's valid Ed signature cannot authorize modified account controls.
        control.issuer = initial.issuer.clone();
        let signed = control.signed_encode(f.channel, &f.owner_account);
        let mut tampered = AccountAuthorityControl::decode(&signed).unwrap();
        tampered.owner_account = f.aa.public().ed25519_pub;
        let event = Event::new(
            &f.owner,
            f.channel,
            5,
            vec![upgrade.id],
            5,
            5,
            EventKind::ChannelAccountAuthority,
            tampered.encode(),
        );
        graph.insert(event.id, &event);
        assert_eq!(
            state.apply(&event, &f.context(root.id), &graph),
            Err(AuthorityError::Invalid)
        );
    }
    #[test]
    fn concurrent_account_removals_union_and_old_grants_never_reappear() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let a = f.event(
            &f.owner,
            3,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            f.remove(&initial, upgrade.id, &f.aa).encode(),
        );
        let b = f.event(
            &f.owner,
            4,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            f.remove(&initial, upgrade.id, &f.b_account).encode(),
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (a.id, &a),
            (b.id, &b),
        ]);
        let context = f.context(root.id);
        for removals in [[&a, &b], [&b, &a]] {
            let mut state = AccountAuthority::default();
            state.apply(&upgrade, &context, &graph).unwrap();
            state.apply(removals[0], &context, &graph).unwrap();
            state
                .apply(removals[1], &context, &graph)
                .expect("concurrent removal must survive stale full snapshot");
            assert_eq!(state.grants().len(), 1);
            assert_eq!(state.revoked().len(), 2);
            assert_eq!(state.key_epoch(), Some(9));
            assert!(!state.is_member(
                &f.a.public(),
                &Announce::new_with_account(&f.a, &f.aa, "a", 1)
            ));
        }
    }
    #[test]
    fn account_certificate_cannot_forge_original_device_binding() {
        let f = Fixture::new();
        let root = f.genesis();
        let mut control = f.control(root.id);
        let attacker = Account::generate();
        control.grants[1].original.account_cert = Some(attacker.certify(&f.a.public().ed25519_pub));
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            control.encode(),
        );
        let graph = HashMap::from([(root.id, &root), (upgrade.id, &upgrade)]);
        assert_eq!(
            AccountAuthority::default().apply(&upgrade, &f.context(root.id), &graph),
            Err(AuthorityError::Invalid)
        );
    }
    #[test]
    fn owner_role_is_available_to_new_certified_device_without_owner_device_secret() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let sibling = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&sibling, &f.owner_account, "new owner device", 1);
        let inheritance = f.inherit(
            &sibling,
            &f.owner_account,
            upgrade.id,
            initial.grants[0].id().unwrap(),
        );
        let mut next = initial.clone();
        next.previous = Some(upgrade.id);
        next.legacy_bindings.clear();
        next.issuer = proof.clone();
        let next_event = f.event(
            &sibling,
            3,
            vec![inheritance.id],
            EventKind::ChannelAccountAuthority,
            next.encode(),
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (inheritance.id, &inheritance),
            (next_event.id, &next_event),
        ]);
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        assert!(!state.can_admin(&sibling.public(), &proof));
        state.inherit_device(&inheritance, &graph).unwrap();
        assert!(state.can_admin(&sibling.public(), &proof));
        state
            .apply(&next_event, &f.context(root.id), &graph)
            .unwrap();
        assert!(state.can_admin(&sibling.public(), &proof));
    }
    #[test]
    fn concurrent_checkpoint_cannot_restore_revoked_grant_or_split_future_authority() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let remove_a = f.remove(&initial, upgrade.id, &f.aa);
        let a = f.event(
            &f.owner,
            3,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            remove_a.encode(),
        );
        let b = f.event(
            &f.owner,
            4,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            f.remove(&initial, upgrade.id, &f.b_account).encode(),
        );
        let mut checkpoint = remove_a.clone();
        checkpoint.previous = Some(a.id);
        checkpoint.revoked.clear();
        let checkpoint = f.event(
            &f.owner,
            5,
            vec![a.id],
            EventKind::ChannelAccountAuthority,
            checkpoint.encode(),
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (a.id, &a),
            (b.id, &b),
            (checkpoint.id, &checkpoint),
        ]);
        for order in [[&a, &checkpoint, &b], [&a, &b, &checkpoint]] {
            let mut state = AccountAuthority::default();
            state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
            for event in order {
                state.apply(event,&f.context(root.id),&graph).expect("concurrent valid checkpoint must stay usable without restoring removed account");
            }
            assert_eq!(state.current_event(), Some(checkpoint.id));
            assert_eq!(state.grants().len(), 1);
            assert_eq!(state.key_epoch(), Some(9));
        }
    }
    #[test]
    fn late_account_adoption_cannot_relabel_accepted_legacy_grant() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let adopted = Account::generate();
        let mut next = initial.clone();
        next.previous = Some(upgrade.id);
        next.legacy_bindings.clear();
        next.revoked = vec![next.grants[1].id().unwrap()];
        next.grants[1].incarnation = [9; 16];
        next.grants[1].original =
            Announce::new_with_account(&f.a, &adopted, "same old device, new account", 1);
        let replacement = f.event(
            &f.owner,
            3,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            next.encode(),
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (replacement.id, &replacement),
        ]);
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        assert_eq!(
            state.apply(&replacement, &f.context(root.id), &graph),
            Err(AuthorityError::Invalid)
        );
        assert!(state.is_member(&f.a.public(), &initial.grants[1].original));
    }
    #[test]
    fn pending_account_accepts_without_online_owner_and_only_once_per_account() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&device, &account, "new account", 1);
        let meta = ChannelMeta {
            name: "group".into(),
            members: vec![
                f.owner.public(),
                f.a.public(),
                f.b.public(),
                device.public(),
            ],
            epoch: 8,
            owner: f.owner.public().user_id(),
        };
        let invite = f.event(
            &f.owner,
            3,
            vec![upgrade.id],
            EventKind::MembershipChange,
            meta.encode_invitation(),
        );
        let mut grant = initial.clone();
        grant.previous = Some(upgrade.id);
        grant.legacy_bindings.clear();
        grant.grants.push(AccountGrant {
            incarnation: [8; 16],
            pending: true,
            original: proof.clone(),
            membership_event: invite.id,
            acceptance_event: None,
        });
        let grant_event = f.event(
            &f.owner,
            4,
            vec![invite.id],
            EventKind::ChannelAccountAuthority,
            grant.encode(),
        );
        let sibling = DeviceIdentity::generate();
        let sibling_proof = Announce::new_with_account(&sibling, &account, "account sibling", 1);
        let inheritance = f.inherit(
            &sibling,
            &account,
            grant_event.id,
            grant.grants.last().unwrap().id().unwrap(),
        );
        let accept = f.event(
            &sibling,
            5,
            vec![inheritance.id],
            EventKind::ChannelAccept,
            vec![],
        );
        let graph = HashMap::from([
            (root.id, &root),
            (inheritance.id, &inheritance),
            (upgrade.id, &upgrade),
            (invite.id, &invite),
            (grant_event.id, &grant_event),
            (accept.id, &accept),
        ]);
        let mut context = f.context(root.id);
        context.invited.push(LegacyAccountMember {
            public: device.public(),
            membership_event: invite.id,
            acceptance_event: None,
        });
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        state.apply(&grant_event, &context, &graph).unwrap();
        state.inherit_device(&inheritance, &graph).unwrap();
        assert!(!state.is_member(&sibling.public(), &sibling_proof));
        assert!(
            state
                .invitation_for_device(&sibling.public(), &sibling_proof)
                .is_some()
        );
        assert_eq!(state.key_epoch(), Some(7));
        assert!(
            state
                .accept_account(&accept, &sibling_proof, &graph)
                .unwrap()
        );
        assert!(state.is_member(&device.public(), &proof));
        assert!(state.is_member(&sibling.public(), &sibling_proof));
        assert_eq!(state.key_epoch(), Some(8));
        assert!(
            !state
                .accept_account(&accept, &sibling_proof, &graph)
                .unwrap()
        );
        assert_eq!(state.key_epoch(), Some(8));
    }
    #[test]
    fn account_leave_revokes_all_certified_devices_and_owner_cannot_leave() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let sibling = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&sibling, &f.aa, "sibling", 1);
        let inheritance = f.inherit(&sibling, &f.aa, upgrade.id, initial.grants[1].id().unwrap());
        let leave = f.event(
            &sibling,
            3,
            vec![inheritance.id],
            EventKind::ChannelLeave,
            vec![],
        );
        let owner_leave = f.event(
            &f.owner,
            4,
            vec![upgrade.id],
            EventKind::ChannelLeave,
            vec![],
        );
        let graph = HashMap::from([
            (root.id, &root),
            (inheritance.id, &inheritance),
            (upgrade.id, &upgrade),
            (leave.id, &leave),
            (owner_leave.id, &owner_leave),
        ]);
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        state.inherit_device(&inheritance, &graph).unwrap();
        assert!(state.leave_account(&leave, &proof, &graph).unwrap());
        assert!(!state.is_member(&f.a.public(), &initial.grants[1].original));
        assert!(!state.is_member(&sibling.public(), &proof));
        assert_eq!(state.key_epoch(), Some(8));
        assert_eq!(
            state.leave_account(&owner_leave, &initial.issuer, &graph),
            Err(AuthorityError::Invalid)
        );
    }
    #[test]
    fn retired_owner_cannot_control_after_account_owner_transfer() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let mut transfer = initial.clone();
        transfer.previous = Some(upgrade.id);
        transfer.legacy_bindings.clear();
        transfer.owner_account = f.aa.public().ed25519_pub;
        let transferred = f.event(
            &f.owner,
            3,
            vec![upgrade.id],
            EventKind::ChannelAccountAuthority,
            transfer.encode(),
        );
        let mut old = transfer.clone();
        old.previous = Some(transferred.id);
        let retired = f.event(
            &f.owner,
            4,
            vec![transferred.id],
            EventKind::ChannelAccountAuthority,
            old.encode(),
        );
        let mut current = old;
        current.issuer = initial.grants[1].original.clone();
        let new_owner = Event::new(
            &f.a,
            f.channel,
            5,
            vec![transferred.id],
            5,
            5,
            EventKind::ChannelAccountAuthority,
            current.signed_encode(f.channel, &f.aa),
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (transferred.id, &transferred),
            (retired.id, &retired),
            (new_owner.id, &new_owner),
        ]);
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        state
            .apply(&transferred, &f.context(root.id), &graph)
            .unwrap();
        assert!(!state.can_admin(&f.owner.public(), &initial.issuer));
        assert!(state.can_admin(&f.a.public(), &current.issuer));
        assert_eq!(
            state.apply(&retired, &f.context(root.id), &graph),
            Err(AuthorityError::Invalid)
        );
        state
            .apply(&new_owner, &f.context(root.id), &graph)
            .unwrap();
    }
    #[test]
    fn old_self_leave_cannot_revoke_fresh_account_invitation() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let leave = f.event(&f.a, 3, vec![upgrade.id], EventKind::ChannelLeave, vec![]);
        let meta = ChannelMeta {
            name: "group".into(),
            members: vec![f.owner.public(), f.a.public(), f.b.public()],
            epoch: 9,
            owner: f.owner.user_id(),
        };
        let fresh_root = f.event(
            &f.owner,
            4,
            vec![leave.id],
            EventKind::MembershipChange,
            meta.encode(),
        );
        let mut regrant = initial.clone();
        regrant.previous = Some(upgrade.id);
        regrant.legacy_bindings.clear();
        regrant.grants[1].incarnation = [99; 16];
        regrant.grants[1].membership_event = fresh_root.id;
        let fresh_grant = f.event(
            &f.owner,
            5,
            vec![fresh_root.id],
            EventKind::ChannelAccountAuthority,
            regrant.encode(),
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (leave.id, &leave),
            (fresh_root.id, &fresh_root),
            (fresh_grant.id, &fresh_grant),
        ]);
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        assert!(
            state
                .leave_account(&leave, &initial.grants[1].original, &graph)
                .unwrap()
        );
        let mut context = f.context(root.id);
        context.accepted[1].membership_event = fresh_root.id;
        state.apply(&fresh_grant, &context, &graph).unwrap();
        assert!(state.is_member(&f.a.public(), &initial.grants[1].original));
        assert_eq!(
            state.leave_account(&leave, &initial.grants[1].original, &graph),
            Err(AuthorityError::Stale)
        );
        assert!(state.is_member(&f.a.public(), &initial.grants[1].original));
        assert_eq!(state.key_epoch(), Some(9));
    }
    #[test]
    fn inherited_self_action_must_descend_its_account_root_device_authorization() {
        let f = Fixture::new();
        let root = f.genesis();
        let initial = f.control(root.id);
        let upgrade = f.event(
            &f.owner,
            2,
            vec![root.id],
            EventKind::ChannelAccountAuthority,
            initial.encode(),
        );
        let sibling = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&sibling, &f.aa, "sibling", 1);
        let inheritance = f.inherit(&sibling, &f.aa, upgrade.id, initial.grants[1].id().unwrap());
        let unrelated_leave = f.event(
            &sibling,
            100,
            vec![upgrade.id],
            EventKind::ChannelLeave,
            vec![],
        );
        let legitimate_leave = f.event(
            &sibling,
            101,
            vec![inheritance.id],
            EventKind::ChannelLeave,
            vec![],
        );
        let graph = HashMap::from([
            (root.id, &root),
            (upgrade.id, &upgrade),
            (inheritance.id, &inheritance),
            (unrelated_leave.id, &unrelated_leave),
            (legitimate_leave.id, &legitimate_leave),
        ]);
        let mut state = AccountAuthority::default();
        state.apply(&upgrade, &f.context(root.id), &graph).unwrap();
        state.inherit_device(&inheritance, &graph).unwrap();
        assert_eq!(
            state.leave_account(&unrelated_leave, &proof, &graph),
            Err(AuthorityError::MissingAncestry)
        );
        assert!(
            state
                .leave_account(&legitimate_leave, &proof, &graph)
                .unwrap()
        );
    }

    #[test]
    fn pending_decline_epoch_reservation_overflow_is_atomic() {
        let f = crate::node::channels::account_self_control::tests::fixture(true);
        let grant = f.witness().grant;
        let mut state = f.projection().authority;
        state.base_epoch = Some(u64::MAX - 1);
        let decline = Event::new(
            &f.member,
            f.group,
            1,
            vec![f.events.last().unwrap().id],
            3,
            3,
            EventKind::ChannelLeave,
            AccountDeclinePayload { version: 1, grant }.encode(),
        );
        let mut events = f.events.clone();
        events.push(decline.clone());
        let graph: HashMap<_, _> = events.iter().map(|event| (event.id, event)).collect();
        assert_eq!(
            state.decline_account(&decline, &f.proof, &graph),
            Err(AuthorityError::Unsupported)
        );
        assert_eq!(state.key_epoch(), Some(u64::MAX - 1));
        assert!(state.revoked.is_empty() && state.declined_epoch_reservations.is_empty());
        assert!(
            state
                .invitation_for_device(&f.member.public(), &f.proof)
                .is_some()
        );
    }

    #[test]
    fn declined_incarnation_cannot_resurrect_or_revoke_a_later_invitation() {
        let f = crate::node::channels::account_self_control::tests::fixture(true);
        let original_id = f.witness().grant;
        let original = AccountAuthorityControl::decode(&f.events[1].ciphertext).unwrap();
        let mut state = f.projection().authority;
        let decline = Event::new(
            &f.member,
            f.group,
            1,
            vec![f.events[1].id],
            3,
            3,
            EventKind::ChannelLeave,
            AccountDeclinePayload {
                version: 1,
                grant: original_id,
            }
            .encode(),
        );
        let fresh_root = Event::new(
            &f.owner,
            f.group,
            3,
            vec![decline.id],
            4,
            4,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "reinvite".into(),
                members: vec![f.owner.public(), f.member.public()],
                epoch: 2,
                owner: f.owner.user_id(),
            }
            .encode_invitation(),
        );
        let mut fresh = original.clone();
        fresh.previous = Some(f.events[1].id);
        fresh.legacy_bindings.clear();
        let fresh_member = fresh
            .grants
            .iter_mut()
            .find(|grant| grant.id() == Some(original_id))
            .unwrap();
        fresh_member.incarnation = [99; 16];
        fresh_member.membership_event = fresh_root.id;
        let fresh_id = fresh_member.id().unwrap();
        let fresh_event = Event::new(
            &f.owner,
            f.group,
            4,
            vec![fresh_root.id],
            5,
            5,
            EventKind::ChannelAccountAuthority,
            fresh.signed_encode(f.group, &f.owner_account),
        );
        let mut stale = original.clone();
        stale.previous = Some(f.events[1].id);
        stale.legacy_bindings.clear();
        let stale_event = Event::new(
            &f.owner,
            f.group,
            5,
            vec![fresh_event.id],
            6,
            6,
            EventKind::ChannelAccountAuthority,
            stale.signed_encode(f.group, &f.owner_account),
        );
        let mut events = f.events.clone();
        events.extend([
            decline.clone(),
            fresh_root.clone(),
            fresh_event.clone(),
            stale_event.clone(),
        ]);
        let graph: HashMap<_, _> = events.iter().map(|event| (event.id, event)).collect();
        state.decline_account(&decline, &f.proof, &graph).unwrap();
        assert_eq!(state.key_epoch(), Some(2));
        let context = LegacyAuthorityContext {
            channel: f.group,
            owner: f.owner.public(),
            epoch: 2,
            live: vec![f.owner.public(), f.member.public()],
            accepted: vec![LegacyAccountMember {
                public: f.owner.public(),
                membership_event: f.events[0].id,
                acceptance_event: None,
            }],
            invited: vec![LegacyAccountMember {
                public: f.member.public(),
                membership_event: fresh_root.id,
                acceptance_event: None,
            }],
        };
        state.apply(&fresh_event, &context, &graph).unwrap();
        assert_eq!(
            state
                .invitation_for_device(&f.member.public(), &f.proof)
                .and_then(AccountGrant::id),
            Some(fresh_id)
        );
        assert!(state.decline_account(&decline, &f.proof, &graph).is_err());
        state.apply(&stale_event, &context, &graph).unwrap();
        assert_eq!(
            state
                .invitation_for_device(&f.member.public(), &f.proof)
                .and_then(AccountGrant::id),
            Some(fresh_id),
            "an old owner snapshot or old decline must neither resurrect the old incarnation nor revoke the fresh one"
        );
    }
}
