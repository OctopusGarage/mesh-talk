//! Mutations of upgraded account-principal channel authority.

use super::{Node, NodeError};
use crate::{
    eventlog::event::{ConversationId, EventId},
    identity::device::PublicIdentity,
};

pub(super) enum AccountChannelMutation {
    Legacy,
    Unchanged,
    Changed(Vec<PublicIdentity>),
    /// Durable owner effect whose exact indexed admission must complete after
    /// the caller releases its authorization and membership guards.
    CommittedControl {
        event: EventId,
        recipients: Vec<PublicIdentity>,
    },
}
impl Node {
    pub(super) fn try_account_channel_remove(
        &self,
        channel: ConversationId,
        account_id: &str,
    ) -> Result<AccountChannelMutation, NodeError> {
        self.apply_owner_control(
            channel,
            &super::channels::owner_control::OwnerControlInput::RemoveAccount(account_id.into()),
        )
    }
    pub(super) fn try_account_channel_transfer(
        &self,
        channel: ConversationId,
        device_id: &str,
    ) -> Result<AccountChannelMutation, NodeError> {
        let projection = self.account_group_projection(channel)?;
        if projection.authority.current_event().is_none() {
            return Ok(AccountChannelMutation::Legacy);
        }
        let state = projection
            .state
            .as_ref()
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        let account = state
            .member_account_id(device_id)
            .filter(|_| state.is_accepted_member(device_id))
            .ok_or_else(|| {
                NodeError::Channel(
                    "new owner account must have accepted the channel invitation".into(),
                )
            })?;
        self.apply_owner_control(
            channel,
            &super::channels::owner_control::OwnerControlInput::Transfer(account.into()),
        )
    }
    pub(super) fn try_account_channel_invite(
        &self,
        channel: ConversationId,
        new_member: &PublicIdentity,
        verified_proof: &crate::discovery::Announce,
    ) -> Result<AccountChannelMutation, NodeError> {
        use super::channels::account_invitation::{
            InvitationInput, InvitationPreparation, InvitationWitness,
        };
        if self
            .account_group_projection(channel)?
            .authority
            .current_event()
            .is_none()
        {
            return Ok(AccountChannelMutation::Legacy);
        }
        if verified_proof.public() != *new_member {
            return Err(NodeError::Authorization(
                "invitation target identity changed".into(),
            ));
        }
        let input = InvitationInput {
            target_account_id: verified_proof.account_id().ok_or_else(|| {
                NodeError::Authorization("verified invitation account required".into())
            })?,
        };
        let policy = self.channel_history_policy(channel)?;
        let selected = self.select_join_history(channel, policy, super::node::now_millis())?;
        let history = self.prepare_invitation_history(channel, new_member, selected, policy)?;
        let mut incarnation = [0; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut incarnation);
        let own = self.own_group_proof();
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let (transaction, recipients) = {
            let book = self.channels.lock().expect("channels mutex");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
            let log = self.log.lock().expect("log mutex");
            let projection =
                super::channel::ChannelBook::project_membership(channel, &log.events(&channel)?);
            let witness = InvitationWitness {
                own,
                target: verified_proof.clone(),
                incarnation,
                parents: log.prepare(&channel).map_err(NodeError::Log)?.0,
                recipients: projection
                    .state
                    .as_ref()
                    .ok_or_else(|| NodeError::Channel("unknown channel".into()))?
                    .members()
                    .to_vec(),
            };
            let Some(prepared) = self.prepare_account_invitation_locked(InvitationPreparation {
                group: channel,
                log: &log,
                state,
                projection: &projection,
                input: &input,
                witness,
                history,
            })?
            else {
                return Ok(AccountChannelMutation::Unchanged);
            };
            let transaction = super::delivery_store::DeliveryTransaction::AccountInvitation {
                input,
                witness: prepared.witness,
                transaction: Box::new(prepared.transaction),
            };
            delivery
                .preflight_group_invitation_transaction(&transaction, &log)
                .map_err(NodeError::Log)?;
            log.sync().map_err(NodeError::Log)?;
            (transaction, prepared.recipients)
        };
        delivery.begin(transaction).map_err(NodeError::Log)?;
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        drop(delivery);
        self.process_channel(channel);
        Ok(AccountChannelMutation::Changed(recipients))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        Node,
        channel_account_authority::{AccountAuthorityControl, AccountGrant},
        channel_account_bootstrap::{DeviceControl, inheritance_account_message},
    };
    use crate::{
        channel::ChannelMeta,
        discovery::{Announce, Roster},
        eventlog::{Event, EventKind, event::ConversationId},
        identity::{account::Account, device::DeviceIdentity},
    };
    use std::{
        net::{IpAddr, Ipv4Addr},
        sync::{Arc, Mutex},
    };
    use tokio::sync::mpsc;

    struct Fixture {
        _directory: tempfile::TempDir,
        node: Arc<Node>,
        channel: ConversationId,
        accounts: Vec<Account>,
        devices: Vec<DeviceIdentity>,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let owner_account = Account::generate();
            let accounts = vec![
                owner_account,
                Account::generate(),
                Account::generate(),
                Account::generate(),
            ];
            let devices = (0..4)
                .map(|_| DeviceIdentity::generate())
                .collect::<Vec<_>>();
            let (dm, _) = mpsc::unbounded_channel();
            let (channel_tx, _) = mpsc::unbounded_channel();
            let (files, _) = mpsc::unbounded_channel();
            let node = Node::open_with_account(
                DeviceIdentity::generate(),
                Account::from_secret_bytes(accounts[0].secret_bytes()),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel_tx,
                files,
                &directory.path().join("events.log"),
                &directory.path().join("sent.log"),
                "pw",
            )
            .unwrap();
            node.set_post_office_enabled(false);
            let channel = ConversationId::new([75; 32]);
            let meta = ChannelMeta {
                name: "account roles".into(),
                members: devices[..3].iter().map(DeviceIdentity::public).collect(),
                epoch: 0,
                owner: devices[0].user_id(),
            };
            let root = Event::new(
                &devices[0],
                channel,
                1,
                vec![],
                1,
                1,
                EventKind::MembershipChange,
                meta.encode(),
            );
            let proofs = devices[..3]
                .iter()
                .zip(&accounts[..3])
                .map(|(device, account)| Announce::new_with_account(device, account, "member", 1))
                .collect::<Vec<_>>();
            let grants = proofs
                .iter()
                .enumerate()
                .map(|(index, proof)| AccountGrant {
                    incarnation: [index as u8 + 1; 16],
                    pending: false,
                    original: proof.clone(),
                    membership_event: root.id,
                    acceptance_event: None,
                })
                .collect::<Vec<_>>();
            let control = AccountAuthorityControl {
                version: 1,
                previous: None,
                issuer: proofs[0].clone(),
                owner_account: accounts[0].public().ed25519_pub,
                grants: grants.clone(),
                revoked: vec![],
                legacy_bindings: proofs,
                account_signature: vec![],
            };
            let authority = Event::new(
                &devices[0],
                channel,
                2,
                vec![root.id],
                2,
                2,
                EventKind::ChannelAccountAuthority,
                control.signed_encode(channel, &accounts[0]),
            );
            let inheritance = DeviceControl::Inherit {
                authority: authority.id,
                grant: grants[0].id().unwrap(),
                device: node.own_group_proof(),
                account_signature: accounts[0]
                    .sign(&inheritance_account_message(
                        channel,
                        authority.id,
                        grants[0].id().unwrap(),
                        &node.identity.public(),
                    ))
                    .to_vec(),
            };
            let inherited = Event::new(
                &node.identity,
                channel,
                1,
                vec![authority.id],
                3,
                3,
                EventKind::ChannelDeviceInheritance,
                inheritance.encode(),
            );
            {
                let mut log = node.log.lock().unwrap();
                for event in [root, authority, inherited] {
                    log.append(event).unwrap();
                }
            }
            node.process_channel(channel);
            crate::node::test_support::settle_group_metadata(&node, channel);
            Self {
                _directory: directory,
                node,
                channel,
                accounts,
                devices,
            }
        }
    }

    #[tokio::test]
    async fn inherited_owner_removes_account_and_transfers_account_authority() {
        let f = Fixture::new();
        assert!(f.node.channel_can_manage(f.channel).unwrap());
        f.node
            .remove_channel_account_members_if(f.channel, &f.accounts[2].account_id(), |commit| {
                commit()
            })
            .await
            .unwrap();
        let projection = f.node.account_group_projection(f.channel).unwrap();
        assert!(
            projection
                .authority
                .account_grant(&f.accounts[2].public().ed25519_pub)
                .is_none()
        );
        assert_eq!(projection.authority.key_epoch(), Some(1));
        f.node
            .transfer_channel_owner(f.channel, &f.devices[1].user_id())
            .await
            .unwrap();
        assert_eq!(
            f.node.channel_owner_account_id(f.channel),
            Some(f.accounts[1].account_id())
        );
        assert!(!f.node.channel_can_manage(f.channel).unwrap());
    }

    #[tokio::test]
    async fn inherited_owner_invitation_commits_pending_account_authority_with_invitation() {
        let f = Fixture::new();
        let target_directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let (dm, _) = mpsc::unbounded_channel();
        let (channels, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let secret = f.devices[3].secret_bytes();
        let target = Node::open_with_account(
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            Account::from_secret_bytes(f.accounts[3].secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channels,
            files,
            &target_directory.path().join("invite-target.log"),
            &target_directory.path().join("invite-target-sent.log"),
            "pw",
        )
        .unwrap();
        target.set_post_office_enabled(false);
        let proof = Announce::new_with_account(
            &f.devices[3],
            &f.accounts[3],
            "new account",
            listener.local_addr().unwrap().port(),
        );
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let target = Arc::clone(&target);
                tokio::spawn(async move {
                    if let Ok(channel) = target.privacy_accept(stream).await {
                        target.serve_connection(channel).await;
                    }
                });
            }
        });
        f.node.roster.lock().unwrap().update(
            &proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &f.node.user_id(),
        );
        f.node
            .add_channel_member(f.channel, f.devices[3].public())
            .await
            .unwrap();
        let projection = f.node.account_group_projection(f.channel).unwrap();
        let grant = projection
            .authority
            .account_grant(&f.accounts[3].public().ed25519_pub)
            .expect("invitation durably declares an account grant");
        assert!(grant.pending);
        assert!(
            projection
                .authority
                .invitation_for_device(&f.devices[3].public(), &proof)
                .is_some()
        );
        assert!(
            !projection
                .authority
                .is_member(&f.devices[3].public(), &proof)
        );
        assert_eq!(projection.authority.key_epoch(), Some(0));
        {
            let log = f.node.log.lock().unwrap();
            let event = log
                .get(&projection.authority.current_event().unwrap())
                .unwrap()
                .unwrap();
            assert!(event.parents.contains(&grant.membership_event));
            assert_eq!(
                log.get(&grant.membership_event).unwrap().unwrap().kind,
                EventKind::MembershipChange
            );
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}
