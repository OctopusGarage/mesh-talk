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
use std::collections::{BTreeMap, HashSet};

pub(super) fn rank(role: AccountGroupRole) -> u8 {
    match role {
        AccountGroupRole::Owner => 3,
        AccountGroupRole::Member => 2,
        AccountGroupRole::Invited => 1,
        _ => 0,
    }
}
fn view_contains(
    log: &PersistentEventLog,
    group: ConversationId,
    stamp: ([u8; 32], usize),
    access: &SourceAccess,
) -> bool {
    (access.fingerprint == stamp.0 && access.event_count == stamp.1 as u64)
        || access.complete_frontier
            && access.frontier.iter().all(|id| {
                log.get(id)
                    .is_some_and(|event| event.conversation_id == group)
            })
}
fn local_receipt_contains(
    log: &PersistentEventLog,
    group: ConversationId,
    access: &SourceAccess,
) -> bool {
    !access.local_frontier.is_empty()
        && access.local_frontier.iter().all(|id| {
            log.get(id)
                .is_some_and(|event| event.conversation_id == group)
        })
}
fn evaluated(log: &PersistentEventLog, group: ConversationId, access: &SourceAccess) -> bool {
    let present = |id: EventId| {
        log.get(&id)
            .is_some_and(|event| event.conversation_id == group)
    };
    let considered = |source: &AccountGroupSource| {
        present(source.lifetime.membership)
            && source.lifetime.acceptance.is_none_or(present)
            && source.authority.authority.is_none_or(present)
    };
    // Rejected competing decisions were evaluated too. Missing decisions were not.
    considered(&access.source)
        && access.ownership.is_none_or(present)
        && access
            .observation
            .sources
            .iter()
            .all(|bound| considered(&bound.source))
}
fn authorization_view(
    log: &PersistentEventLog,
    group: ConversationId,
) -> (Vec<EventId>, ([u8; 32], usize)) {
    authorization_view_at(log, group, None)
}
fn authorization_view_at(
    log: &PersistentEventLog,
    group: ConversationId,
    scope: Option<&HashSet<EventId>>,
) -> (Vec<EventId>, ([u8; 32], usize)) {
    let relevant: HashSet<_> = log
        .events(&group)
        .into_iter()
        // The validated log can contain rejected competing controls which are
        // parents of a later applied decision. The reducer's authorized index
        // is not a complete record of its state transitions.
        .filter(|event| {
            scope.is_none_or(|scope| scope.contains(&event.id))
                && super::wire::authorization_control(event.kind)
        })
        .map(|event| event.id)
        .collect();
    let mut ids: Vec<_> = relevant.iter().copied().collect();
    ids.sort();
    let mut digest = Sha256::new();
    digest.update(b"mesh-talk-group-authorization-view-v1");
    for id in &ids {
        digest.update(id.as_bytes());
    }
    let stamp = (digest.finalize().into(), ids.len());
    let mut ancestors = HashSet::new();
    let mut visited = HashSet::new();
    let mut pending: Vec<_> = ids
        .iter()
        .filter_map(|id| log.get(id))
        .flat_map(|event| event.parents.iter().copied())
        .collect();
    while let Some(id) = pending.pop() {
        if relevant.contains(&id) {
            ancestors.insert(id);
        } else if visited.insert(id)
            && let Some(event) = log.get(&id)
        {
            pending.extend(event.parents.iter().copied());
        }
    }
    ids.retain(|id| !ancestors.contains(id));
    (ids, stamp)
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
    group: ConversationId,
    log: &PersistentEventLog,
    projection: &crate::node::channel::ChannelMembershipProjection,
    own: &crate::identity::device::PublicIdentity,
    proof: &crate::discovery::Announce,
    proofs: &GroupProofSnapshot,
) -> Option<(AccountGroupSource, EventId)> {
    let state = projection.state.as_ref()?;
    if (projection.authority.current_event().is_some()
        && projection
            .authority
            .omitted_legacy_departure(own, proof)
            .is_none())
        || state.members().contains(own)
        || !proofs.binding_current(proof)
        || proof.public() != *own
    {
        return None;
    }
    let departure = projection
        .departures
        .iter()
        .filter(|(device, _)| *device == own.user_id())
        .filter_map(|(_, id)| log.get(id))
        .filter(|event| {
            event.conversation_id == group
                && event.author.ed25519_pub() == &own.ed25519_pub
                && event.kind == EventKind::ChannelLeave
                && event.ciphertext.is_empty()
                && projection.authorized.contains(&event.id)
                && event.verify_signature()
                && event.verify_integrity()
        })
        .max_by_key(|event| (event.lamport, event.id))?;
    let mut pending = departure.parents.clone();
    let mut ancestors = BTreeMap::new();
    while let Some(id) = pending.pop() {
        if ancestors.contains_key(&id) {
            continue;
        }
        let parent = log.get(&id)?;
        if parent.conversation_id != group || parent.id == departure.id {
            return None;
        }
        pending.extend(parent.parents.iter().copied());
        ancestors.insert(id, parent);
    }
    let original = crate::node::channel::ChannelBook::project_membership(
        group,
        &ancestors.values().copied().collect::<Vec<_>>(),
    );
    let mut source = Node::public_account_group_source(group, &original, own, proof)?;
    if source.role != AccountGroupRole::Member
        || source.authority.authority.is_some()
        || source.lifetime.member != *own
    {
        return None;
    }
    source.role = AccountGroupRole::Left;
    source.authority.epoch = state.epoch();
    Some((source, departure.id))
}

/// Recover only terminal cleanup provenance from an authorized dissolution's
/// dependency-closed parent cut. This never grants current group authority.
fn dissolved_account_sources(
    group: ConversationId,
    log: &PersistentEventLog,
    projection: &crate::node::channel::ChannelMembershipProjection,
    proofs: &GroupProofSnapshot,
    own_proof: &crate::discovery::Announce,
) -> Vec<AccountGroupSource> {
    let Some(state) = projection
        .state
        .as_ref()
        .filter(|state| state.is_dissolved())
    else {
        return Vec::new();
    };
    let Some(dissolution) = log.events(&group).into_iter().find(|event| {
        event.kind == EventKind::ChannelDissolve && projection.authorized.contains(&event.id)
    }) else {
        return Vec::new();
    };
    let mut pending = dissolution.parents.clone();
    let mut ancestors = BTreeMap::new();
    while let Some(id) = pending.pop() {
        if ancestors.contains_key(&id) {
            continue;
        }
        let Some(event) = log.get(&id).filter(|event| event.conversation_id == group) else {
            return Vec::new();
        };
        pending.extend(event.parents.iter().copied());
        ancestors.insert(id, event);
    }
    let original = crate::node::channel::ChannelBook::project_membership(
        group,
        &ancestors.values().copied().collect::<Vec<_>>(),
    );
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
    sources.into_values().collect()
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
) -> Option<(AccountGroupSource, EventId)> {
    use crate::node::channel_account_authority::{AccountAuthorityControl, AccountGrant};
    type RevokedGrantCandidate = ((u64, EventId, [u8; 16]), AccountGrant);
    let state = projection.state.as_ref()?;
    projection.authority.current_event()?;
    if state.id() != group
        || proof.public() != *own
        || !proofs.binding_current(proof)
        || projection
            .authority
            .account_grant(&proofs.account_public)
            .is_some()
    {
        return None;
    }
    let revoked: HashSet<_> = projection
        .authority
        .revoked()
        .into_iter()
        .filter(|id| id.account_pub == proofs.account_public)
        .collect();
    let revocation = projection
        .authority
        .explicit_revocation_events(&proofs.account_public)
        .into_iter()
        .filter_map(|id| log.get(&id))
        .filter(|event| event.conversation_id == group && projection.authorized.contains(&event.id))
        .max_by_key(|event| (event.lamport, event.id))?;
    let mut previous: Option<RevokedGrantCandidate> = None;
    for event in log.events(&group) {
        if event.kind != EventKind::ChannelAccountAuthority
            || !projection.authorized.contains(&event.id)
        {
            continue;
        }
        let Some(control) = AccountAuthorityControl::decode(&event.ciphertext) else {
            continue;
        };
        for grant in control.grants {
            let Some(id) = grant.id().filter(|id| revoked.contains(id)) else {
                continue;
            };
            let Some(membership) = log.get(&grant.membership_event).filter(|event| {
                event.conversation_id == group
                    && event.kind == EventKind::MembershipChange
                    && projection.authorized.contains(&event.id)
            }) else {
                continue;
            };
            if grant
                .acceptance_event
                .is_some_and(|id| !projection.authorized.contains(&id) || log.get(&id).is_none())
            {
                continue;
            }
            let key = (membership.lamport, membership.id, id.incarnation);
            if previous.as_ref().is_none_or(|(old, _)| key > *old) {
                previous = Some((key, grant));
            }
        }
    }
    let (_, grant) = previous?;
    Some((
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
    ))
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
        let projection =
            crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
        let state = projection
            .state
            .as_ref()
            .ok_or_else(|| NodeError::Channel("legacy departure ancestry unavailable".into()))?;
        if state.is_dissolved() || projection.authority.current_event().is_some() {
            return Err(NodeError::Authorization(
                "legacy departure format changed".into(),
            ));
        }
        let (_, stamp) = authorization_view(&log, group);
        if heads
            .iter()
            .any(|head| !view_contains(&log, group, stamp, &head.access))
        {
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
            let projection =
                crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
            if projection
                .state
                .as_ref()
                .is_some_and(|state| state.is_dissolved())
            {
                access.role = AccountGroupRole::Dissolved;
                access.sources =
                    dissolved_account_sources(group, &log, &projection, &proofs, &own_proof);
                return Ok(access);
            }
            let (_, stamp) = authorization_view(&log, group);
            let known: Vec<_> = heads
                .iter()
                .map(|head| {
                    (view_contains(&log, group, stamp, &head.access)
                        || head.proof.public() == own
                            && local_receipt_contains(&log, group, &head.access))
                        && evaluated(&log, group, &head.access)
                })
                .collect();
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
                )
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
    ) -> bool {
        let log = self.log.lock().expect("log mutex");
        let (_, stamp) = authorization_view(&log, group);
        view_contains(&log, group, stamp, &head.access)
            || head.proof.public() == self.identity.public()
                && local_receipt_contains(&log, group, &head.access)
    }
    pub(in crate::node) fn account_group_local_observation(
        &self,
        group: ConversationId,
        prior: Option<&SourceAccess>,
    ) -> Option<SourceAccess> {
        let proofs = self.account_group_proof_snapshot();
        let heads: Vec<_> = self
            .group_control_heads(group)
            .into_iter()
            .filter(|head| proofs.binding_current(&head.proof))
            .collect();
        let log = self.log.lock().expect("log mutex");
        let projection =
            crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
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
    ) -> Option<SourceAccess> {
        let own = self.own_group_proof();
        let basis = projection
            .authority
            .legacy_closing_basis(&own.public(), &own);
        let initial_owner = basis
            .iter()
            .filter_map(|id| log.get(id))
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
        if !basis.is_empty() && initial_owner != Some(proofs.account_public) {
            let mut ancestors = HashSet::new();
            let mut pending = basis;
            let mut complete = true;
            while let Some(id) = pending.pop() {
                if !ancestors.insert(id) {
                    continue;
                }
                let Some(event) = log.get(&id).filter(|event| event.conversation_id == group)
                else {
                    complete = false;
                    break;
                };
                pending.extend(event.parents.iter().copied());
            }
            if complete {
                let events: Vec<_> = log
                    .events(&group)
                    .into_iter()
                    .filter(|event| ancestors.contains(&event.id))
                    .collect();
                let closed = crate::node::channel::ChannelBook::project_membership(group, &events);
                return self.account_group_snapshot_inner(
                    group,
                    log,
                    &closed,
                    None,
                    heads,
                    proofs,
                    Some(&ancestors),
                );
            }
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
        scope: Option<&HashSet<EventId>>,
    ) -> Option<SourceAccess> {
        let state = projection.state.as_ref()?;
        let own = self.identity.public();
        let own_proof = self.own_group_proof();
        let revoked = revoked_account_source(group, log, projection, &own, &own_proof, proofs)
            .or_else(|| departed_legacy_source(group, log, projection, &own, &own_proof, proofs))
            .or_else(|| {
                if !proofs.binding_current(&own_proof) {
                    return None;
                }
                let (_, original, departure) = projection
                    .authority
                    .legacy_retired_source(&own, &own_proof)?;
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
            })?;
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
            ids.extend(projection.authority.legacy_closing_basis(&own, &own_proof));
        }
        if state.is_dissolved() {
            ids.extend(
                log.events(&group)
                    .into_iter()
                    .find(|event| {
                        event.kind == EventKind::ChannelDissolve
                            && projection.authorized.contains(&event.id)
                    })
                    .map(|event| event.id),
            );
        }
        ids.sort();
        ids.dedup();
        let controls = ids.iter().filter_map(|id| log.get(id).cloned()).collect();
        let (local_frontier, (fingerprint, count)) = authorization_view_at(log, group, scope);
        let mut frontier = local_frontier.clone();
        let complete_frontier = frontier.len() <= MAX_ACCESS_FRONTIER;
        frontier.truncate(MAX_ACCESS_FRONTIER);
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
                && scope
                    .is_none_or(|scope| head.access.frontier.iter().all(|id| scope.contains(id)))
                && !result.compatible(&head.access)
                && view_contains(log, group, (fingerprint, count), &head.access)
                && evaluated(log, group, &head.access)
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
        Some(result)
    }
}
