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
    let relevant: HashSet<_> = log
        .events(&group)
        .into_iter()
        // The validated log can contain rejected competing controls which are
        // parents of a later applied decision. The reducer's authorized index
        // is not a complete record of its state transitions.
        .filter(|event| super::wire::authorization_control(event.kind))
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

impl Node {
    pub fn account_group_access(
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
        let heads: Vec<_> = self
            .group_control_heads(group)
            .into_iter()
            .filter(|head| self.account_group_binding_current(&head.proof))
            .collect();
        if heads.len() > MAX_OBSERVED_SOURCES || self.group_control_has_unreadable_head(group) {
            return Ok(access);
        }
        let own = self.identity.public();
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
        let mut left = false;
        for (candidate, _, is_local, known_public) in maximal {
            left |= candidate.observation.role == AccountGroupRole::Left;
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
    pub(super) fn public_account_group_source(
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
        let heads: Vec<_> = self
            .group_control_heads(group)
            .into_iter()
            .filter(|head| self.account_group_binding_current(&head.proof))
            .collect();
        let log = self.log.lock().expect("log mutex");
        let projection =
            crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
        self.account_group_snapshot(group, &log, &projection, prior, &heads)
    }
    fn account_group_snapshot(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &crate::node::channel::ChannelMembershipProjection,
        prior: Option<&SourceAccess>,
        heads: &[GroupControlHead],
    ) -> Option<SourceAccess> {
        let state = projection.state.as_ref()?;
        let own = self.identity.public();
        let own_proof = self.own_group_proof();
        let source = Self::public_account_group_source(group, projection, &own, &own_proof)
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
            self.historical_author(&device.ed25519_pub)
                .filter(|proof| proof.public() == *device)
                .or_else(|| {
                    heads
                        .iter()
                        .flat_map(|head| {
                            std::iter::once(&head.proof).chain(head.access.owner_proof.iter())
                        })
                        .find(|proof| {
                            proof.public() == *device && self.account_group_binding_current(proof)
                        })
                        .cloned()
                })
        });
        let mut proofs: Vec<_> = self.historical_peer_proofs();
        proofs.push(own_proof);
        proofs.extend(heads.iter().flat_map(|head| {
            std::iter::once(head.proof.clone()).chain(
                head.access
                    .observation
                    .sources
                    .iter()
                    .map(|bound| bound.proof.clone()),
            )
        }));
        proofs.extend(owner_proof.clone());
        let mut members = BTreeMap::new();
        for device in state.members() {
            let proof = proofs
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
            for proof in proofs {
                if !self.account_group_binding_current(&proof) {
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
        let (local_frontier, (fingerprint, count)) = authorization_view(log, group);
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
