use super::wire::{
    AccountObservation, BoundSource, GroupControlHead, HeadReference, MAX_ACCESS_FRONTIER,
    MAX_OBSERVED_SOURCES, SourceAccess,
};
use super::*;
use crate::{
    eventlog::{EventKind, persist::PersistentEventLog},
    node::{Node, NodeError},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) fn rank(role: AccountGroupRole) -> u8 {
    match role {
        AccountGroupRole::Owner => 3,
        AccountGroupRole::Member => 2,
        AccountGroupRole::Invited => 1,
        _ => 0,
    }
}
fn present(
    log: &PersistentEventLog,
    group: ConversationId,
    id: EventId,
) -> Result<bool, crate::eventlog::LogError> {
    Ok(log.event_conversation(&id)? == Some(group))
}
fn all_present(
    log: &PersistentEventLog,
    group: ConversationId,
    ids: impl IntoIterator<Item = EventId>,
) -> Result<bool, crate::eventlog::LogError> {
    for id in ids {
        if !present(log, group, id)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn view_contains(
    log: &PersistentEventLog,
    group: ConversationId,
    stamp: ([u8; 32], usize),
    access: &SourceAccess,
) -> Result<bool, crate::eventlog::LogError> {
    Ok(
        (access.fingerprint == stamp.0 && access.event_count == stamp.1 as u64)
            || access.complete_frontier
                && all_present(log, group, access.frontier.iter().copied())?,
    )
}
fn local_receipt_contains(
    log: &PersistentEventLog,
    group: ConversationId,
    access: &SourceAccess,
) -> Result<bool, crate::eventlog::LogError> {
    Ok(!access.local_frontier.is_empty()
        && all_present(log, group, access.local_frontier.iter().copied())?)
}
fn evaluated(
    log: &PersistentEventLog,
    group: ConversationId,
    access: &SourceAccess,
) -> Result<bool, crate::eventlog::LogError> {
    let sources = std::iter::once(&access.source)
        .chain(access.observation.sources.iter().map(|bound| &bound.source));
    for source in sources {
        if !all_present(
            log,
            group,
            std::iter::once(source.lifetime.membership)
                .chain(source.lifetime.acceptance)
                .chain(source.authority.authority),
        )? {
            return Ok(false);
        }
    }
    all_present(log, group, access.ownership)
}
fn scope_contains(
    scope: Option<&crate::node::channel::ChannelMembershipProjection>,
    ids: &[EventId],
) -> Result<bool, crate::eventlog::LogError> {
    if let Some(scope) = scope {
        for id in ids {
            if !scope.contains_event(*id)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
#[path = "access/view.rs"]
mod raw_view;
pub(in crate::node) use raw_view::{DirectoryView, RawViewCache};
type RawAuthorizationView = (Vec<EventId>, ([u8; 32], usize));
impl Node {
    pub(in crate::node) fn directory_observation(
        &self,
        log: &PersistentEventLog,
        group: ConversationId,
    ) -> Result<DirectoryView, crate::eventlog::LogError> {
        raw_view::directory_view(self, log, group)
    }
}
fn authorization_view(
    node: &Node,
    log: &PersistentEventLog,
    group: ConversationId,
) -> Result<RawAuthorizationView, crate::eventlog::LogError> {
    raw_view::read(node, log, group, None)
}
fn authorization_view_at(
    node: &Node,
    log: &PersistentEventLog,
    group: ConversationId,
    scope: Option<&crate::node::channel::ChannelMembershipProjection>,
) -> Result<RawAuthorizationView, crate::eventlog::LogError> {
    raw_view::read(node, log, group, scope)
}

/// Immutable identity inputs captured before acquiring the public event log.
/// Privacy sync readers acquire privacy before log; role projection must never
/// call back into privacy or roster owners while retaining that log.
struct GroupProofSnapshot {
    account_public: [u8; 32],
    current: BTreeMap<[u8; 32], crate::discovery::Announce>,
    historical: Vec<crate::discovery::Announce>,
    owners: Vec<crate::discovery::Announce>,
}
impl GroupProofSnapshot {
    fn binding_current(&self, proof: &crate::discovery::Announce) -> bool {
        proof.verify()
            && proof
                .account_cert
                .as_ref()
                .is_some_and(|certificate| certificate.account_ed25519_pub == self.account_public)
            && self.current.get(&proof.ed25519_pub).is_none_or(|current| {
                current.public() == proof.public() && current.account_id() == proof.account_id()
            })
    }
}

/// A legacy physical member can retire itself while its owner is offline.
/// Reconstruct only the exact signed leave's original causal membership cut;
/// no prior private head and no account-wide grant is manufactured.
fn departed_legacy_source(
    node: &Node,
    group: ConversationId,
    log: &PersistentEventLog,
    projection: &crate::node::channel::ChannelMembershipProjection,
    own: &crate::identity::device::PublicIdentity,
    proof: &crate::discovery::Announce,
    proofs: &GroupProofSnapshot,
) -> Result<Option<(AccountGroupSource, EventId)>, crate::eventlog::LogError> {
    let Some(state) = projection.state.as_ref() else {
        return Ok(None);
    };
    if (projection.authority.current_event().is_some()
        && projection
            .authority
            .try_omitted_legacy_departure(own, proof)?
            .is_none())
        || state.members().contains(own)
        || !proofs.binding_current(proof)
        || proof.public() != *own
    {
        return Ok(None);
    }
    let Some(departure_id) =
        projection.latest_authorized_event(EventKind::ChannelLeave, Some(&own.ed25519_pub))?
    else {
        return Ok(None);
    };
    let Some(departure) = log.get(&departure_id)? else {
        return Ok(None);
    };
    if departure.conversation_id != group
        || departure.author.ed25519_pub() != &own.ed25519_pub
        || departure.kind != EventKind::ChannelLeave
        || !departure.ciphertext.is_empty()
        || !departure.verify_signature()
        || !departure.verify_integrity()
    {
        return Ok(None);
    }
    let Some(original) =
        node.channel_checkpoints
            .causal_projection(log.repository()?, group, departure.id)?
    else {
        return Ok(None);
    };
    let Some(mut source) = Node::public_account_group_source(group, &original, own, proof) else {
        return Ok(None);
    };
    if source.role != AccountGroupRole::Member
        || source.authority.authority.is_some()
        || source.lifetime.member != *own
    {
        return Ok(None);
    }
    source.role = AccountGroupRole::Left;
    source.authority.epoch = state.epoch();
    Ok(Some((source, departure.id)))
}

/// Recover only terminal cleanup provenance from an authorized dissolution's
/// dependency-closed parent cut. This never grants current group authority.
fn dissolved_account_sources(
    node: &Node,
    group: ConversationId,
    log: &PersistentEventLog,
    projection: &crate::node::channel::ChannelMembershipProjection,
    proofs: &GroupProofSnapshot,
    own_proof: &crate::discovery::Announce,
) -> Result<Vec<AccountGroupSource>, crate::eventlog::LogError> {
    let Some(state) = projection
        .state
        .as_ref()
        .filter(|state| state.is_dissolved())
    else {
        return Ok(Vec::new());
    };
    let Some(dissolution) = projection.latest_authorized_event(EventKind::ChannelDissolve, None)?
    else {
        return Ok(Vec::new());
    };
    let Some(original) =
        node.channel_checkpoints
            .causal_projection(log.repository()?, group, dissolution)?
    else {
        return Ok(Vec::new());
    };
    let mut sources = BTreeMap::new();
    for proof in proofs.current.values().chain(std::iter::once(own_proof)) {
        if !proofs.binding_current(proof) {
            continue;
        }
        if let Some(mut source) =
            Node::public_account_group_source(group, &original, &proof.public(), proof).filter(
                |source| {
                    matches!(
                        source.role,
                        AccountGroupRole::Owner | AccountGroupRole::Member
                    )
                },
            )
        {
            source.role = AccountGroupRole::Dissolved;
            source.authority.epoch = state.epoch();
            sources.insert(source.device.user_id(), source);
        }
    }
    Ok(sources.into_values().collect())
}

/// Recover a terminal modern account observation from validated public grant and
/// revocation provenance, even when this device never published a private head.
/// All identity inputs are the already captured pre-log snapshot.
fn revoked_account_source(
    group: ConversationId,
    log: &PersistentEventLog,
    projection: &crate::node::channel::ChannelMembershipProjection,
    own: &crate::identity::device::PublicIdentity,
    proof: &crate::discovery::Announce,
    proofs: &GroupProofSnapshot,
) -> Result<Option<(AccountGroupSource, EventId)>, crate::eventlog::LogError> {
    use crate::node::channel_account_authority::{AccountAuthorityControl, AccountGrant};
    type RevokedGrantCandidate = ((u64, EventId, [u8; 16]), AccountGrant);
    let Some(state) = projection.state.as_ref() else {
        return Ok(None);
    };
    if projection.authority.current_event().is_none() {
        return Ok(None);
    }
    if state.id() != group
        || proof.public() != *own
        || !proofs.binding_current(proof)
        || projection
            .authority
            .account_grant(&proofs.account_public)
            .is_some()
    {
        return Ok(None);
    }
    let mut revocation: Option<crate::eventlog::Event> = None;
    let mut previous: Option<RevokedGrantCandidate> = None;
    let mut after = None;
    loop {
        let page = projection
            .authority
            .revocation_page(&proofs.account_public, after.as_deref())?;
        for (id, revoked_at) in page.grants {
            if let Some(event) = log.get(&revoked_at)?
                && event.conversation_id == group
                && projection.is_authorized(event.id)?
                && revocation
                    .as_ref()
                    .is_none_or(|old| (event.lamport, event.id) > (old.lamport, old.id))
            {
                revocation = Some(event);
            }
            // A grant incarnation is immutable. Its exact original control
            // retains the same binding without materializing other grants.
            let Some(origin) = projection.authority.try_grant_origin(id)? else {
                continue;
            };
            let Some(event) = log.get(&origin)? else {
                continue;
            };
            if event.conversation_id != group
                || event.kind != EventKind::ChannelAccountAuthority
                || !projection.is_authorized(event.id)?
            {
                continue;
            }
            let Some(control) = AccountAuthorityControl::decode(&event.ciphertext) else {
                continue;
            };
            let Some(grant) = control
                .grants
                .into_iter()
                .find(|grant| grant.id() == Some(id))
            else {
                continue;
            };
            let Some(membership) = log.get(&grant.membership_event)? else {
                continue;
            };
            if membership.conversation_id != group
                || membership.kind != EventKind::MembershipChange
                || !projection.is_authorized(membership.id)?
            {
                continue;
            }
            if let Some(accepted) = grant.acceptance_event
                && (!projection.is_authorized(accepted)? || !log.has(&accepted)?)
            {
                continue;
            }
            let key = (membership.lamport, membership.id, id.incarnation);
            if previous.as_ref().is_none_or(|(old, _)| key > *old) {
                previous = Some((key, grant));
            }
        }
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let Some(revocation) = revocation else {
        return Ok(None);
    };
    let Some((_, grant)) = previous else {
        return Ok(None);
    };
    Ok(Some((
        AccountGroupSource {
            device: own.clone(),
            role: if state.is_dissolved() {
                AccountGroupRole::Dissolved
            } else {
                AccountGroupRole::Left
            },
            lifetime: MembershipLifetime {
                member: grant.original.public(),
                membership: grant.membership_event,
                acceptance: grant.acceptance_event,
            },
            authority: AuthorityExpectation {
                authority: projection.authority.current_event(),
                epoch: state.epoch(),
            },
            execution: AccountGroupExecution::Local,
        },
        revocation.id,
    )))
}

impl Node {
    fn account_group_proof_snapshot(&self) -> GroupProofSnapshot {
        let (memory, live) = {
            let roster = self.roster.lock().expect("roster mutex");
            let memory = roster.historical_announcements();
            let live: BTreeMap<_, _> = memory
                .iter()
                .filter_map(|proof| {
                    roster
                        .announcement(&proof.public())
                        .cloned()
                        .map(|current| (proof.ed25519_pub, current))
                })
                .collect();
            (memory, live)
        };
        let guard = self.privacy.state.read().expect("privacy state mutex");
        let current: BTreeMap<_, _> = guard
            .as_ref()
            .map(|state| {
                state
                    .proofs
                    .announcements()
                    .into_iter()
                    .map(|proof| (proof.ed25519_pub, proof))
                    .collect()
            })
            .unwrap_or_default();
        let historical = if let Some(state) = guard.as_ref() {
            let mut proofs: Vec<_> = current.values().cloned().collect();
            if !state.policy.snapshot().invisible {
                proofs.extend(memory.into_iter().filter(|proof| {
                    proof.account_id().is_none() && !current.contains_key(&proof.ed25519_pub)
                }));
            }
            proofs
        } else {
            memory
        };
        // Preserve historical_author's privacy eligibility, using this same
        // directory/policy snapshot rather than reacquiring it under the log.
        let owners = historical
            .iter()
            .filter(|proof| {
                guard.as_ref().is_none_or(|state| {
                    !state.policy.snapshot().invisible
                        || state
                            .proofs
                            .account_for(&proof.public())
                            .or_else(|| {
                                live.get(&proof.ed25519_pub)
                                    .filter(|announce| {
                                        announce.public() == proof.public() && announce.verify()
                                    })
                                    .and_then(crate::discovery::Announce::account_id)
                            })
                            .is_some_and(|account| {
                                account == self.account_id() || state.policy.allows(&account)
                            })
                })
            })
            .cloned()
            .collect();
        GroupProofSnapshot {
            account_public: self.account.public().ed25519_pub,
            current,
            historical,
            owners,
        }
    }
    /// Freeze only canonical physical legacy members with verified current
    /// bindings. A linked member with missing proof is unresolved, never absent.
    pub(in crate::node) fn freeze_legacy_leave_sources(
        &self,
        group: ConversationId,
        pinned: &AccountGroupSource,
    ) -> Result<Vec<AccountGroupSource>, NodeError> {
        let proofs = self.account_group_proof_snapshot();
        let linked = self.linked_devices();
        let own = self.own_group_proof();
        let heads = self
            .group_control_heads(group)
            .into_iter()
            .filter(|head| proofs.binding_current(&head.proof))
            .collect::<Vec<_>>();
        let mut candidates = proofs.historical.clone();
        candidates.extend(proofs.current.values().cloned());
        candidates.push(own);
        let log = self.log.lock().expect("log mutex");
        let projection = self.account_group_projection_with_log(&log, group)?;
        let state = projection
            .state
            .as_ref()
            .ok_or_else(|| NodeError::Channel("legacy departure ancestry unavailable".into()))?;
        if state.is_dissolved() || projection.authority.current_event().is_some() {
            return Err(NodeError::Authorization(
                "legacy departure format changed".into(),
            ));
        }
        let (_, stamp) = authorization_view(self, &log, group)?;
        let mut missing = false;
        for head in &heads {
            if !view_contains(&log, group, stamp, &head.access)? {
                missing = true;
                break;
            }
        }
        if missing {
            return Err(NodeError::Channel(
                "legacy physical member ancestry is reconciling".into(),
            ));
        }
        let mut sources = Vec::new();
        for member in state.members() {
            let proof = candidates
                .iter()
                .find(|proof| proof.public() == *member && proofs.binding_current(proof));
            let known_own = *member == self.identity.public()
                || linked.iter().any(|device| {
                    device.public == *member
                        && device.certificate.verify()
                        && device.certificate.account_ed25519_pub
                            == self.account.public().ed25519_pub
                });
            if proof.is_none() && !known_own {
                continue;
            }
            let proof = proof
                .ok_or_else(|| NodeError::Channel("legacy member binding is reconciling".into()))?;
            let source = Self::public_account_group_source(group, &projection, member, proof)
                .ok_or_else(|| {
                    NodeError::Channel("legacy member lifetime is reconciling".into())
                })?;
            if source.role != AccountGroupRole::Member {
                return Err(NodeError::Authorization(
                    "legacy account owner or invitation cannot leave".into(),
                ));
            }
            sources.push(source);
        }
        let actual = sources
            .iter()
            .find(|source| source.device == pinned.device)
            .ok_or_else(|| {
                NodeError::Authorization("legacy coordinator membership retired".into())
            })?;
        let mut normalized = actual.clone();
        if normalized.authority.authority.is_some()
            || pinned.authority.authority.is_some()
            || normalized.authority.epoch < pinned.authority.epoch
        {
            return Err(NodeError::Authorization(
                "legacy coordinator authority changed".into(),
            ));
        }
        normalized.authority.epoch = pinned.authority.epoch;
        normalized.execution = pinned.execution.clone();
        if normalized != *pinned || sources.is_empty() || sources.len() > 65 {
            return Err(NodeError::Authorization(
                "legacy coordinator lifetime changed".into(),
            ));
        }
        sources.sort_by_key(|source| source.device.user_id());
        Ok(sources)
    }

    pub fn account_group_access(
        &self,
        group: ConversationId,
    ) -> Result<AccountGroupAccess, NodeError> {
        let mut access = self.account_group_access_with_closed(group)?;
        // Departed sources are not executable sources in the public role DTO.
        if matches!(
            access.role,
            AccountGroupRole::Left | AccountGroupRole::Dissolved
        ) {
            access.sources.clear();
        }
        Ok(access)
    }
    pub(in crate::node) fn account_group_access_with_closed(
        &self,
        group: ConversationId,
    ) -> Result<AccountGroupAccess, NodeError> {
        let mut access = AccountGroupAccess {
            group,
            account_id: self.account_id(),
            role: AccountGroupRole::Unknown,
            execution: AccountGroupExecution::Reconciling,
            sources: Vec::new(),
        };
        let proofs = self.account_group_proof_snapshot();
        let heads: Vec<_> = self
            .group_control_heads(group)
            .into_iter()
            .filter(|head| proofs.binding_current(&head.proof))
            .collect();
        if heads.len() > MAX_OBSERVED_SOURCES || self.group_control_has_unreadable_head(group) {
            return Ok(access);
        }
        let own = self.identity.public();
        let own_proof = self.own_group_proof();
        let own_head = heads.iter().find(|head| head.proof.public() == own);
        let (local, projection, known) = {
            let log = self.log.lock().expect("log mutex");
            let projection = self.account_group_projection_with_log(&log, group)?;
            if projection
                .state
                .as_ref()
                .is_some_and(|state| state.is_dissolved())
            {
                access.role = AccountGroupRole::Dissolved;
                access.sources =
                    dissolved_account_sources(self, group, &log, &projection, &proofs, &own_proof)?;
                return Ok(access);
            }
            let (_, stamp) = authorization_view(self, &log, group)?;
            let mut known = Vec::with_capacity(heads.len());
            for head in &heads {
                known.push(
                    (view_contains(&log, group, stamp, &head.access)?
                        || head.proof.public() == own
                            && local_receipt_contains(&log, group, &head.access)?)
                        && evaluated(&log, group, &head.access)?,
                );
            }
            if heads.iter().zip(&known).any(|(head, known)| {
                head.proof.public() == own && !head.access.complete_frontier && !*known
            }) {
                // A missing or corrupt own receipt is recovery uncertainty, not a
                // license to restore that device's cached positive permission.
                return Ok(access);
            }
            let local = if known.iter().any(|known| !*known) {
                None
            } else {
                self.account_group_snapshot(
                    group,
                    &log,
                    &projection,
                    own_head.map(|head| &head.access),
                    &heads,
                    &proofs,
                )?
            };
            (local, projection, known)
        };
        let mut candidates: Vec<(&SourceAccess, Option<HeadReference>, bool, bool)> = heads
            .iter()
            .zip(&known)
            .filter(|(head, _)| local.is_none() || head.proof.public() != own)
            .map(|(head, known)| (&head.access, Some(head.reference()), false, *known))
            .collect();
        if let Some(local) = &local {
            candidates.push((local, None, true, true));
        }
        if candidates.iter().any(|(candidate, _, _, _)| {
            !candidate.unavailable && candidate.observation.role == AccountGroupRole::Dissolved
        }) {
            access.role = AccountGroupRole::Dissolved;
            return Ok(access);
        }
        let maximal: Vec<_> = candidates
            .iter()
            .filter(|(_, reference, _, _)| {
                reference.as_ref().is_none_or(|reference| {
                    !candidates
                        .iter()
                        .any(|(candidate, _, _, _)| candidate.acknowledgements.contains(reference))
                })
            })
            .collect();
        if maximal.is_empty() {
            return Ok(access);
        }
        if maximal
            .iter()
            .any(|(candidate, _, _, _)| candidate.unavailable)
            || maximal.iter().any(|(left, _, _, _)| {
                maximal
                    .iter()
                    .any(|(right, _, _, _)| !left.compatible(right))
            })
        {
            return Ok(access);
        }
        let mut sources = BTreeMap::new();
        let mut closed_sources = BTreeMap::new();
        let mut left = false;
        for (candidate, _, is_local, known_public) in maximal {
            left |= candidate.observation.role == AccountGroupRole::Left;
            if candidate.observation.role == AccountGroupRole::Left
                && candidate.source.role == AccountGroupRole::Left
            {
                closed_sources.insert(candidate.source.device.user_id(), candidate.source.clone());
            }
            for bound in &candidate.observation.sources {
                if !self.account_group_binding_current(&bound.proof) {
                    continue;
                }
                if heads.iter().any(|head| {
                    head.access.source.device == bound.source.device
                        && head.access.source.role == AccountGroupRole::Left
                        && head.access.source.lifetime == bound.source.lifetime
                        && head.access.source.authority.authority
                            == bound.source.authority.authority
                }) {
                    continue;
                }
                let mut source = if *known_public {
                    let Some(current) = Self::public_account_group_source(
                        group,
                        &projection,
                        &bound.source.device,
                        &bound.proof,
                    ) else {
                        left = true;
                        continue;
                    };
                    current
                } else {
                    bound.source.clone()
                };
                source.execution = if source.device == own {
                    if *is_local {
                        AccountGroupExecution::Local
                    } else {
                        AccountGroupExecution::Reconciling
                    }
                } else {
                    self.account_group_source_execution(&source.device)
                };
                sources.insert(source.device.user_id(), source);
            }
        }
        access.sources = sources.into_values().collect();
        if let Some(source) = access
            .sources
            .iter()
            .max_by_key(|source| (rank(source.role), source.device == own))
        {
            access.role = source.role;
            access.execution = source.execution.clone();
        } else if left {
            access.role = AccountGroupRole::Left;
            access.sources = closed_sources.into_values().collect();
        }
        Ok(access)
    }
    fn account_group_source_execution(&self, device: &PublicIdentity) -> AccountGroupExecution {
        if !self
            .roster
            .lock()
            .expect("roster mutex")
            .get(&device.user_id())
            .is_some_and(|peer| peer.public == *device)
        {
            return AccountGroupExecution::WaitingForDevice;
        }
        if self
            .account_group_versions
            .lock()
            .expect("group versions mutex")
            .get(&device.user_id())
            .is_some_and(|support| support.public == *device)
        {
            AccountGroupExecution::ViaDevice(device.clone())
        } else {
            AccountGroupExecution::Reconciling
        }
    }
    pub(in crate::node) fn public_account_group_source(
        _group: ConversationId,
        projection: &crate::node::channel::ChannelMembershipProjection,
        device: &PublicIdentity,
        proof: &crate::discovery::Announce,
    ) -> Option<AccountGroupSource> {
        let state = projection.state.as_ref()?;
        if state.is_dissolved() {
            return None;
        }
        let (lifetime, role) = if projection.authority.current_event().is_some() {
            let accepted = projection.authority.grant_for_device(device, proof);
            let invited = projection.authority.invitation_for_device(device, proof);
            let grant = accepted.or(invited)?;
            (
                MembershipLifetime {
                    member: grant.original.public(),
                    membership: grant.membership_event,
                    acceptance: grant.acceptance_event,
                },
                if invited.is_some() {
                    AccountGroupRole::Invited
                } else if projection.authority.can_admin(device, proof) {
                    AccountGroupRole::Owner
                } else {
                    AccountGroupRole::Member
                },
            )
        } else {
            if !state.members().contains(device) {
                return None;
            }
            let accepted = projection.accepted.iter().find(|member| {
                member.public == *device && state.is_accepted_member(&device.user_id())
            });
            let invited = projection.invited.iter().find(|member| {
                member.public == *device && !state.is_accepted_member(&device.user_id())
            });
            let member = accepted.or(invited)?;
            (
                MembershipLifetime {
                    member: member.public.clone(),
                    membership: member.membership_event,
                    acceptance: member.acceptance_event,
                },
                if invited.is_some() {
                    AccountGroupRole::Invited
                } else if state.owner() == device.user_id() {
                    AccountGroupRole::Owner
                } else {
                    AccountGroupRole::Member
                },
            )
        };
        Some(AccountGroupSource {
            device: device.clone(),
            role,
            lifetime,
            authority: AuthorityExpectation {
                authority: projection.authority.current_event(),
                epoch: state.epoch(),
            },
            execution: AccountGroupExecution::Local,
        })
    }

    pub(in crate::node) fn account_group_binding_current(
        &self,
        proof: &crate::discovery::Announce,
    ) -> bool {
        if !proof.verify()
            || proof.account_cert.as_ref().is_none_or(|certificate| {
                certificate.account_ed25519_pub != self.account.public().ed25519_pub
            })
        {
            return false;
        }
        let state = self.privacy.state.read().expect("privacy state mutex");
        state.as_ref().is_none_or(|state| {
            state
                .proofs
                .by_author(&proof.ed25519_pub)
                .is_none_or(|current| {
                    current.public() == proof.public() && current.account_id() == proof.account_id()
                })
        })
    }

    pub(in crate::node) fn account_group_observation_known(
        &self,
        group: ConversationId,
        head: &GroupControlHead,
    ) -> Result<bool, NodeError> {
        let log = self.log.lock().expect("log mutex");
        let (_, stamp) = authorization_view(self, &log, group)?;
        Ok(view_contains(&log, group, stamp, &head.access)?
            || head.proof.public() == self.identity.public()
                && local_receipt_contains(&log, group, &head.access)?)
    }
    pub(in crate::node) fn account_group_local_observation(
        &self,
        group: ConversationId,
        prior: Option<&SourceAccess>,
    ) -> Result<Option<SourceAccess>, NodeError> {
        let proofs = self.account_group_proof_snapshot();
        let heads: Vec<_> = self
            .group_control_heads(group)
            .into_iter()
            .filter(|head| proofs.binding_current(&head.proof))
            .collect();
        let log = self.log.lock().expect("log mutex");
        let projection = self.account_group_projection_with_log(&log, group)?;
        self.account_group_snapshot(group, &log, &projection, prior, &heads, &proofs)
    }
    fn account_group_snapshot(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &crate::node::channel::ChannelMembershipProjection,
        prior: Option<&SourceAccess>,
        heads: &[GroupControlHead],
        proofs: &GroupProofSnapshot,
    ) -> Result<Option<SourceAccess>, NodeError> {
        let own = self.own_group_proof();
        let basis = projection
            .authority
            .try_legacy_closing_basis(&own.public(), &own)?;
        let initial_owner = basis
            .iter()
            .map(|id| log.get(id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .find_map(|event| {
                (event.kind == EventKind::ChannelAccountAuthority)
                    .then(|| {
                        super::super::channel_account_authority::AccountAuthorityControl::decode(
                            &event.ciphertext,
                        )
                    })
                    .flatten()
            })
            .map(|control| control.owner_account);
        if !basis.is_empty()
            && initial_owner != Some(proofs.account_public)
            && all_present(log, group, basis.iter().copied())?
            && let Some(closed) = self.channel_checkpoints.causal_projection_basis(
                log.repository()?,
                group,
                &basis,
            )?
        {
            return self.account_group_snapshot_inner(
                group,
                log,
                &closed,
                None,
                heads,
                proofs,
                Some(&closed),
            );
        }
        self.account_group_snapshot_inner(group, log, projection, prior, heads, proofs, None)
    }

    fn account_group_snapshot_inner(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &crate::node::channel::ChannelMembershipProjection,
        prior: Option<&SourceAccess>,
        heads: &[GroupControlHead],
        proofs: &GroupProofSnapshot,
        scope: Option<&crate::node::channel::ChannelMembershipProjection>,
    ) -> Result<Option<SourceAccess>, NodeError> {
        let Some(state) = projection.state.as_ref() else {
            return Ok(None);
        };
        let own = self.identity.public();
        let own_proof = self.own_group_proof();
        let retired = projection
            .authority
            .try_legacy_retired_source(&own, &own_proof)?;
        let revoked =
            match revoked_account_source(group, log, projection, &own, &own_proof, proofs)? {
                Some(value) => Some(value),
                None => {
                    departed_legacy_source(self, group, log, projection, &own, &own_proof, proofs)?
                }
            }
            .or_else(|| {
                if !proofs.binding_current(&own_proof) {
                    return None;
                }
                let (_, original, departure) = retired?;
                Some((
                    AccountGroupSource {
                        device: own.clone(),
                        role: if state.is_dissolved() {
                            AccountGroupRole::Dissolved
                        } else {
                            AccountGroupRole::Left
                        },
                        lifetime: MembershipLifetime {
                            member: original.public,
                            membership: original.membership_event,
                            acceptance: original.acceptance_event,
                        },
                        authority: AuthorityExpectation {
                            authority: projection.authority.current_event(),
                            epoch: projection.authority.key_epoch()?,
                        },
                        execution: AccountGroupExecution::Local,
                    },
                    departure,
                ))
            });
        let source = Self::public_account_group_source(group, projection, &own, &own_proof)
            .or_else(|| revoked.as_ref().map(|(source, _)| source.clone()))
            .or_else(|| {
                let mut source = prior?.source.clone();
                source.role = if state.is_dissolved() {
                    AccountGroupRole::Dissolved
                } else {
                    AccountGroupRole::Left
                };
                source.authority = AuthorityExpectation {
                    authority: projection.authority.current_event(),
                    epoch: state.epoch(),
                };
                Some(source)
            });
        let Some(source) = source else {
            return Ok(None);
        };
        let owner = state
            .members()
            .iter()
            .find(|device| device.user_id() == state.owner())
            .cloned();
        let owner_proof = owner.as_ref().and_then(|device| {
            if *device == own {
                return Some(own_proof.clone());
            }
            proofs
                .owners
                .iter()
                .find(|proof| proof.public() == *device)
                .cloned()
                .or_else(|| {
                    heads
                        .iter()
                        .flat_map(|head| {
                            std::iter::once(&head.proof).chain(head.access.owner_proof.iter())
                        })
                        .find(|proof| proof.public() == *device && proofs.binding_current(proof))
                        .cloned()
                })
        });
        let mut candidate_proofs = proofs.historical.clone();
        candidate_proofs.push(own_proof.clone());
        candidate_proofs.extend(projection.authority.original_device_proofs());
        candidate_proofs.extend(heads.iter().flat_map(|head| {
            std::iter::once(head.proof.clone()).chain(
                head.access
                    .observation
                    .sources
                    .iter()
                    .map(|bound| bound.proof.clone()),
            )
        }));
        candidate_proofs.extend(owner_proof.clone());
        let mut members = BTreeMap::new();
        for device in state.members() {
            let proof = candidate_proofs
                .iter()
                .find(|proof| proof.public() == *device)
                .cloned()
                .or_else(|| {
                    projection
                        .authority
                        .original_device_proofs()
                        .into_iter()
                        .find(|proof| proof.public() == *device)
                });
            if let Some(source) = Self::public_account_group_source(
                group,
                projection,
                device,
                proof.as_ref().unwrap_or(&self.own_group_proof()),
            ) {
                members.insert(source.device.user_id(), source);
            }
        }
        let members: Vec<_> = members.into_values().collect();
        let mut sources = BTreeMap::new();
        if !state.is_dissolved() {
            for proof in candidate_proofs {
                if !proofs.binding_current(&proof) {
                    continue;
                }
                if let Some(source) =
                    Self::public_account_group_source(group, projection, &proof.public(), &proof)
                {
                    sources.insert(source.device.user_id(), BoundSource { source, proof });
                }
            }
        }
        let sources: Vec<_> = sources.into_values().collect();
        let role = if state.is_dissolved() {
            AccountGroupRole::Dissolved
        } else {
            sources
                .iter()
                .map(|bound| bound.source.role)
                .max_by_key(|role| rank(*role))
                .unwrap_or(AccountGroupRole::Left)
        };
        let mut public_members: Vec<_> = state
            .members()
            .iter()
            .map(|device| {
                let root = projection
                    .accepted
                    .iter()
                    .chain(&projection.invited)
                    .find(|member| member.public == *device);
                (
                    device.clone(),
                    state.is_accepted_member(&device.user_id()),
                    root.map(|root| (root.membership_event, root.acceptance_event)),
                )
            })
            .collect();
        public_members.sort_by_key(|row| row.0.user_id());
        let public_state = Sha256::digest(
            serde_json::to_vec(&(
                state.owner(),
                state.epoch(),
                state.is_dissolved(),
                projection.ownership,
                projection.authority.current_event(),
                public_members,
            ))
            .expect("public role snapshot serializes"),
        )
        .into();
        let mut ids = vec![source.lifetime.membership];
        ids.extend(source.lifetime.acceptance);
        ids.extend(projection.ownership);
        if !state.is_dissolved() {
            ids.extend(revoked.as_ref().map(|(_, event)| *event));
            ids.extend(
                projection
                    .authority
                    .try_legacy_closing_basis(&own, &own_proof)?,
            );
        }
        if state.is_dissolved() {
            ids.extend(projection.latest_authorized_event(EventKind::ChannelDissolve, None)?);
        }
        ids.sort();
        ids.dedup();
        let controls = ids
            .iter()
            .map(|id| log.get(id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let (local_frontier, (fingerprint, count)) =
            authorization_view_at(self, log, group, scope)?;
        let complete_frontier = local_frontier.len() <= MAX_ACCESS_FRONTIER;
        let frontier = local_frontier
            .iter()
            .take(MAX_ACCESS_FRONTIER)
            .copied()
            .collect();
        let mut result = SourceAccess {
            source,
            owner_proof,
            controls,
            observation: AccountObservation {
                role,
                sources,
                members,
                owner,
                public_state,
            },
            acknowledgements: Vec::new(),
            unavailable: false,
            ownership: projection.ownership,
            frontier,
            complete_frontier,
            fingerprint,
            event_count: count as u64,
            local_frontier,
        };
        let mut acknowledgements: BTreeMap<_, _> = prior
            .into_iter()
            .flat_map(|prior| prior.acknowledgements.iter())
            .map(|reference| (reference.device.user_id(), reference.clone()))
            .collect();
        for head in heads {
            if head.proof.public() != own
                && scope_contains(scope, &head.access.frontier)?
                && !result.compatible(&head.access)
                && view_contains(log, group, (fingerprint, count), &head.access)?
                && evaluated(log, group, &head.access)?
            {
                acknowledgements.insert(head.proof.user_id.clone(), head.reference());
            }
        }
        result.acknowledgements = acknowledgements.into_values().collect();
        if result.observation.sources.len() > MAX_OBSERVED_SOURCES
            || result.observation.members.len() > super::wire::MAX_OBSERVED_MEMBERS
            || result.acknowledgements.len() > MAX_OBSERVED_SOURCES
        {
            result.make_unavailable();
        }
        Ok(Some(result))
    }
}

#[cfg(test)]
mod raw_view_tests {
    use super::*;
    use crate::{discovery::Roster, eventlog::Event, identity::device::DeviceIdentity};
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn raw_authorization_cache_rechecks_noncontrol_reordering() {
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join("events.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let group = crate::channel::new_channel_id();
        let first = Event::new(
            &node.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::ChannelLeave,
            vec![],
        );
        let missing = Event::new(
            &node.identity,
            group,
            2,
            vec![first.id],
            2,
            2,
            EventKind::Message,
            vec![8; 2048],
        );
        let last = Event::new(
            &node.identity,
            group,
            3,
            vec![],
            3,
            3,
            EventKind::ChannelLeave,
            vec![],
        );
        let mut log = node.log.lock().unwrap();
        log.append(first.clone()).unwrap();
        log.append(last.clone()).unwrap();
        let (mut frontier, stamp) = authorization_view(&node, &log, group).unwrap();
        let mut expected = vec![first.id, last.id];
        expected.sort();
        frontier.sort();
        assert_eq!(frontier, expected);
        let cold = raw_view::cold_reads();
        let bodies = log.repository().unwrap().body_read_counts();
        log.append(missing).unwrap();
        let (frontier, updated_stamp) = authorization_view(&node, &log, group).unwrap();
        let mut frontier = frontier;
        frontier.sort();
        assert_eq!(
            frontier, expected,
            "a late ordinary branch preserves these independent controls"
        );
        assert_eq!(
            updated_stamp, stamp,
            "the exact signed control set is unchanged"
        );
        assert_eq!(
            raw_view::cold_reads(),
            cold + 1,
            "an invalidated causal generation cannot reuse its frontier"
        );
        assert_eq!(log.repository().unwrap().body_read_counts(), bodies);
    }

    #[tokio::test]
    async fn raw_authorization_view_does_not_decode_retained_message_history() {
        for count in [128_u64, 512] {
            let dir = tempfile::tempdir().unwrap();
            let (dm, _) = tokio::sync::mpsc::unbounded_channel();
            let (channel, _) = tokio::sync::mpsc::unbounded_channel();
            let (file, _) = tokio::sync::mpsc::unbounded_channel();
            let node = Node::open(
                DeviceIdentity::generate(),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &dir.path().join("events.log"),
                &dir.path().join("sent.log"),
                "pw",
            )
            .unwrap();
            let group = crate::channel::new_channel_id();
            let mut log = node.log.lock().unwrap();
            let mut parent = Vec::new();
            let mut controls = Vec::new();
            for sequence in 1..=count {
                let control = sequence % 31 == 0;
                let event = Event::new(
                    &node.identity,
                    group,
                    sequence,
                    parent,
                    sequence,
                    sequence,
                    if control {
                        EventKind::ChannelLeave
                    } else {
                        EventKind::Message
                    },
                    if control { Vec::new() } else { vec![7; 2048] },
                );
                parent = vec![event.id];
                if control {
                    controls.push(event.id);
                }
                log.append(event).unwrap();
            }
            // Rejected, concurrent controls still belong to the raw view.
            let rejected = Event::new(
                &node.identity,
                group,
                count + 1,
                vec![],
                count + 1,
                count + 1,
                EventKind::ChannelLeave,
                vec![1],
            );
            let last = *controls.last().unwrap();
            controls.push(rejected.id);
            log.append(rejected.clone()).unwrap();
            let before = log.repository().unwrap().body_read_counts();
            let (mut frontier, (stamp, actual_count)) =
                authorization_view(&node, &log, group).unwrap();
            assert_eq!(
                log.repository().unwrap().body_read_counts(),
                before,
                "raw authorization metadata must not decode {count} retained messages"
            );
            controls.sort();
            let mut digest = Sha256::new();
            digest.update(b"mesh-talk-group-authorization-view-v1");
            for id in &controls {
                digest.update(id.as_bytes());
            }
            assert_eq!(stamp, <[u8; 32]>::from(digest.finalize()));
            assert_eq!(actual_count, controls.len());
            frontier.sort();
            let mut expected = vec![last, rejected.id];
            expected.sort();
            assert_eq!(frontier, expected);
            let scope = node
                .channel_checkpoints
                .causal_projection(log.repository().unwrap(), group, parent[0])
                .unwrap()
                .unwrap();
            let before = log.repository().unwrap().body_read_counts();
            let (scoped_frontier, (scoped_stamp, scoped_count)) =
                authorization_view_at(&node, &log, group, Some(&scope)).unwrap();
            assert_eq!(log.repository().unwrap().body_read_counts(), before);
            assert_eq!(scoped_frontier, vec![last]);
            let mut digest = Sha256::new();
            digest.update(b"mesh-talk-group-authorization-view-v1");
            for id in controls.iter().filter(|id| **id != rejected.id) {
                digest.update(id.as_bytes());
            }
            assert_eq!(scoped_stamp, <[u8; 32]>::from(digest.finalize()));
            assert_eq!(scoped_count, controls.len() - 1);
            let before = raw_view::cold_reads();
            let tail = Event::new(
                &node.identity,
                group,
                count + 2,
                parent,
                count + 2,
                count + 2,
                EventKind::Message,
                vec![4; 2048],
            );
            log.append(tail).unwrap();
            let observation = authorization_view(&node, &log, group).unwrap();
            assert_eq!(observation.1, (stamp, actual_count));
            assert_eq!(
                raw_view::cold_reads(),
                before,
                "an ordinary safe message tail must not rescan the raw control graph"
            );
        }
    }
    fn directory_test_node() -> (tempfile::TempDir, Arc<Node>) {
        let directory = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &directory.path().join("events"),
            &directory.path().join("sent"),
            "pw",
        )
        .unwrap();
        (directory, node)
    }
    fn finish_directory(
        node: &Node,
        log: &PersistentEventLog,
        group: ConversationId,
    ) -> DirectoryView {
        for _ in 0..4096 {
            let repository = log.repository().unwrap();
            let before = repository.ancestry_vertex_reads();
            let outcome = node.directory_observation(log, group);
            assert!(
                repository.ancestry_vertex_reads() - before <= 128,
                "one directory turn drained the ancestry graph"
            );
            match outcome {
                Ok(view) => return view,
                Err(crate::eventlog::LogError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(crate::eventlog::LogError::StaleCursor) => {}
                Err(error) => panic!("directory preparation failed: {error}"),
            }
        }
        panic!("bounded directory preparation failed to complete")
    }
    #[test]
    fn directory_selector_preserves_hex_digest_raw_rejections_and_authorization_cache() {
        let (_directory, node) = directory_test_node();
        let group = crate::channel::new_channel_id();
        let mut log = node.log.lock().unwrap();
        let mut parent = Vec::new();
        let mut selected = Vec::new();
        for sequence in 1..=601 {
            let kind = match sequence {
                1 => EventKind::ChannelLeave,
                301 => EventKind::ChannelRename,
                601 => EventKind::ChannelHistoryPolicy,
                _ => EventKind::Message,
            };
            let event = Event::new(
                &node.identity,
                group,
                sequence,
                parent,
                sequence,
                sequence,
                kind,
                vec![7; 2048],
            );
            parent = vec![event.id];
            if sequence == 1 || sequence == 301 || sequence == 601 {
                selected.push(event.id);
            }
            log.append(event).unwrap();
        }
        let authorization = authorization_view(&node, &log, group).unwrap();
        let before = log.repository().unwrap().body_read_counts();
        assert!(matches!(
            node.directory_observation(&log, group),
            Err(crate::eventlog::LogError::Io(_))
        ));
        // A changed source must discard its partial digest and include the raw
        // rejected control, even though canonical membership will ignore it.
        let rejected = Event::new(
            &node.identity,
            group,
            602,
            vec![selected[0]],
            602,
            602,
            EventKind::ChannelRename,
            vec![255],
        );
        log.append(rejected.clone()).unwrap();
        selected.push(rejected.id);
        let view = finish_directory(&node, &log, group);
        let mut expected = Sha256::new();
        expected.update(b"mesh-talk-group-directory-v1");
        selected.sort();
        for event in &selected {
            expected.update(event.to_hex().as_bytes());
        }
        assert_eq!(view.fingerprint, <[u8; 32]>::from(expected.finalize()));
        assert_eq!(view.count, 4);
        assert_eq!(
            view.controls,
            selected.iter().map(EventId::to_hex).collect::<Vec<_>>()
        );
        let mut frontier = vec![parent[0], rejected.id];
        frontier.sort();
        assert_eq!(view.frontier, frontier);
        assert_eq!(
            authorization_view(&node, &log, group).unwrap(),
            authorization,
            "directory observations contaminated the authorization cache"
        );
        assert_eq!(log.repository().unwrap().body_read_counts(), before);
        let before = log.repository().unwrap().ancestry_vertex_reads();
        assert_eq!(
            node.directory_observation(&log, group).unwrap().fingerprint,
            view.fingerprint
        );
        assert_eq!(
            log.repository().unwrap().ancestry_vertex_reads(),
            before,
            "warm view walked history"
        );
    }
    #[test]
    fn directory_preparation_capacity_advances_abandoned_calls_without_eviction() {
        let (_directory, node) = directory_test_node();
        let mut log = node.log.lock().unwrap();
        let mut groups = Vec::new();
        for sequence in 1..=65 {
            let group = crate::channel::new_channel_id();
            log.append(Event::new(
                &node.identity,
                group,
                sequence,
                vec![],
                sequence,
                sequence,
                EventKind::ChannelRename,
                vec![1],
            ))
            .unwrap();
            groups.push(group);
        }
        for group in groups.iter().take(64) {
            assert!(matches!(
                node.directory_observation(&log, *group),
                Err(crate::eventlog::LogError::Io(_))
            ));
        }
        let view = finish_directory(&node, &log, groups[64]);
        assert_eq!(
            view.count, 1,
            "new requests were starved by abandoned preparations"
        );
        assert_eq!(view.frontier.len(), 1);
        for group in groups.iter().take(64) {
            assert_eq!(finish_directory(&node, &log, *group).count, 1);
        }
    }
}
