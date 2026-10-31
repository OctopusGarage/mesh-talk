//! Canonical owner effects prepared from one immutable public membership view.
use super::*;
use crate::{
    channel::HistoryPolicy,
    discovery::Announce,
    eventlog::{Author, Event, persist::PersistentEventLog},
    identity::account::AccountPublic,
    node::{
        AccountGroupRole, MembershipLifetime,
        channel_account_authority::{AccountAuthorityControl, AccountGrant, AccountGrantId},
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OwnerControlInput {
    Rename(String),
    HistoryPolicy(HistoryPolicy),
    Dissolve,
    RemoveAccount(String),
    Transfer(String),
}
impl OwnerControlInput {
    pub(in crate::node) fn canonical(&self) -> bool {
        match self {
            Self::Rename(name) => normalize_simple_name(name).is_ok_and(|value| value == *name),
            Self::RemoveAccount(account) | Self::Transfer(account) => {
                account.len() == 32
                    && hex::decode(account)
                        .is_ok_and(|bytes| bytes.len() == 16 && hex::encode(bytes) == *account)
            }
            Self::HistoryPolicy(_) | Self::Dissolve => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct OwnerControlMember {
    pub lifetime: MembershipLifetime,
    pub accepted: bool,
    pub current: Option<Announce>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OwnerControlWitness {
    pub(in crate::node) own: Announce,
    pub(in crate::node) members: Vec<OwnerControlMember>,
    /// Canonical owner representative in the saved view: the explicit legacy
    /// owner or first accepted modern owner-account device, using full keys.
    pub(in crate::node) owner: PublicIdentity,
    pub(in crate::node) target_grant: Option<AccountGrantId>,
    pub(in crate::node) parents: Vec<EventId>,
}
pub(in crate::node) struct OwnerControlProofSnapshot {
    own: Announce,
    current: BTreeMap<[u8; 32], Announce>,
}
pub(in crate::node) struct PreparedOwnerControl {
    pub event: Event,
    pub witness: OwnerControlWitness,
    pub recipients: Vec<PublicIdentity>,
}

/// Account authority is signed separately from the event, after the pure owner
/// decision. Validation checks this same unsigned decision plus that signature.
pub(in crate::node) enum OwnerControlEffect {
    Public(EventKind, Vec<u8>),
    Account(Box<AccountAuthorityControl>),
}

fn member_key(member: &OwnerControlMember) -> (String, [u8; 32], [u8; 32]) {
    let public = &member.lifetime.member;
    (public.user_id(), public.ed25519_pub, public.x25519_pub)
}
pub(in crate::node) fn physical_members(
    projection: &ChannelMembershipProjection,
) -> Vec<OwnerControlMember> {
    let mut members: Vec<_> = projection
        .accepted
        .iter()
        .map(|row| (row, true))
        .chain(projection.invited.iter().map(|row| (row, false)))
        .map(|(row, accepted)| OwnerControlMember {
            lifetime: MembershipLifetime {
                member: row.public.clone(),
                membership: row.membership_event,
                acceptance: row.acceptance_event,
            },
            accepted,
            current: None,
        })
        .collect();
    members.sort_by_key(member_key);
    members
}
pub(in crate::node) fn selected_owner(
    projection: &ChannelMembershipProjection,
) -> Option<PublicIdentity> {
    let state = projection.state.as_ref()?;
    let account = projection.authority.owner_account_id();
    let mut members: Vec<_> = state
        .members()
        .iter()
        .filter(|member| {
            state.is_accepted_member(&member.user_id())
                && if let Some(account) = account.as_ref() {
                    state.member_account_id(&member.user_id()) == Some(account.as_str())
                } else {
                    member.user_id() == state.owner()
                }
        })
        .cloned()
        .collect();
    members.sort_by_key(|public| (public.user_id(), public.ed25519_pub, public.x25519_pub));
    members.into_iter().next()
}
pub(in crate::node) fn witness_rows_match(
    projection: &ChannelMembershipProjection,
    witness: &OwnerControlWitness,
) -> bool {
    let expected = physical_members(projection);
    expected.len() == witness.members.len()
        && expected.iter().zip(&witness.members).all(|(a, b)| {
            a.lifetime == b.lifetime
                && a.accepted == b.accepted
                && b.current.as_ref().is_none_or(|proof| {
                    proof.public() == b.lifetime.member
                        && proof.valid_presence()
                        && !proof.post_office
                })
        })
        && selected_owner(projection).as_ref() == Some(&witness.owner)
}

/// One source of truth for authority, account target selection and canonical
/// effects. It performs no locking, appending, network or current roster reads.
pub(in crate::node) fn expected_effect(
    group: ConversationId,
    projection: &ChannelMembershipProjection,
    input: &OwnerControlInput,
    witness: &OwnerControlWitness,
    history: HistoryPolicy,
) -> Result<Option<OwnerControlEffect>, NodeError> {
    let state = projection
        .state
        .as_ref()
        .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
    if !input.canonical()
        || state.id() != group
        || !witness.own.valid_presence()
        || witness.own.post_office
        || witness.own.account_cert.is_none()
        || !witness_rows_match(projection, witness)
        || Node::public_account_group_source(group, projection, &witness.own.public(), &witness.own)
            .is_none_or(|source| source.role != AccountGroupRole::Owner)
    {
        return Err(NodeError::Channel(
            "invalid active channel owner control witness".into(),
        ));
    }
    match input {
        OwnerControlInput::Rename(name) => {
            if witness.target_grant.is_some() {
                return Err(NodeError::Channel("unexpected target grant".into()));
            }
            Ok((state.name() != name).then(|| {
                OwnerControlEffect::Public(EventKind::ChannelRename, name.as_bytes().to_vec())
            }))
        }
        OwnerControlInput::HistoryPolicy(policy) => {
            if witness.target_grant.is_some() {
                return Err(NodeError::Channel("unexpected target grant".into()));
            }
            Ok((history != *policy).then(|| {
                OwnerControlEffect::Public(
                    EventKind::ChannelHistoryPolicy,
                    super::super::join_history::encode_policy(*policy),
                )
            }))
        }
        OwnerControlInput::Dissolve => {
            if witness.target_grant.is_some() {
                return Err(NodeError::Channel("unexpected target grant".into()));
            }
            Ok(Some(OwnerControlEffect::Public(
                EventKind::ChannelDissolve,
                Vec::new(),
            )))
        }
        OwnerControlInput::RemoveAccount(target) | OwnerControlInput::Transfer(target) => {
            let transfer = matches!(input, OwnerControlInput::Transfer(_));
            if let Some(previous) = projection.authority.current_event() {
                let mut control = AccountAuthorityControl {
                    version: 1,
                    previous: Some(previous),
                    issuer: witness.own.clone(),
                    owner_account: witness
                        .own
                        .account_cert
                        .as_ref()
                        .expect("validated proof")
                        .account_ed25519_pub,
                    grants: projection.authority.control_grants(),
                    revoked: Vec::new(),
                    legacy_bindings: Vec::new(),
                    account_signature: Vec::new(),
                };
                let owner = projection
                    .authority
                    .owner_account_id()
                    .expect("active account authority");
                if owner == *target {
                    if transfer {
                        return Ok(None);
                    }
                    return Err(NodeError::Channel(
                        "transfer channel ownership before the owner account leaves".into(),
                    ));
                }
                let index = control.grants.iter().position(|grant| {
                    grant.id().is_some_and(|id| {
                        AccountPublic {
                            ed25519_pub: id.account_pub,
                        }
                        .account_id()
                            == *target
                    })
                });
                let Some(index) = index else {
                    if witness.target_grant.is_some() {
                        return Err(NodeError::Channel("unexpected target grant".into()));
                    }
                    if transfer {
                        return Err(NodeError::Channel(
                            "new owner has no active channel account grant".into(),
                        ));
                    }
                    return Ok(None);
                };
                let grant = &control.grants[index];
                if witness.target_grant != grant.id() {
                    return Err(NodeError::Channel("channel target grant changed".into()));
                }
                if transfer {
                    if !projection
                        .authority
                        .is_member(&grant.original.public(), &grant.original)
                    {
                        return Err(NodeError::Channel(
                            "new owner must have accepted the channel invitation".into(),
                        ));
                    }
                    control.owner_account = grant.id().expect("projected grant").account_pub;
                } else {
                    control
                        .revoked
                        .push(control.grants.remove(index).id().expect("projected grant"));
                }
                Ok(Some(OwnerControlEffect::Account(Box::new(control))))
            } else {
                if witness.target_grant.is_some() {
                    return Err(NodeError::Channel("unexpected legacy target grant".into()));
                }
                let belongs = |member: &OwnerControlMember| {
                    (!transfer || member.accepted)
                        && member
                            .current
                            .as_ref()
                            .and_then(Announce::account_id)
                            .as_ref()
                            == Some(target)
                };
                let targets: Vec<_> = witness
                    .members
                    .iter()
                    .filter(|member| belongs(member))
                    .collect();
                if transfer {
                    if witness.own.account_id().as_ref() == Some(target) {
                        return Ok(None);
                    }
                    let elected = targets.first().ok_or_else(|| {
                        NodeError::Channel(
                            "new owner account must have an accepted certified channel device"
                                .into(),
                        )
                    })?;
                    let owner = elected.lifetime.member.user_id();
                    Ok((owner != state.owner()).then(|| {
                        OwnerControlEffect::Public(
                            EventKind::ChannelOwnerTransfer,
                            owner.into_bytes(),
                        )
                    }))
                } else {
                    if targets
                        .iter()
                        .any(|member| member.lifetime.member == witness.owner)
                    {
                        return Err(NodeError::Channel(
                            "transfer channel ownership before the owner leaves".into(),
                        ));
                    }
                    if targets.is_empty() {
                        return Ok(None);
                    }
                    let remaining = canonical_members(
                        state
                            .members()
                            .iter()
                            .filter(|public| {
                                !targets.iter().any(|row| row.lifetime.member == **public)
                            })
                            .cloned()
                            .collect(),
                    );
                    if remaining.is_empty() {
                        return Err(NodeError::Channel(
                            "cannot remove the last channel member".into(),
                        ));
                    }
                    let meta = ChannelMeta {
                        name: state.name().into(),
                        members: remaining,
                        epoch: state
                            .epoch()
                            .checked_add(1)
                            .ok_or_else(|| NodeError::Channel("channel epoch exhausted".into()))?,
                        owner: state.owner().into(),
                    };
                    Ok(Some(OwnerControlEffect::Public(
                        EventKind::MembershipChange,
                        meta.encode(),
                    )))
                }
            }
        }
    }
}

impl Node {
    /// Direct mutators share the prepared owner decision and existing WAL.
    /// The caller owns membership. Recover before projection, then retain the
    /// delivery allocation guard until this exact immutable event is installed.
    pub(in crate::node) fn apply_owner_control(
        &self,
        channel: ConversationId,
        input: &OwnerControlInput,
    ) -> Result<super::super::channel_account_controls::AccountChannelMutation, NodeError> {
        use super::super::{
            channel_account_controls::AccountChannelMutation, delivery_store::DeliveryTransaction,
        };
        let proofs = self.owner_control_proof_snapshot();
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let prepared = {
            let log = self.log.lock().expect("log mutex");
            let projection = ChannelBook::project_membership(channel, &log.events(&channel)?);
            let prepared =
                self.prepare_owner_control(channel, &log, &projection, input, &proofs)?;
            log.sync().map_err(NodeError::Log)?;
            prepared
        };
        let Some(prepared) = prepared else {
            return Ok(AccountChannelMutation::Unchanged);
        };
        let id = prepared.event.id;
        delivery
            .begin(DeliveryTransaction::AccountOwnerControl {
                input: input.clone(),
                witness: prepared.witness,
                events: vec![prepared.event],
            })
            .map_err(NodeError::Log)?;
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        drop(delivery);
        self.process_channel(channel);
        let projection = self.account_group_projection(channel)?;
        if !projection.is_authorized(id)? {
            return Err(NodeError::Channel(
                "channel owner control was not admitted".into(),
            ));
        }
        Ok(AccountChannelMutation::Changed(prepared.recipients))
    }
    /// Capture current signed identities before taking the public log lock.
    pub(in crate::node) fn owner_control_proof_snapshot(&self) -> OwnerControlProofSnapshot {
        let mut current: BTreeMap<_, _> = self
            .roster
            .lock()
            .expect("roster mutex")
            .historical_announcements()
            .into_iter()
            .map(|proof| (proof.ed25519_pub, proof))
            .collect();
        if let Some(state) = self
            .privacy
            .state
            .read()
            .expect("privacy state mutex")
            .as_ref()
        {
            current.retain(|_, proof| state.proofs.accepts_binding(proof));
            for proof in state.proofs.announcements() {
                if state.proofs.accepts_binding(&proof) {
                    current.insert(proof.ed25519_pub, proof);
                }
            }
        }
        let own = self.own_group_proof();
        current.insert(own.ed25519_pub, own.clone());
        OwnerControlProofSnapshot { own, current }
    }
    pub(in crate::node) fn prepare_owner_control(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &ChannelMembershipProjection,
        input: &OwnerControlInput,
        proofs: &OwnerControlProofSnapshot,
    ) -> Result<Option<PreparedOwnerControl>, NodeError> {
        let state = projection
            .state
            .as_ref()
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        let owner = selected_owner(projection)
            .ok_or_else(|| NodeError::Channel("channel owner identity unavailable".into()))?;
        let mut members = physical_members(projection);
        for member in &mut members {
            member.current = proofs
                .current
                .get(&member.lifetime.member.ed25519_pub)
                .filter(|proof| {
                    proof.public() == member.lifetime.member
                        && proof.valid_presence()
                        && !proof.post_office
                })
                .cloned();
        }
        let target_grant = match input {
            OwnerControlInput::RemoveAccount(target) | OwnerControlInput::Transfer(target) => {
                projection
                    .authority
                    .grants()
                    .iter()
                    .filter_map(AccountGrant::id)
                    .find(|id| {
                        AccountPublic {
                            ed25519_pub: id.account_pub,
                        }
                        .account_id()
                            == *target
                    })
            }
            _ => None,
        };
        let (parents, lamport) = log.prepare(&group).map_err(NodeError::Log)?;
        let witness = OwnerControlWitness {
            own: proofs.own.clone(),
            members,
            owner,
            target_grant,
            parents: parents.clone(),
        };
        let Some(effect) = expected_effect(
            group,
            projection,
            input,
            &witness,
            super::super::join_history::policy(&log.events(&group)?.iter().collect::<Vec<_>>()),
        )?
        else {
            return Ok(None);
        };
        let (kind, payload) = match effect {
            OwnerControlEffect::Public(kind, payload) => (kind, payload),
            OwnerControlEffect::Account(control) => {
                let payload = control.signed_encode(group, &self.account);
                if AccountAuthorityControl::decode(&payload).is_none() {
                    return Err(NodeError::Channel(
                        "channel account authority exceeds supported bounds".into(),
                    ));
                }
                (EventKind::ChannelAccountAuthority, payload)
            }
        };
        let seq = log
            .next_sequence(
                &group,
                &Author::from_ed25519(self.identity.public().ed25519_pub),
            )
            .map_err(NodeError::Log)?;
        let event = Event::new(
            &self.identity,
            group,
            seq,
            parents,
            lamport,
            now_millis(),
            kind,
            payload,
        );
        if !super::super::session::event_fits_frame(&event) {
            return Err(NodeError::Channel(
                "channel owner control exceeds supported frame".into(),
            ));
        }
        Ok(Some(PreparedOwnerControl {
            event,
            witness,
            recipients: state.members().to_vec(),
        }))
    }
}

#[cfg(test)]
mod witness_row_regression_tests {
    use super::*;
    use crate::channel::ChannelMeta;
    use crate::node::channels::account_self_control::tests::{Fixture, fixture};

    fn witness(f: &Fixture, projection: &ChannelMembershipProjection) -> OwnerControlWitness {
        let own = Announce::new_with_account(&f.owner, &f.owner_account, "owner", 1);
        let mut members = physical_members(projection);
        for member in &mut members {
            member.current = Some(if member.lifetime.member == f.owner.public() {
                own.clone()
            } else {
                f.proof.clone()
            });
        }
        OwnerControlWitness {
            own,
            members,
            owner: selected_owner(projection).unwrap(),
            target_grant: None,
            parents: vec![f.events.last().unwrap().id],
        }
    }
    fn rename(
        projection: &ChannelMembershipProjection,
        witness: &OwnerControlWitness,
        group: ConversationId,
    ) -> Result<Option<OwnerControlEffect>, NodeError> {
        expected_effect(
            group,
            projection,
            &OwnerControlInput::Rename("Canonical rename".into()),
            witness,
            HistoryPolicy::Off,
        )
    }

    #[test]
    fn owner_decision_rejects_stale_rejoined_lifetime_with_current_acceptance() {
        let mut f = fixture(false);
        f.events.truncate(1); // The actual signed legacy genesis, before format cutover.
        let before = f.projection();
        let mut stale = witness(&f, &before);
        assert!(matches!(
            rename(&before, &stale, f.group).unwrap(),
            Some(OwnerControlEffect::Public(EventKind::ChannelRename, _))
        ));
        let leave = Event::new(
            &f.member,
            f.group,
            1,
            vec![f.events[0].id],
            2,
            2,
            EventKind::ChannelLeave,
            vec![],
        );
        let rejoin = Event::new(
            &f.owner,
            f.group,
            2,
            vec![leave.id],
            3,
            3,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "account self controls".into(),
                members: vec![f.owner.public(), f.member.public()],
                epoch: 2,
                owner: f.owner.user_id(),
            }
            .encode(),
        );
        f.events.extend([leave.clone(), rejoin.clone()]);
        let current = f.projection();
        assert!(current.authorized.contains(&leave.id) && current.authorized.contains(&rejoin.id));
        let fresh = witness(&f, &current);
        let old_row = stale
            .members
            .iter()
            .find(|row| row.lifetime.member == f.member.public())
            .unwrap();
        let new_row = fresh
            .members
            .iter()
            .find(|row| row.lifetime.member == f.member.public())
            .unwrap();
        assert!(old_row.accepted && new_row.accepted);
        assert!(old_row.current.as_ref().unwrap().valid_presence());
        assert_ne!(old_row.lifetime, new_row.lifetime);
        assert_eq!(stale.owner, fresh.owner);
        // Use the current real parent cut while retaining the stale paired row.
        stale.parents = fresh.parents.clone();
        assert!(rename(&current, &fresh, f.group).unwrap().is_some());
        assert!(
            rename(&current, &stale, f.group).is_err(),
            "accepted and certified is insufficient when the physical lifetime changed"
        );
    }

    #[test]
    fn owner_decision_rejects_matching_lifetime_with_false_role_or_wrong_proof() {
        let f = fixture(false);
        let projection = f.projection();
        let valid = witness(&f, &projection);
        assert!(rename(&projection, &valid, f.group).unwrap().is_some());
        let index = valid
            .members
            .iter()
            .position(|row| row.lifetime.member == f.member.public())
            .unwrap();
        let mut false_role = valid.clone();
        false_role.members[index].accepted = false;
        assert!(
            rename(&projection, &false_role, f.group).is_err(),
            "matching lifetime cannot replace the signed accepted role"
        );
        let mut wrong_proof = valid.clone();
        wrong_proof.members[index].current = Some(valid.own.clone());
        assert!(
            wrong_proof.members[index]
                .current
                .as_ref()
                .unwrap()
                .valid_presence()
        );
        assert!(
            rename(&projection, &wrong_proof, f.group).is_err(),
            "a valid other member proof cannot stand in for this exact physical identity"
        );
    }
}
