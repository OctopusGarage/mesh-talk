//! Account invitation acceptance and exact physical/account departure preparation.
//! The integration owner retains membership/delivery/log guards and WAL replay.

use crate::{
    discovery::Announce,
    eventlog::{Author, ConversationId, Event, EventId, EventKind, persist::PersistentEventLog},
    identity::device::PublicIdentity,
    node::{
        AccountGroupOperation, AccountGroupRole, Node, NodeError,
        channel::ChannelMembershipProjection, channel_account_authority::AccountGrantId,
    },
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AccountSelfControlInput {
    Accept,
    Leave,
    Decline,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountSelfControlWitness {
    pub(in crate::node) own: Announce,
    pub(in crate::node) grant: AccountGrantId,
    pub(in crate::node) parents: Vec<EventId>,
    pub(in crate::node) recipients: Vec<PublicIdentity>,
}

impl AccountSelfControlInput {
    pub(in crate::node) fn operation(self) -> AccountGroupOperation {
        match self {
            Self::Accept => AccountGroupOperation::AcceptInvitation,
            Self::Leave => AccountGroupOperation::LeaveAccount,
            Self::Decline => AccountGroupOperation::DeclineInvitation,
        }
    }
    pub(in crate::node) fn required_role(self) -> AccountGroupRole {
        match self {
            Self::Accept | Self::Decline => AccountGroupRole::Invited,
            Self::Leave => AccountGroupRole::Member,
        }
    }
    pub(in crate::node) fn event_payload(self, grant: AccountGrantId) -> Vec<u8> {
        match self {
            Self::Accept | Self::Leave => vec![],
            Self::Decline => {
                crate::node::channel_account_authority::AccountDeclinePayload { version: 1, grant }
                    .encode()
            }
        }
    }
    pub(in crate::node) fn event_kind(self) -> EventKind {
        match self {
            Self::Accept => EventKind::ChannelAccept,
            Self::Leave | Self::Decline => EventKind::ChannelLeave,
        }
    }
}
pub(in crate::node) struct PreparedAccountSelfControl {
    pub event: Event,
    pub witness: AccountSelfControlWitness,
    pub recipients: Vec<PublicIdentity>,
}

/// One decision owner for preparation and replay of existing modern self actions.
/// No locks, side effects, current roster lookup or legacy authority inference.
pub(in crate::node) fn expected_self_control(
    group: ConversationId,
    projection: &ChannelMembershipProjection,
    input: AccountSelfControlInput,
    witness: &AccountSelfControlWitness,
) -> Result<(EventKind, Vec<PublicIdentity>), NodeError> {
    let invalid = || NodeError::Authorization("invalid modern account self control".into());
    let state = projection.state.as_ref().ok_or_else(invalid)?;
    let account = witness.own.account_cert.as_ref().ok_or_else(invalid)?;
    if state.id() != group
        || state.is_dissolved()
        || projection.authority.current_event().is_none()
        || !witness.own.valid_presence()
        || witness.own.post_office
        || account.account_ed25519_pub != witness.grant.account_pub
    {
        return Err(invalid());
    }
    let source =
        Node::public_account_group_source(group, projection, &witness.own.public(), &witness.own)
            .ok_or_else(invalid)?;
    let grant = match input {
        AccountSelfControlInput::Accept | AccountSelfControlInput::Decline => projection
            .authority
            .invitation_for_device(&witness.own.public(), &witness.own),
        AccountSelfControlInput::Leave => projection
            .authority
            .grant_for_device(&witness.own.public(), &witness.own),
    }
    .ok_or_else(invalid)?;
    let recipients = state.members().to_vec();
    if source.role != input.required_role()
        || grant.id() != Some(witness.grant)
        || recipients != witness.recipients
        || state
            .epoch()
            .checked_add(if input == AccountSelfControlInput::Decline {
                2
            } else {
                1
            })
            .is_none()
    {
        return Err(invalid());
    }
    Ok((input.event_kind(), recipients))
}

impl Node {
    /// Direct modern entrypoints share this producer with the account facade.
    /// The caller retains membership and host authority; distribution uses the
    /// returned original recipients after all guards have been released.
    pub(in crate::node) fn apply_account_self_control(
        &self,
        group: ConversationId,
        input: AccountSelfControlInput,
        own: &Announce,
    ) -> Result<Vec<PublicIdentity>, NodeError> {
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let (transaction, recipients, id) = {
            let log = self.log.lock().expect("log mutex");
            let projection =
                crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
            let PreparedAccountSelfControl {
                event,
                witness,
                recipients,
            } = self.prepare_account_self_control_locked(group, &log, &projection, input, own)?;
            let id = event.id;
            let transaction =
                crate::node::delivery_store::DeliveryTransaction::AccountSelfControl {
                    input,
                    witness,
                    events: vec![event],
                };
            delivery
                .preflight_group_self_control_transaction(&transaction, &log)
                .map_err(NodeError::Log)?;
            log.sync().map_err(NodeError::Log)?;
            (transaction, recipients, id)
        };
        delivery.begin(transaction).map_err(NodeError::Log)?;
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        drop(delivery);
        self.process_channel(group);
        if !self
            .account_group_projection(group)
            .authorized
            .contains(&id)
        {
            return Err(NodeError::Channel(
                "account self control was not admitted".into(),
            ));
        }
        Ok(recipients)
    }

    /// Caller owns membership/delivery allocation and this immutable log view.
    /// The returned exact event and original recipients must enter the existing
    /// request WAL together before any public append or role publication.
    pub(in crate::node) fn prepare_account_self_control_locked(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &ChannelMembershipProjection,
        input: AccountSelfControlInput,
        own: &Announce,
    ) -> Result<PreparedAccountSelfControl, NodeError> {
        if own.public() != self.identity.public()
            || own
                .account_cert
                .as_ref()
                .is_none_or(|cert| cert.account_ed25519_pub != self.account.public().ed25519_pub)
        {
            return Err(NodeError::Authorization(
                "self-control source identity changed".into(),
            ));
        }
        let state = projection
            .state
            .as_ref()
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        let grant = projection
            .authority
            .account_grant(&self.account.public().ed25519_pub)
            .and_then(|grant| grant.id())
            .ok_or_else(|| NodeError::Authorization("no modern account grant".into()))?;
        let (parents, lamport) = log.prepare(&group).map_err(NodeError::Log)?;
        let witness = AccountSelfControlWitness {
            own: own.clone(),
            grant,
            parents: parents.clone(),
            recipients: state.members().to_vec(),
        };
        let (kind, recipients) = expected_self_control(group, projection, input, &witness)?;
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
            crate::node::node::now_millis(),
            kind,
            input.event_payload(grant),
        );
        if !crate::node::session::event_fits_frame(&event) {
            return Err(NodeError::InvalidInput(
                "account self control exceeds supported frame".into(),
            ));
        }
        Ok(PreparedAccountSelfControl {
            event,
            witness,
            recipients,
        })
    }
}

/// A legacy departure proves only this exact physical membership lifetime.
/// Kept separate from the released modern witness's positional WAL layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LegacyMemberLeaveWitness {
    pub(in crate::node) own: Announce,
    pub(in crate::node) lifetime: crate::node::account_group::MembershipLifetime,
    pub(in crate::node) parents: Vec<EventId>,
    pub(in crate::node) recipients: Vec<PublicIdentity>,
}

pub(in crate::node) struct PreparedLegacyMemberLeave {
    pub event: Event,
    pub witness: LegacyMemberLeaveWitness,
    pub recipients: Vec<PublicIdentity>,
}

pub(in crate::node) fn expected_legacy_member_leave(
    group: ConversationId,
    projection: &ChannelMembershipProjection,
    witness: &LegacyMemberLeaveWitness,
) -> Result<Vec<PublicIdentity>, NodeError> {
    let invalid = || NodeError::Authorization("invalid legacy member departure".into());
    let state = projection.state.as_ref().ok_or_else(invalid)?;
    if state.id() != group
        || state.is_dissolved()
        || projection.authority.current_event().is_some()
        || !witness.own.valid_presence()
        || witness.own.post_office
        || witness.own.account_cert.is_none()
    {
        return Err(invalid());
    }
    let source =
        Node::public_account_group_source(group, projection, &witness.own.public(), &witness.own)
            .ok_or_else(invalid)?;
    if source.role != AccountGroupRole::Member
        || source.lifetime != witness.lifetime
        || source.lifetime.member != witness.own.public()
        || source.authority.authority.is_some()
        || state.members() != witness.recipients
        || state.epoch().checked_add(1).is_none()
    {
        return Err(invalid());
    }
    Ok(state.members().to_vec())
}

impl Node {
    pub(in crate::node) fn prepare_legacy_member_leave_locked(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &ChannelMembershipProjection,
        own: &Announce,
    ) -> Result<PreparedLegacyMemberLeave, NodeError> {
        if own.public() != self.identity.public()
            || own.account_cert.as_ref().is_none_or(|certificate| {
                certificate.account_ed25519_pub != self.account.public().ed25519_pub
            })
        {
            return Err(NodeError::Authorization(
                "legacy departure source identity changed".into(),
            ));
        }
        let source = Node::public_account_group_source(group, projection, &own.public(), own)
            .ok_or_else(|| {
                NodeError::Authorization("legacy departure source unavailable".into())
            })?;
        let (parents, lamport) = log.prepare(&group).map_err(NodeError::Log)?;
        let witness = LegacyMemberLeaveWitness {
            own: own.clone(),
            lifetime: source.lifetime,
            parents: parents.clone(),
            recipients: projection
                .state
                .as_ref()
                .ok_or_else(|| NodeError::Channel("unknown channel".into()))?
                .members()
                .to_vec(),
        };
        let recipients = expected_legacy_member_leave(group, projection, &witness)?;
        let sequence = log
            .next_sequence(
                &group,
                &Author::from_ed25519(self.identity.public().ed25519_pub),
            )
            .map_err(NodeError::Log)?;
        let event = Event::new(
            &self.identity,
            group,
            sequence,
            parents,
            lamport,
            crate::node::node::now_millis(),
            EventKind::ChannelLeave,
            vec![],
        );
        if !crate::node::session::event_fits_frame(&event) {
            return Err(NodeError::InvalidInput(
                "legacy departure exceeds supported frame".into(),
            ));
        }
        Ok(PreparedLegacyMemberLeave {
            event,
            witness,
            recipients,
        })
    }

    /// The caller holds membership admission; WAL acceptance precedes the leave.
    pub(in crate::node) fn apply_legacy_member_leave(
        &self,
        group: ConversationId,
        own: &Announce,
    ) -> Result<Vec<PublicIdentity>, NodeError> {
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let (transaction, recipients, event_id) = {
            let log = self.log.lock().expect("log mutex");
            let projection =
                crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
            let prepared =
                self.prepare_legacy_member_leave_locked(group, &log, &projection, own)?;
            let event_id = prepared.event.id;
            let transaction = crate::node::delivery_store::DeliveryTransaction::LegacyMemberLeave {
                witness: prepared.witness,
                events: vec![prepared.event],
            };
            delivery
                .preflight_legacy_member_leave_transaction(&transaction, &log)
                .map_err(NodeError::Log)?;
            log.sync().map_err(NodeError::Log)?;
            (transaction, prepared.recipients, event_id)
        };
        delivery.begin(transaction).map_err(NodeError::Log)?;
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        drop(delivery);
        self.process_channel(group);
        if !self
            .account_group_projection(group)
            .authorized
            .contains(&event_id)
        {
            return Err(NodeError::Channel(
                "legacy departure was not admitted".into(),
            ));
        }
        Ok(recipients)
    }
}

#[cfg(test)]
pub(in crate::node) mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        eventlog::Event,
        identity::{account::Account, device::DeviceIdentity},
        node::{
            channel::ChannelBook,
            channel_account_authority::{AccountAuthorityControl, AccountGrant},
        },
    };
    pub(in crate::node) struct Fixture {
        pub group: ConversationId,
        pub owner: DeviceIdentity,
        pub owner_account: Account,
        pub member: DeviceIdentity,
        pub account: Account,
        pub proof: Announce,
        pub events: Vec<Event>,
    }
    impl Fixture {
        pub fn projection(&self) -> ChannelMembershipProjection {
            ChannelBook::project_membership(self.group, &self.events.iter().collect::<Vec<_>>())
        }
        pub fn witness(&self) -> AccountSelfControlWitness {
            let projection = self.projection();
            let grant = projection
                .authority
                .account_grant(&self.account.public().ed25519_pub)
                .unwrap()
                .id()
                .unwrap();
            AccountSelfControlWitness {
                own: self.proof.clone(),
                grant,
                parents: vec![self.events.last().unwrap().id],
                recipients: projection.state.unwrap().members().to_vec(),
            }
        }
    }
    pub(in crate::node) fn fixture(pending: bool) -> Fixture {
        let owner = DeviceIdentity::generate();
        let member = DeviceIdentity::generate();
        let owner_account = Account::generate();
        let account = Account::generate();
        let owner_proof = Announce::new_with_account(&owner, &owner_account, "owner", 1);
        let proof = Announce::new_with_account(&member, &account, "member", 1);
        let group = ConversationId::new([115; 32]);
        let meta = ChannelMeta {
            name: "account self controls".into(),
            members: vec![owner.public(), member.public()],
            epoch: 0,
            owner: owner.user_id(),
        };
        let genesis = Event::new(
            &owner,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            if pending {
                meta.encode_invitation()
            } else {
                meta.encode()
            },
        );
        let control = AccountAuthorityControl {
            version: 1,
            previous: None,
            issuer: owner_proof.clone(),
            owner_account: owner_account.public().ed25519_pub,
            grants: vec![
                AccountGrant {
                    incarnation: [1; 16],
                    pending: false,
                    original: owner_proof.clone(),
                    membership_event: genesis.id,
                    acceptance_event: None,
                },
                AccountGrant {
                    incarnation: [2; 16],
                    pending,
                    original: proof.clone(),
                    membership_event: genesis.id,
                    acceptance_event: None,
                },
            ],
            revoked: vec![],
            legacy_bindings: vec![owner_proof, proof.clone()],
            account_signature: vec![],
        };
        let authority = Event::new(
            &owner,
            group,
            2,
            vec![genesis.id],
            2,
            2,
            EventKind::ChannelAccountAuthority,
            control.signed_encode(group, &owner_account),
        );
        let fixture = Fixture {
            group,
            owner,
            owner_account,
            member,
            account,
            proof,
            events: vec![genesis, authority.clone()],
        };
        assert_eq!(
            fixture.projection().authority.current_event(),
            Some(authority.id),
            "a real validated modern grant is a fixture prerequisite"
        );
        fixture
    }
    #[test]
    fn accept_requires_pending_modern_account_grant() {
        let f = fixture(false);
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Accept,
                &f.witness()
            )
            .is_err()
        );
    }
    #[test]
    fn leave_rejects_pending_account_and_owner() {
        let f = fixture(true);
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Leave,
                &f.witness()
            )
            .is_err()
        );
        let f = fixture(false);
        let mut witness = f.witness();
        witness.own = Announce::new_with_account(&f.owner, &f.owner_account, "owner", 1);
        witness.grant = f
            .projection()
            .authority
            .account_grant(&f.owner_account.public().ed25519_pub)
            .unwrap()
            .id()
            .unwrap();
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Leave,
                &witness
            )
            .is_err()
        );
    }
    #[test]
    fn certificate_without_registered_full_source_cannot_accept() {
        let f = fixture(true);
        let mut witness = f.witness();
        witness.own =
            Announce::new_with_account(&DeviceIdentity::generate(), &f.account, "sibling", 1);
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Accept,
                &witness
            )
            .is_err()
        );
    }
    #[test]
    fn a_different_grant_incarnation_cannot_retarget_self_control() {
        let f = fixture(false);
        let mut witness = f.witness();
        witness.grant.incarnation = [9; 16];
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Leave,
                &witness
            )
            .is_err()
        );
    }
    #[test]
    fn self_control_rejects_legacy_authority_and_changed_original_recipients() {
        let f = fixture(false);
        let legacy = ChannelBook::project_membership(f.group, &[&f.events[0]]);
        assert!(
            expected_self_control(
                f.group,
                &legacy,
                AccountSelfControlInput::Leave,
                &f.witness()
            )
            .is_err()
        );
        let mut witness = f.witness();
        witness.recipients.clear();
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Leave,
                &witness
            )
            .is_err()
        );
    }
    #[test]
    fn valid_modern_accept_and_member_leave_preserve_original_recipients() {
        for (pending, input, kind) in [
            (
                true,
                AccountSelfControlInput::Accept,
                EventKind::ChannelAccept,
            ),
            (
                false,
                AccountSelfControlInput::Leave,
                EventKind::ChannelLeave,
            ),
        ] {
            let f = fixture(pending);
            let witness = f.witness();
            let (actual, recipients) =
                expected_self_control(f.group, &f.projection(), input, &witness).unwrap();
            assert_eq!(actual, kind);
            assert_eq!(recipients, witness.recipients);
            assert!(recipients.contains(&f.member.public()));
        }
    }
    fn direct_node(
        dir: &std::path::Path,
        identity: DeviceIdentity,
        account: Account,
    ) -> std::sync::Arc<Node> {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channels, _) = tokio::sync::mpsc::unbounded_channel();
        let (files, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open_with_account(
            identity,
            account,
            std::sync::Arc::new(std::sync::Mutex::new(crate::discovery::Roster::default())),
            dm,
            channels,
            files,
            &dir.join("events"),
            &dir.join("sent"),
            "pw",
        )
        .unwrap()
    }
    #[tokio::test]
    async fn direct_modern_self_control_uses_the_same_wal_and_survives_reopen() {
        for pending in [false, true] {
            let f = fixture(pending);
            let group = f.group;
            let own = f.proof.clone();
            let (ed, x) = f.member.secret_bytes();
            let account_secret = f.account.secret_bytes();
            let dir = tempfile::tempdir().unwrap();
            let node = direct_node(dir.path(), f.member, f.account);
            for parent in f.events {
                node.log.lock().unwrap().append_durable(parent).unwrap();
            }
            node.process_channel(group);
            let input = if pending {
                AccountSelfControlInput::Accept
            } else {
                AccountSelfControlInput::Leave
            };
            if pending {
                node.accept_channel_invite(group).await.unwrap();
            } else {
                node.leave_channel(group).await.unwrap();
            }
            let effects = node
                .log
                .lock()
                .unwrap()
                .events(&group)
                .into_iter()
                .filter(|event| event.kind == input.event_kind())
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(effects.len(), 1);
            assert!(
                node.delivery
                    .lock()
                    .unwrap()
                    .pending_transactions()
                    .is_empty()
            );
            drop(node);
            let node = direct_node(
                dir.path(),
                DeviceIdentity::from_secret_bytes(ed, x),
                Account::from_secret_bytes(account_secret),
            );
            assert_eq!(
                node.log
                    .lock()
                    .unwrap()
                    .events(&group)
                    .into_iter()
                    .filter(|event| event.kind == input.event_kind())
                    .map(|event| event.id)
                    .collect::<Vec<_>>(),
                vec![effects[0].id]
            );
            let projection = node.account_group_projection(group);
            assert_eq!(projection.authority.is_member(&own.public(), &own), pending);
        }
    }
    #[tokio::test]
    async fn direct_owner_leave_is_rejected_before_any_public_effect_or_wal() {
        let f = fixture(false);
        let group = f.group;
        let dir = tempfile::tempdir().unwrap();
        let node = direct_node(dir.path(), f.owner, f.owner_account);
        for parent in f.events {
            node.log.lock().unwrap().append_durable(parent).unwrap();
        }
        node.process_channel(group);
        let result = node.leave_channel(group).await;
        assert!(result.is_err());
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        assert!(
            !node
                .log
                .lock()
                .unwrap()
                .events(&group)
                .into_iter()
                .any(|event| event.kind == EventKind::ChannelLeave)
        );
    }
    #[tokio::test]
    async fn legacy_direct_departure_uses_exact_device_wal_with_owner_offline() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let member = crate::identity::device::DeviceIdentity::generate();
        let offline_owner = crate::identity::device::DeviceIdentity::generate();
        let node = direct_node(dir.path(), member, account);
        let group = crate::channel::new_channel_id();
        let genesis = Event::new(
            &offline_owner,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            crate::channel::ChannelMeta {
                name: "offline legacy owner".into(),
                members: vec![offline_owner.public(), node.identity.public()],
                epoch: 0,
                owner: offline_owner.public().user_id(),
            }
            .encode(),
        );
        node.log.lock().unwrap().append_durable(genesis).unwrap();
        node.process_channel(group);
        let impostor = crate::identity::device::DeviceIdentity::generate();
        let other_proof = Announce::new_with_account(&impostor, &node.account, "other device", 1);
        {
            let log = node.log.lock().unwrap();
            let projection =
                crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
            assert!(
                node.prepare_legacy_member_leave_locked(group, &log, &projection, &other_proof)
                    .is_err()
            );
            let prepared = node
                .prepare_legacy_member_leave_locked(
                    group,
                    &log,
                    &projection,
                    &node.own_group_proof(),
                )
                .unwrap();
            assert_eq!(prepared.event.kind, EventKind::ChannelLeave);
            assert!(prepared.event.ciphertext.is_empty());
            assert_eq!(prepared.witness.lifetime.member, node.identity.public());
        }
        assert!(node.group_control_heads(group).is_empty());
        node.leave_channel(group).await.unwrap();
        assert_eq!(
            node.account_group_access(group).unwrap().role,
            AccountGroupRole::Left,
            "signed legacy departure proves Left before any private head exists"
        );
        node.publish_account_group_access_for(group);
        assert_eq!(
            node.account_group_access(group).unwrap().role,
            AccountGroupRole::Left
        );
        let events = node
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::ChannelLeave)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].ciphertext.is_empty()
                && events[0].verify_signature()
                && events[0].verify_integrity()
        );
        assert_eq!(
            events[0].author.ed25519_pub(),
            &node.identity.public().ed25519_pub
        );
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        assert!(node.leave_channel(group).await.is_err());
        let secret = node.identity.secret_bytes();
        let account = Account::from_secret_bytes(node.account.secret_bytes());
        drop(node);
        let reopened = direct_node(
            dir.path(),
            crate::identity::device::DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            account,
        );
        assert_eq!(
            reopened.account_group_access(group).unwrap().role,
            AccountGroupRole::Left,
            "legacy departure provenance survives reopening the original device"
        );
    }

    async fn assert_local_account_self_control(pending: bool) {
        let f = fixture(pending);
        let account_id = f.account.account_id();
        let group = f.group;
        let dir = tempfile::tempdir().unwrap();
        let node = direct_node(dir.path(), f.member, f.account);
        for parent in f.events {
            node.log.lock().unwrap().append_durable(parent).unwrap();
        }
        node.process_channel(group);
        let operation = if pending {
            AccountGroupOperation::AcceptInvitation
        } else {
            AccountGroupOperation::LeaveAccount
        };
        let kind = if pending {
            EventKind::ChannelAccept
        } else {
            EventKind::ChannelLeave
        };
        let id = node.new_account_group_operation_id(&account_id).unwrap();
        let admitted = node
            .submit_account_group_operation(&account_id, id, group, operation.clone())
            .expect(
                "a modern account's self control must enter the original account-operation facade",
            );
        assert!(admitted.committed_events.is_empty());
        node.maintain_account_group_operations().await;
        let status = node
            .account_group_operation_status(&account_id, id)
            .unwrap()
            .unwrap();
        assert_eq!(
            status.phase,
            crate::node::AccountGroupOperationPhase::Committed
        );
        let effects: Vec<_> = node
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == kind)
            .cloned()
            .collect();
        assert_eq!(effects.len(), 1);
        assert!(effects[0].ciphertext.is_empty());
        assert!(effects[0].verify_integrity() && effects[0].verify_signature());
        assert_eq!(
            effects[0].author.ed25519_pub(),
            &node.identity.public().ed25519_pub
        );
        assert_eq!(status.committed_events, vec![effects[0].id]);
        assert_eq!(
            node.account_group_access(group).unwrap().role,
            if pending {
                AccountGroupRole::Member
            } else {
                AccountGroupRole::Left
            }
        );
        assert_eq!(
            node.submit_account_group_operation(&account_id, id, group, operation)
                .unwrap(),
            status
        );
        node.maintain_account_group_operations().await;
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .events(&group)
                .into_iter()
                .filter(|event| event.kind == kind)
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn local_modern_accept_facade_keeps_original_empty_event_and_intent_id() {
        assert_local_account_self_control(true).await;
    }
    #[tokio::test]
    async fn local_modern_leave_facade_keeps_original_empty_event_and_intent_id() {
        assert_local_account_self_control(false).await;
    }
    #[tokio::test]
    async fn modern_self_control_reopen_finishes_original_intent_after_terminal_write_failure() {
        for pending in [false, true] {
            let f = fixture(pending);
            let group = f.group;
            let account_id = f.account.account_id();
            let account_secret = f.account.secret_bytes();
            let (ed, x) = f.member.secret_bytes();
            let dir = tempfile::tempdir().unwrap();
            let node = direct_node(dir.path(), f.member, f.account);
            for parent in f.events {
                node.log.lock().unwrap().append_durable(parent).unwrap();
            }
            node.process_channel(group);
            let operation = if pending {
                AccountGroupOperation::AcceptInvitation
            } else {
                AccountGroupOperation::LeaveAccount
            };
            let kind = if pending {
                EventKind::ChannelAccept
            } else {
                EventKind::ChannelLeave
            };
            let id = node.new_account_group_operation_id(&account_id).unwrap();
            node.submit_account_group_operation(&account_id, id, group, operation.clone())
                .unwrap();
            assert!(
                node.interrupt_account_group_completion_for_test(group, id)
                    .is_err()
            );
            let effects: Vec<_> = node
                .log
                .lock()
                .unwrap()
                .events(&group)
                .into_iter()
                .filter(|event| event.kind == kind)
                .cloned()
                .collect();
            assert_eq!(
                effects.len(),
                1,
                "the exact original effect is durable before terminal publication fails"
            );
            assert_eq!(
                node.delivery.lock().unwrap().pending_transactions().len(),
                1
            );
            node.fail_account_group_operation_journal_appends_for_test(false)
                .unwrap();
            drop(node);
            let node = direct_node(
                dir.path(),
                DeviceIdentity::from_secret_bytes(ed, x),
                Account::from_secret_bytes(account_secret),
            );
            node.maintain_account_group_operations().await;
            let status = node
                .account_group_operation_status(&account_id, id)
                .unwrap()
                .unwrap();
            assert_eq!(
                status.phase,
                crate::node::AccountGroupOperationPhase::Committed
            );
            assert_eq!(status.committed_events, vec![effects[0].id]);
            assert!(
                node.delivery
                    .lock()
                    .unwrap()
                    .pending_transactions()
                    .is_empty()
            );
            assert_eq!(
                node.submit_account_group_operation(&account_id, id, group, operation)
                    .unwrap(),
                status
            );
            node.maintain_account_group_operations().await;
            assert_eq!(
                node.log
                    .lock()
                    .unwrap()
                    .events(&group)
                    .into_iter()
                    .filter(|event| event.kind == kind)
                    .map(|event| event.id)
                    .collect::<Vec<_>>(),
                vec![effects[0].id]
            );
        }
    }
    async fn wait_self_control_noise(mut ready: impl FnMut() -> bool, message: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while !ready() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect(message);
    }
    async fn reference_self_control_over_noise(input: AccountSelfControlInput) {
        let pending = input != AccountSelfControlInput::Leave;
        let accepted_after = input == AccountSelfControlInput::Accept;
        use crate::node::{AccountDevice, AccountGroupOperationPhase, SyncCategory};
        let f = fixture(pending);
        let dir = tempfile::tempdir().unwrap();
        let group = f.group;
        let grant = f.witness().grant;
        let shared = Account::from_secret_bytes(f.account.secret_bytes());
        let source = direct_node(&dir.path().join("source"), f.member, f.account);
        let owner = direct_node(&dir.path().join("owner"), f.owner, f.owner_account);
        let reference_identity = DeviceIdentity::generate();
        let reference_secrets = reference_identity.secret_bytes();
        let reference = direct_node(
            &dir.path().join("reference"),
            reference_identity,
            Account::from_secret_bytes(shared.secret_bytes()),
        );
        let extra = (!pending).then(|| {
            direct_node(
                &dir.path().join("extra"),
                DeviceIdentity::generate(),
                Account::from_secret_bytes(shared.secret_bytes()),
            )
        });
        let mut events = f.events;
        if let Some(extra) = &extra {
            let authority = events.last().unwrap().id;
            let inheritance = crate::node::channel_account_bootstrap::DeviceControl::Inherit {
                authority,
                grant,
                device: extra.own_group_proof(),
                account_signature: shared
                    .sign(
                        &crate::node::channel_account_bootstrap::inheritance_account_message(
                            group,
                            authority,
                            grant,
                            &extra.identity.public(),
                        ),
                    )
                    .to_vec(),
            };
            events.push(Event::new(
                &extra.identity,
                group,
                1,
                vec![authority],
                3,
                3,
                EventKind::ChannelDeviceInheritance,
                inheritance.encode(),
            ));
        }
        for node in std::iter::once(&source)
            .chain(std::iter::once(&owner))
            .chain(extra.iter())
        {
            for event in &events {
                node.log
                    .lock()
                    .unwrap()
                    .append_durable(event.clone())
                    .unwrap();
            }
            node.process_channel(group);
        }
        assert!(
            reference.channel_members(group).is_empty(),
            "the requester begins with no manufactured public membership"
        );
        let mut nodes = vec![
            ("source", source.clone()),
            ("reference", reference.clone()),
            ("owner", owner.clone()),
        ];
        if let Some(extra) = &extra {
            nodes.push(("extra", extra.clone()));
        }
        let trace = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let trace_started = std::time::Instant::now();
        let peer_labels: std::collections::HashMap<_, _> = nodes
            .iter()
            .map(|(label, node)| (node.identity.public().user_id(), *label))
            .collect();
        for &(label, ref node) in &nodes {
            let trace = trace.clone();
            let peer_labels = peer_labels.clone();
            *node.account_session_test_hook.lock().unwrap() = Some(std::sync::Arc::new(
                move |peer, frame| {
                    let trace = trace.clone();
                    let peer_label = peer_labels
                        .get(&peer.user_id())
                        .copied()
                        .unwrap_or("unknown");
                    Box::pin(async move {
                        use crate::node::account_sync_wire::AccountHistoryFrame;
                        let key_kind = |key: &crate::node::AccountDocumentKey| match key {
                            crate::node::AccountDocumentKey::GroupOperation { .. } => "operation",
                            crate::node::AccountDocumentKey::GroupControl { .. } => "control",
                            crate::node::AccountDocumentKey::GroupInventory { .. } => "inventory",
                            _ => "other",
                        };
                        let event = match frame {
                            AccountHistoryFrame::DocumentsRequest { body } => {
                                crate::node::account_documents::wire::DocumentRequest::decode(&body)
                                    .map(|request| {
                                        format!(
                                            "request from={peer_label} v={} domain={:?} after={} zero={} probe={}",
                                            request.version,
                                            request.domain,
                                            request.after.as_ref().map_or("none", key_kind),
                                            request.known_digest == [0; 32],
                                            request.operation.is_some(),
                                        )
                                    })
                            }
                            AccountHistoryFrame::DocumentHeader { body } => {
                                serde_json::from_slice::<crate::node::AccountDocumentMeta>(&body)
                                    .ok()
                                    .map(|meta| {
                                        format!(
                                            "header to={peer_label} kind={} revision={}",
                                            key_kind(&meta.key),
                                            meta.revision
                                        )
                                    })
                            }
                            _ => None,
                        };
                        if let Some(event) = event {
                            let mut trace = trace.lock().unwrap();
                            if trace.len() >= 128 {
                                trace.pop_front();
                            }
                            trace.push_back((trace_started.elapsed().as_millis(), label, event));
                        }
                    })
                },
            ));
        }
        let siblings: Vec<_> = nodes
            .iter()
            .filter(|(_, node)| node.account_id() == shared.account_id())
            .map(|(_, node)| node.clone())
            .collect();
        for left in &siblings {
            for right in &siblings {
                if left.user_id() != right.user_id() {
                    left.account_devices
                        .lock()
                        .unwrap()
                        .insert(AccountDevice {
                            public: right.identity.public(),
                            certificate: shared.certify(&right.identity.public().ed25519_pub),
                        })
                        .unwrap();
                }
            }
        }
        let mut listeners = Vec::new();
        let mut addresses = Vec::new();
        for (name, node) in &nodes {
            node.set_device_sync_category(SyncCategory::History, false)
                .unwrap();
            // This regression isolates mandatory account membership/control
            // reconciliation from optional public-profile archive traffic.
            node.set_device_sync_category(SyncCategory::Profile, false)
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            node.configure_privacy(
                &dir.path().join(name),
                "pw",
                &node.signed_announce("Self control fixture", address.port()),
                std::sync::Arc::new(crate::discovery::DiscoveryVisibility::default()),
            )
            .unwrap();
            listeners.push(listener);
            addresses.push(address);
        }
        for (_, left) in &nodes {
            for ((_, right), address) in nodes.iter().zip(&addresses) {
                if left.user_id() != right.user_id()
                    && left.user_id() != reference.user_id()
                    && right.user_id() != reference.user_id()
                {
                    left.roster.lock().unwrap().update(
                        &right.signed_announce("Self control peer", address.port()),
                        address.ip(),
                        &left.user_id(),
                    );
                }
            }
        }
        source.publish_account_group_access_for(group);
        if let Some(extra) = &extra {
            extra.publish_account_group_access_for(group);
        }
        let mut handles = Vec::new();
        let mut reference_listener = None;
        for (index, ((_, node), listener)) in nodes.iter().zip(listeners).enumerate() {
            if index == 1 {
                reference_listener = Some(listener);
                handles.push(None);
            } else {
                handles.push(Some(tokio::spawn(node.clone().run_accept_loop(listener))));
            }
        }
        let archive_source = if pending { &owner } else { &source };
        archive_source
            .send_channel_message(group, b"unrelated retained archive")
            .await
            .unwrap();
        let archive = archive_source
            .history(group, 100)
            .into_iter()
            .find(|row| row.text == b"unrelated retained archive")
            .unwrap()
            .id;
        if !pending {
            wait_self_control_noise(|| nodes.iter().filter(|(_, node)| node.user_id() != reference.user_id())
                .all(|(_, node)| node.history(group, 100).iter().any(|row| row.id == archive)),
                "the archive marker must be fully retained by physical sources before reference enrollment").await;
        }
        for (_, left) in &nodes {
            for ((_, right), address) in nodes.iter().zip(&addresses) {
                if left.user_id() != right.user_id() {
                    left.roster.lock().unwrap().update(
                        &right.signed_announce("Self control peer", address.port()),
                        address.ip(),
                        &left.user_id(),
                    );
                }
            }
        }
        handles[1] = Some(tokio::spawn(
            reference
                .clone()
                .run_accept_loop(reference_listener.take().unwrap()),
        ));
        let (online, offline_index) = match &extra {
            Some(extra) if extra.user_id() < source.user_id() => (extra.clone(), Some(0)),
            Some(_) => (source.clone(), Some(3)),
            None => (source.clone(), None),
        };
        let offline = offline_index.map(|index| nodes[index].1.clone());
        let offline_public = offline.as_ref().map(|node| node.identity.public());
        let kind = input.operation().kind();
        wait_self_control_noise(|| {
            let role_ready = reference.account_group_access(group).unwrap().role == if pending { AccountGroupRole::Invited } else { AccountGroupRole::Member };
            let capability_ready = reference.group_operation_versions.lock().unwrap().get(&online.user_id()).is_some_and(|support| support.public == online.identity.public() && support.kinds & kind.capability_bit() != 0);
            let reverse_ready = online.group_operation_versions.lock().unwrap().get(&reference.user_id()).is_some_and(|support| support.public == reference.identity.public());
            let old_head_ready = offline_public.as_ref().is_none_or(|public| [&reference, &online].iter().all(|node| node.group_control_heads(group).iter().any(|head| head.proof.public() == *public && head.access.observation.role == AccountGroupRole::Member)));
            role_ready && capability_ready && reverse_ready && old_head_ready
        }, "real private role/capability and old Member provenance must arrive before self-control admission").await;
        if let Some(index) = offline_index {
            let offline = offline.as_ref().unwrap();
            offline.runtime_work.close();
            let handle = handles[index].take().unwrap();
            handle.abort();
            let _ = handle.await;
            offline.sessions.clear();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                offline.runtime_work.drain(),
            )
            .await
            .unwrap();
            // This scenario retains authenticated stale membership provenance,
            // not a live discovery advertisement for the retired participant.
            // Withdraw only its route after retirement; signed heads and the
            // certified account directory must survive the simulated outage.
            let offline_public = offline.identity.public();
            for (_, node) in &nodes {
                if node.user_id() != offline.user_id() {
                    node.roster.lock().unwrap().forget(&offline_public);
                }
            }
            for node in [&reference, &online] {
                assert!(
                    node.linked_devices().iter().any(|device| {
                        device.public == offline_public
                            && device.certificate.verify()
                            && device.certificate.account_id() == shared.account_id()
                    }),
                    "route withdrawal must preserve the offline source's certified identity"
                );
                assert!(
                    node.group_control_heads(group).iter().any(|head| {
                        head.proof.public() == offline_public
                            && head.valid(&shared.public().ed25519_pub)
                            && head.access.observation.role == AccountGroupRole::Member
                    }),
                    "route withdrawal must retain the authentic stale Member head"
                );
            }
        }
        let executor_label = nodes
            .iter()
            .find(|(_, node)| node.user_id() == online.user_id())
            .unwrap()
            .0;
        let fanout_trace = trace.clone();
        *online.control_fanout_start_test_hook.lock().unwrap() = Some(Box::new(move || {
            fanout_trace.lock().unwrap().push_back((
                trace_started.elapsed().as_millis(),
                executor_label,
                "fanout_started".to_owned(),
            ));
        }));
        trace.lock().unwrap().push_back((
            trace_started.elapsed().as_millis(),
            "reference",
            "admission_started".to_owned(),
        ));
        let operation = input.operation();
        let id = reference
            .new_account_group_operation_id(&shared.account_id())
            .unwrap();
        let admission_started = std::time::Instant::now();
        let admitted = reference
            .submit_account_group_operation(&shared.account_id(), id, group, operation.clone())
            .unwrap();
        trace.lock().unwrap().push_back((
            trace_started.elapsed().as_millis(),
            "reference",
            "admission_returned".to_owned(),
        ));
        assert_eq!(admitted.executions.len(), 1);
        let executor = nodes
            .iter()
            .find(|(_, node)| node.user_id() == admitted.executions[0].device_id)
            .unwrap()
            .1
            .clone();
        assert_eq!(executor.account_id(), shared.account_id());
        assert!(
            !executor.runtime_work.is_closed(),
            "an available canonical source must be pinned"
        );
        let full_source = executor.identity.public();
        let canonical = Node::public_account_group_source(
            group,
            &executor.account_group_projection(group),
            &full_source,
            &executor.own_group_proof(),
        )
        .unwrap();
        assert_eq!(
            canonical.role,
            if pending {
                AccountGroupRole::Invited
            } else {
                AccountGroupRole::Member
            }
        );
        let event_kind = input.event_kind();
        let convergence = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            let mut previous_phase = None;
            loop {
                let phase = executor
                    .account_group_operation_status(&shared.account_id(), id)
                    .unwrap()
                    .map(|status| status.phase);
                if phase != previous_phase {
                    trace.lock().unwrap().push_back((
                        trace_started.elapsed().as_millis(),
                        executor_label,
                        format!(
                            "executor_phase={phase:?} since_admission_ms={}",
                            admission_started.elapsed().as_millis()
                        ),
                    ));
                    previous_phase = phase;
                }
                let ready = reference
                    .account_group_operation_status(&shared.account_id(), id)
                    .unwrap()
                    .is_some_and(|status| {
                        status.phase == AccountGroupOperationPhase::Committed
                            && status.committed_events.len() == 1
                            && status
                                .committed_events
                                .iter()
                                .all(|event| owner.log.lock().unwrap().get(event).is_some())
                    })
                    && reference.account_group_access(group).unwrap().role
                        == if accepted_after {
                            AccountGroupRole::Member
                        } else {
                            AccountGroupRole::Left
                        }
                    && executor.account_group_access(group).unwrap().role
                        == if accepted_after {
                            AccountGroupRole::Member
                        } else {
                            AccountGroupRole::Left
                        };
                if ready {
                    trace.lock().unwrap().push_back((
                        trace_started.elapsed().as_millis(),
                        "reference",
                        format!(
                            "converged since_admission_ms={}",
                            admission_started.elapsed().as_millis()
                        ),
                    ));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        if convergence.is_err() {
            let diagnostics: Vec<_> = nodes.iter().map(|(label, node)| {
                let status = node.account_group_operation_status(&shared.account_id(), id).ok().flatten();
                let phase = status.as_ref().map(|status| &status.phase);
                let committed_count = status.as_ref().map_or(0, |status| status.committed_events.len());
                let effects_local = status.as_ref().is_some_and(|status| !status.committed_events.is_empty() && status.committed_events.iter().all(|event| node.log.lock().unwrap().get(event).is_some()));
                let effects_external = status.as_ref().is_some_and(|status| !status.committed_events.is_empty() && status.committed_events.iter().all(|event| owner.log.lock().unwrap().get(event).is_some()));
                let effects = node.log.lock().unwrap().events(&group).into_iter()
                    .filter(|event| event.kind == event_kind).count();
                let role = node.account_group_access(group).map(|access| access.role);
                let source_role = Node::public_account_group_source(group, &node.account_group_projection(group), &full_source, &executor.own_group_proof()).map(|source| source.role);
                let sync: Vec<_> = node.device_sync_status().into_iter().map(|status|
                    (status.phase, status.document_phase, status.document_requires_upgrade, status.document_error)).collect();
                format!("node={label} phase={phase:?} role={role:?} source_role={source_role:?} effects={effects} committed_count={committed_count} effects_local={effects_local} effects_external={effects_external} executor={} runtime_closed={} fanout_sequence={:?} sync={sync:?}", node.user_id() == executor.user_id(), node.runtime_work.is_closed(), node.account_group_fanout_sequence(group))
            }).collect();
            panic!(
                "self control Noise did not converge: input={input:?} pending={pending} nodes={diagnostics:?} trace={:?}",
                trace.lock().unwrap()
            );
        }
        eprintln!(
            "self control Noise timing: input={input:?} trace={:?}",
            trace
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, event)| {
                    event.starts_with("executor_phase=") || event.starts_with("converged ")
                })
                .collect::<Vec<_>>()
        );
        let status = reference
            .account_group_operation_status(&shared.account_id(), id)
            .unwrap()
            .unwrap();
        assert_eq!(
            status.primary_event, None,
            "public controls have no primary message event"
        );
        assert_eq!(status.committed_events.len(), 1);
        let event = owner
            .log
            .lock()
            .unwrap()
            .get(&status.committed_events[0])
            .unwrap()
            .clone();
        assert_eq!(event.kind, event_kind);
        assert!(
            event.ciphertext == input.event_payload(grant)
                && event.verify_integrity()
                && event.verify_signature()
        );
        if input == AccountSelfControlInput::Decline {
            assert!(
                owner
                    .log
                    .lock()
                    .unwrap()
                    .events(&group)
                    .iter()
                    .all(|event| event.kind != EventKind::ChannelAccept),
                "explicit decline must never manufacture acceptance before removing the invitation"
            );
            assert!(
                owner
                    .account_group_projection(group)
                    .authority
                    .account_grant(&shared.public().ed25519_pub)
                    .is_none()
            );
        }
        assert_eq!(event.author.ed25519_pub(), &full_source.ed25519_pub);
        assert_eq!(status.committed_events, vec![event.id]);
        if !accepted_after {
            for node in [&reference, &executor] {
                let authored_messages = || {
                    node.log
                        .lock()
                        .unwrap()
                        .events(&group)
                        .into_iter()
                        .filter(|event| {
                            event.kind == EventKind::Message
                                && event.author.ed25519_pub() == &node.identity.public().ed25519_pub
                        })
                        .map(|event| event.id)
                        .collect::<Vec<_>>()
                };
                let before = authored_messages();
                assert!(
                    node.enqueue_channel_message_reply_expiring_if(
                        group,
                        b"forbidden after account departure",
                        None,
                        None,
                        |commit| commit(),
                    )
                    .is_err(),
                    "a Left account must reject direct public enqueue before allocation"
                );
                assert!(
                    node.send_channel_message(group, b"forbidden after account departure")
                        .await
                        .is_err(),
                    "a Left account must reject direct public send even with a stale local public view"
                );
                assert_eq!(
                    authored_messages(),
                    before,
                    "rejected direct SDK calls must not allocate a public message effect"
                );
            }
        }
        assert_eq!(
            owner
                .account_group_projection(group)
                .authority
                .is_member(&full_source, &executor.own_group_proof()),
            accepted_after
        );
        assert_eq!(
            reference
                .submit_account_group_operation(&shared.account_id(), id, group, operation.clone())
                .unwrap(),
            status
        );
        assert!(
            !reference
                .history(group, 100)
                .iter()
                .any(|row| row.id == archive),
            "explicit self control must not grant optional archive history"
        );
        for (_, node) in &nodes {
            node.runtime_work.close();
        }
        for handle in handles.into_iter().flatten() {
            handle.abort();
            let _ = handle.await;
        }
        for (_, node) in &nodes {
            node.sessions.clear();
            tokio::time::timeout(std::time::Duration::from_secs(5), node.runtime_work.drain())
                .await
                .unwrap();
        }
        drop((
            siblings, executor, offline, online, nodes, source, owner, reference, extra,
        ));
        let reference = direct_node(
            &dir.path().join("reference"),
            DeviceIdentity::from_secret_bytes(reference_secrets.0, reference_secrets.1),
            Account::from_secret_bytes(shared.secret_bytes()),
        );
        assert_eq!(
            reference.account_group_access(group).unwrap().role,
            if accepted_after {
                AccountGroupRole::Member
            } else {
                AccountGroupRole::Left
            }
        );
        assert_eq!(
            reference
                .submit_account_group_operation(&shared.account_id(), id, group, operation)
                .unwrap(),
            status
        );
        if let Some(public) = offline_public {
            assert!(
                reference
                    .group_control_heads(group)
                    .iter()
                    .any(|head| head.proof.public() == public
                        && head.access.observation.role == AccountGroupRole::Member),
                "the authentic old private Member head must remain present across reopen"
            );
            assert_eq!(
                reference.account_group_access(group).unwrap().role,
                AccountGroupRole::Left,
                "retained older Member provenance cannot resurrect a revoked account grant"
            );
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reference_modern_accept_uses_noise_and_keeps_optional_archive_off() {
        reference_self_control_over_noise(AccountSelfControlInput::Accept).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reference_modern_leave_keeps_left_over_old_member_head_and_reopen() {
        reference_self_control_over_noise(AccountSelfControlInput::Leave).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reference_modern_decline_uses_noise_and_keeps_left_without_acceptance_or_archive() {
        reference_self_control_over_noise(AccountSelfControlInput::Decline).await;
    }

    fn decline_payload(grant: AccountGrantId) -> Vec<u8> {
        crate::node::channel_account_authority::AccountDeclinePayload { version: 1, grant }.encode()
    }

    fn decline_event(f: &Fixture, grant: AccountGrantId) -> Event {
        Event::new(
            &f.member,
            f.group,
            1,
            vec![f.events.last().unwrap().id],
            3,
            3,
            EventKind::ChannelLeave,
            decline_payload(grant),
        )
    }

    #[test]
    fn explicit_decline_exact_pending_incarnation_removes_invitation_without_accept_or_keys() {
        let mut f = fixture(true);
        let grant = f.witness().grant;
        let decline = decline_event(&f, grant);
        assert!(decline.verify_integrity() && decline.verify_signature());
        assert!(!decline.ciphertext.is_empty());
        f.events.push(decline);
        let projection = f.projection();
        assert!(
            projection
                .authority
                .account_grant(&f.account.public().ed25519_pub)
                .is_none(),
            "an explicit valid decline must revoke exactly its pending invitation incarnation"
        );
        assert!(
            !projection
                .state
                .unwrap()
                .members()
                .contains(&f.member.public())
        );
        assert!(
            f.events.iter().all(|event| !matches!(
                event.kind,
                EventKind::ChannelAccept | EventKind::KeyRotation
            )),
            "decline must never synthesize acceptance or release content keys"
        );
    }

    fn concurrent_decline_preserves_key_epoch_barrier(decline_first: bool) {
        use crate::node::channel_account_bootstrap::{DeviceControl, inheritance_account_message};
        let mut f = fixture(true);
        let grant = f.witness().grant;
        let authority = f.events.last().unwrap().id;
        let sibling = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&sibling, &f.account, "pending sibling", 1);
        let inherit = DeviceControl::Inherit {
            authority,
            grant,
            device: proof,
            account_signature: f
                .account
                .sign(&inheritance_account_message(
                    f.group,
                    authority,
                    grant,
                    &sibling.public(),
                ))
                .to_vec(),
        };
        let inheritance = Event::new(
            &sibling,
            f.group,
            1,
            vec![authority],
            3,
            3,
            EventKind::ChannelDeviceInheritance,
            inherit.encode(),
        );
        f.events.push(inheritance.clone());
        assert!(
            f.projection()
                .authority
                .invitation_for_device(
                    &sibling.public(),
                    &Announce::new_with_account(&sibling, &f.account, "pending sibling", 1)
                )
                .is_some(),
            "the second actor must have a real validated invitation inheritance"
        );
        // Both actors own independent valid per-device chains and see the same
        // pending grant. Equal Lamport + signed IDs select both canonical orders.
        let (accept, decline) = (0..128)
            .find_map(|trial| {
                let accept = Event::new(
                    &f.member,
                    f.group,
                    1,
                    vec![inheritance.id],
                    4,
                    10 + trial * 2,
                    EventKind::ChannelAccept,
                    vec![],
                );
                let decline = Event::new(
                    &sibling,
                    f.group,
                    2,
                    vec![inheritance.id],
                    4,
                    11 + trial * 2,
                    EventKind::ChannelLeave,
                    decline_payload(grant),
                );
                ((decline.id < accept.id) == decline_first).then_some((accept, decline))
            })
            .expect("independent signed IDs must provide the selected canonical order");
        f.events.push(accept.clone());
        let accepted = f.projection();
        assert!(accepted.authority.is_member(&f.member.public(), &f.proof));
        let epoch_that_may_have_released_keys = accepted.state.unwrap().epoch();
        f.events.push(decline.clone());
        let order =
            crate::node::timeline::ordered_ids(&f.events.iter().collect::<Vec<_>>(), &[], true);
        assert_eq!(
            order.iter().position(|id| *id == decline.id).unwrap()
                < order.iter().position(|id| *id == accept.id).unwrap(),
            decline_first
        );
        let projection = f.projection();
        let final_epoch = projection.state.as_ref().unwrap().epoch();
        assert!(
            final_epoch > epoch_that_may_have_released_keys,
            "concurrent decline must cross the epoch that a partial Accept view may already have exposed; accept epoch={epoch_that_may_have_released_keys}, final epoch={final_epoch}, decline_first={decline_first}"
        );
        assert!(
            projection
                .authority
                .account_grant(&f.account.public().ed25519_pub)
                .is_none(),
            "neither canonical order may resurrect the declined invitation"
        );
        assert!(
            !projection
                .state
                .unwrap()
                .recipient_members()
                .contains(&f.member.public())
        );
    }

    #[test]
    fn concurrent_decline_before_accept_cannot_reuse_a_partial_accept_key_epoch() {
        concurrent_decline_preserves_key_epoch_barrier(true);
    }

    #[test]
    fn concurrent_accept_before_decline_cannot_reuse_a_partial_accept_key_epoch() {
        concurrent_decline_preserves_key_epoch_barrier(false);
    }

    #[test]
    fn explicit_decline_payload_is_versioned_and_old_empty_actions_stay_distinct() {
        use crate::node::channel_account_authority::AccountDeclinePayload;
        let f = fixture(true);
        let grant = f.witness().grant;
        let payload = decline_payload(grant);
        assert_eq!(
            AccountDeclinePayload::decode(&payload).unwrap().grant,
            grant
        );
        assert!(AccountDeclinePayload::decode(&[]).is_none());
        assert!(AccountDeclinePayload::decode(&[payload.clone(), vec![0]].concat()).is_none());
        assert!(
            AccountDeclinePayload::decode(&AccountDeclinePayload { version: 2, grant }.encode())
                .is_none()
        );
        let empty = Event::new(
            &f.member,
            f.group,
            1,
            vec![f.events.last().unwrap().id],
            3,
            3,
            EventKind::ChannelLeave,
            vec![],
        );
        let mut events = f.events.clone();
        events.push(empty);
        let projection =
            ChannelBook::project_membership(f.group, &events.iter().collect::<Vec<_>>());
        assert!(
            projection
                .authority
                .invitation_for_device(&f.member.public(), &f.proof)
                .is_some(),
            "old empty accepted-member leave remains invalid for a pending invitation"
        );
    }

    #[test]
    fn decline_rejects_observed_acceptance_wrong_incarnation_actor_and_malformed_payload() {
        for case in 0..6 {
            let mut f = fixture(true);
            let grant = f.witness().grant;
            let mut event = decline_event(&f, grant);
            match case {
                0 => {
                    let accept = Event::new(
                        &f.member,
                        f.group,
                        1,
                        vec![f.events.last().unwrap().id],
                        3,
                        3,
                        EventKind::ChannelAccept,
                        vec![],
                    );
                    event = Event::new(
                        &f.member,
                        f.group,
                        2,
                        vec![accept.id],
                        4,
                        4,
                        EventKind::ChannelLeave,
                        decline_payload(grant),
                    );
                    f.events.push(accept);
                }
                1 => {
                    event = decline_event(
                        &f,
                        AccountGrantId {
                            incarnation: [99; 16],
                            ..grant
                        },
                    )
                }
                2 => {
                    event = decline_event(
                        &f,
                        AccountGrantId {
                            account_pub: Account::generate().public().ed25519_pub,
                            ..grant
                        },
                    )
                }
                3 => {
                    event = Event::new(
                        &DeviceIdentity::generate(),
                        f.group,
                        1,
                        event.parents.clone(),
                        3,
                        3,
                        EventKind::ChannelLeave,
                        event.ciphertext.clone(),
                    )
                }
                4 => {
                    event = Event::new(
                        &f.member,
                        f.group,
                        1,
                        event.parents.clone(),
                        3,
                        3,
                        EventKind::ChannelLeave,
                        [event.ciphertext.clone(), vec![0]].concat(),
                    )
                }
                _ => {
                    event = Event::new(
                        &f.member,
                        ConversationId::new([99; 32]),
                        1,
                        event.parents.clone(),
                        3,
                        3,
                        EventKind::ChannelLeave,
                        event.ciphertext.clone(),
                    )
                }
            }
            f.events.push(event);
            let projection = f.projection();
            assert!(
                projection
                    .authority
                    .account_grant(&f.account.public().ed25519_pub)
                    .is_some(),
                "invalid decline case {case} must not revoke any current grant"
            );
        }
    }

    #[test]
    fn decline_preparation_requires_exact_registered_full_device_binding() {
        let f = fixture(true);
        let mut witness = f.witness();
        let alternate = DeviceIdentity::from_secret_bytes(
            f.member.secret_bytes().0,
            DeviceIdentity::generate().secret_bytes().1,
        );
        witness.own = Announce::new_with_account(&alternate, &f.account, "wrong X identity", 1);
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Decline,
                &witness
            )
            .is_err()
        );
        let mut witness = f.witness();
        witness.grant.incarnation = [88; 16];
        assert!(
            expected_self_control(
                f.group,
                &f.projection(),
                AccountSelfControlInput::Decline,
                &witness
            )
            .is_err()
        );
    }
}
