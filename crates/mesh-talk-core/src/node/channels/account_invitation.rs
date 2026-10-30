//! Modern invitation decisions shared by direct callers and original account intents.
//! The existing invitation, account-authority and encrypted history bytes are retained.
use super::*;
use crate::{
    channel::ChannelMeta,
    discovery::Announce,
    eventlog::persist::PersistentEventLog,
    node::{
        channel_account_authority::AccountAuthorityControl, join_history::PreparedInvitationHistory,
    },
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InvitationInput {
    pub(in crate::node) target_account_id: String,
}
impl InvitationInput {
    pub(in crate::node) fn operation(&self) -> crate::node::AccountGroupOperation {
        crate::node::AccountGroupOperation::Invite {
            target_account_id: self.target_account_id.clone(),
        }
    }
    pub(in crate::node) fn canonical(&self) -> bool {
        self.target_account_id.len() == 32
            && hex::decode(&self.target_account_id).is_ok_and(|bytes| {
                bytes.len() == 16 && hex::encode(bytes) == self.target_account_id
            })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InvitationWitness {
    pub(in crate::node) own: Announce,
    pub(in crate::node) target: Announce,
    pub(in crate::node) incarnation: [u8; 16],
    pub(in crate::node) parents: Vec<EventId>,
    /// Original causal member set, before adding the invited device.
    pub(in crate::node) recipients: Vec<PublicIdentity>,
}
pub(in crate::node) struct InvitationDecision {
    pub meta: ChannelMeta,
    pub control: AccountAuthorityControl,
    pub recipients: Vec<PublicIdentity>,
}
pub(in crate::node) struct PreparedAccountInvitation {
    pub transaction: crate::node::delivery_store::DeliveryTransaction,
    pub witness: InvitationWitness,
    pub recipients: Vec<PublicIdentity>,
    pub invitation: EventId,
}
pub(in crate::node) struct InvitationAssets {
    pub target: Announce,
    pub incarnation: [u8; 16],
    pub history: PreparedInvitationHistory,
}
pub(in crate::node) struct InvitationPreparation<'a> {
    pub group: ConversationId,
    pub log: &'a PersistentEventLog,
    pub state: &'a crate::channel::ChannelState,
    pub projection: &'a crate::node::channel::ChannelMembershipProjection,
    pub input: &'a InvitationInput,
    pub witness: InvitationWitness,
    pub history: PreparedInvitationHistory,
}

/// No current roster, clock, locks, randomness or public effects are consulted.
/// A current accepted OR pending grant is the existing canonical no-op.
pub(in crate::node) fn expected_invitation(
    group: ConversationId,
    projection: &crate::node::channel::ChannelMembershipProjection,
    input: &InvitationInput,
    witness: &InvitationWitness,
) -> Result<Option<InvitationDecision>, NodeError> {
    let invalid = || NodeError::Authorization("invalid modern account invitation".into());
    let state = projection.state.as_ref().ok_or_else(invalid)?;
    let previous = projection.authority.current_event().ok_or_else(invalid)?;
    let account = witness.own.account_cert.as_ref().ok_or_else(invalid)?;
    let target = witness.target.account_cert.as_ref().ok_or_else(invalid)?;
    if !input.canonical()
        || state.id() != group
        || state.is_dissolved()
        || !witness.own.valid_presence()
        || witness.own.post_office
        || !witness.target.valid_presence()
        || witness.target.post_office
        || witness.target.account_id().as_deref() != Some(input.target_account_id.as_str())
        || state.members() != witness.recipients
        || Node::public_account_group_source(group, projection, &witness.own.public(), &witness.own)
            .is_none_or(|source| source.role != crate::node::AccountGroupRole::Owner)
        || state.members().iter().any(|public| {
            public.ed25519_pub == witness.target.ed25519_pub && *public != witness.target.public()
        })
    {
        return Err(invalid());
    }
    if projection
        .authority
        .account_grant(&target.account_ed25519_pub)
        .is_some()
    {
        return Ok(None);
    }
    let mut recipients = witness.recipients.clone();
    recipients.push(witness.target.public());
    recipients.sort_by_key(PublicIdentity::user_id);
    recipients.dedup();
    let meta = ChannelMeta {
        name: state.name().into(),
        members: recipients.clone(),
        epoch: state.epoch().checked_add(1).ok_or_else(invalid)?,
        owner: state.owner().into(),
    };
    let mut grants = projection.authority.control_grants();
    let target_account = witness
        .target
        .account_cert
        .as_ref()
        .ok_or_else(invalid)?
        .account_ed25519_pub;
    let revoked = grants
        .iter()
        .filter(|grant| {
            grant
                .id()
                .is_some_and(|id| id.account_pub == target_account)
        })
        .filter_map(crate::node::channel_account_authority::AccountGrant::id)
        .collect();
    grants.retain(|grant| grant.id().is_none_or(|id| id.account_pub != target_account));
    grants.push(crate::node::channel_account_authority::AccountGrant {
        incarnation: witness.incarnation,
        pending: true,
        original: witness.target.clone(),
        membership_event: EventId::new([0; 32]),
        acceptance_event: None,
    });
    Ok(Some(InvitationDecision {
        meta,
        recipients,
        control: AccountAuthorityControl {
            version: 1,
            previous: Some(previous),
            issuer: witness.own.clone(),
            owner_account: account.account_ed25519_pub,
            grants,
            revoked,
            legacy_bindings: vec![],
            account_signature: vec![],
        },
    }))
}
impl Node {
    /// One authenticated account-target resolver for creation and later invitations.
    pub(in crate::node) async fn verified_account_invitation_target(
        &self,
        target_account_id: &str,
    ) -> Result<Announce, NodeError> {
        if !(InvitationInput {
            target_account_id: target_account_id.into(),
        })
        .canonical()
        {
            return Err(NodeError::InvalidInput("invalid target account".into()));
        }
        let mut peers: Vec<_> = self
            .routing_peers()
            .into_iter()
            .filter(|peer| peer.account_id.as_deref() == Some(target_account_id))
            .collect();
        peers.sort_by_key(|peer| peer.public.user_id());
        let mut verified = None;
        for peer in peers {
            if let Some((
                crate::node::channel_account_bootstrap::AccountGroupResponse::Capability {
                    version: 1,
                    ..
                },
                proof,
            )) = self
                .query_account_groups_verified(
                    &peer,
                    crate::node::channel_account_bootstrap::AccountGroupRequest::Capability,
                )
                .await
                && proof.public() == peer.public
                && proof.valid_presence()
                && !proof.post_office
                && proof.account_id().as_deref() == Some(target_account_id)
                && proof.account_cert.is_some()
            {
                verified = Some(proof);
                break;
            }
        }
        verified.ok_or_else(|| {
            NodeError::Channel(
                "the invited account must be reachable with a compatible client".into(),
            )
        })
    }
    pub(in crate::node) async fn prepare_invitation_assets(
        &self,
        group: ConversationId,
        target_account_id: &str,
    ) -> Result<InvitationAssets, NodeError> {
        let input = InvitationInput {
            target_account_id: target_account_id.into(),
        };
        if !input.canonical() {
            return Err(NodeError::InvalidInput("invalid target account".into()));
        }
        if let Some(grant) = self
            .account_group_projection(group)?
            .authority
            .grants()
            .into_iter()
            .find(|grant| grant.original.account_id().as_deref() == Some(target_account_id))
        {
            let target = grant.original;
            let history = self.prepare_invitation_history(
                group,
                &target.public(),
                Vec::new(),
                crate::channel::HistoryPolicy::Off,
            )?;
            return Ok(InvitationAssets {
                target,
                incarnation: grant.incarnation,
                history,
            });
        }
        let target = self
            .verified_account_invitation_target(target_account_id)
            .await?;
        let _policy = self.privacy.gate.read().await;
        let policy = self.channel_history_policy(group)?;
        let selected = self.select_join_history(group, policy, super::super::node::now_millis())?;
        let history = self.prepare_invitation_history(group, &target.public(), selected, policy)?;
        let mut incarnation = [0; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut incarnation);
        Ok(InvitationAssets {
            target,
            incarnation,
            history,
        })
    }
    pub(in crate::node) fn prepare_account_invitation_locked(
        &self,
        preparation: InvitationPreparation<'_>,
    ) -> Result<Option<PreparedAccountInvitation>, NodeError> {
        let InvitationPreparation {
            group,
            log,
            state,
            projection,
            input,
            witness,
            history,
        } = preparation;
        if witness.own.public() != self.identity.public()
            || witness
                .own
                .account_cert
                .as_ref()
                .is_none_or(|cert| cert.account_ed25519_pub != self.account.public().ed25519_pub)
            || log.prepare(&group).map_err(NodeError::Log)?.0 != witness.parents
        {
            return Err(NodeError::Authorization(
                "invitation source or causal cut changed".into(),
            ));
        }
        let Some(InvitationDecision {
            meta,
            mut control,
            recipients,
        }) = expected_invitation(group, projection, input, &witness)?
        else {
            return Ok(None);
        };
        let transaction = self.prepare_invitation_with_history_and_control_locked(
            group,
            &meta,
            &witness.target.public(),
            log,
            state,
            projection,
            &witness.own,
            history,
            |invitation| {
                control
                    .grants
                    .last_mut()
                    .expect("new invitation grant")
                    .membership_event = invitation;
                let payload = control.signed_encode(group, &self.account);
                if AccountAuthorityControl::decode(&payload).is_none() {
                    return Err(NodeError::Channel(
                        "channel account authority exceeds supported bounds".into(),
                    ));
                }
                Ok(Some((EventKind::ChannelAccountAuthority, payload)))
            },
        )?;
        let crate::node::delivery_store::DeliveryTransaction::ChannelOutgoing { events, .. } =
            &transaction
        else {
            unreachable!("invitation owner returns ChannelOutgoing");
        };
        let invitation = events.first().expect("prepared invitation").id;
        Ok(Some(PreparedAccountInvitation {
            transaction,
            witness,
            recipients,
            invitation,
        }))
    }
}

#[cfg(test)]
pub(in crate::node) mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};

    pub(in crate::node) struct Fixture {
        pub base: crate::node::channels::account_self_control::tests::Fixture,
        pub input: InvitationInput,
        pub witness: InvitationWitness,
    }
    pub(in crate::node) fn fixture() -> Fixture {
        let base = crate::node::channels::account_self_control::tests::fixture(false);
        let target = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&target, &account, "target", 1);
        let input = InvitationInput {
            target_account_id: account.account_id(),
        };
        let witness = InvitationWitness {
            own: Announce::new_with_account(&base.owner, &base.owner_account, "owner", 1),
            target: proof,
            incarnation: [67; 16],
            parents: vec![base.events.last().unwrap().id],
            recipients: base.projection().state.unwrap().members().to_vec(),
        };
        Fixture {
            base,
            input,
            witness,
        }
    }
    #[test]
    fn invitation_owner_decision_preserves_exact_target_incarnation_and_original_members() {
        let f = fixture();
        let decision =
            expected_invitation(f.base.group, &f.base.projection(), &f.input, &f.witness)
                .unwrap()
                .unwrap();
        assert_eq!(
            decision.meta.epoch,
            f.base.projection().state.unwrap().epoch() + 1
        );
        assert_eq!(decision.meta.name, "account self controls");
        let target = decision
            .control
            .grants
            .iter()
            .find(|grant| grant.original == f.witness.target)
            .unwrap();
        assert!(target.pending);
        assert_eq!(target.incarnation, f.witness.incarnation);
        assert_eq!(target.membership_event, EventId::new([0; 32]));
        assert!(target.acceptance_event.is_none());
        assert_eq!(
            decision.control.previous,
            Some(f.base.events.last().unwrap().id)
        );
        assert!(decision.control.revoked.is_empty() && decision.control.legacy_bindings.is_empty());
        let mut members = f.witness.recipients.clone();
        members.push(f.witness.target.public());
        members.sort_by_key(PublicIdentity::user_id);
        members.dedup();
        assert_eq!(decision.meta.members, members);
        assert_eq!(decision.recipients, members);
    }
    #[test]
    fn invitation_existing_accepted_account_is_canonical_noop() {
        let mut f = fixture();
        f.input.target_account_id = f.base.account.account_id();
        f.witness.target = f.base.proof.clone();
        assert!(
            expected_invitation(f.base.group, &f.base.projection(), &f.input, &f.witness)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn invitation_existing_pending_account_is_canonical_noop() {
        let mut f = fixture();
        f.base = crate::node::channels::account_self_control::tests::fixture(true);
        f.witness.own =
            Announce::new_with_account(&f.base.owner, &f.base.owner_account, "owner", 1);
        f.witness.parents = vec![f.base.events.last().unwrap().id];
        f.witness.recipients = f.base.projection().state.unwrap().members().to_vec();
        f.input.target_account_id = f.base.account.account_id();
        f.witness.target = f.base.proof.clone();
        assert!(
            expected_invitation(f.base.group, &f.base.projection(), &f.input, &f.witness)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn invitation_rejects_nonowner_target_account_swap_and_changed_recipient_cut() {
        for mutation in 0..3 {
            let mut f = fixture();
            match mutation {
                0 => f.witness.own = f.base.proof.clone(),
                1 => f.input.target_account_id = f.base.account.account_id(),
                _ => {
                    f.witness.recipients.pop();
                }
            }
            assert!(
                expected_invitation(f.base.group, &f.base.projection(), &f.input, &f.witness)
                    .is_err()
            );
        }
    }
}
