//! Mutations of upgraded account-principal channel authority.

use super::{
    Node, NodeError,
    channel::ChannelMembershipProjection,
    channel_account_authority::{AccountAuthorityControl, AccountGrant, MAX_AUTHORITY_BYTES},
};
use crate::{
    channel::ChannelMeta,
    eventlog::{
        EventKind,
        event::{ConversationId, EventId},
    },
    identity::{account::AccountPublic, device::PublicIdentity},
};

pub(super) enum AccountChannelMutation {
    Legacy,
    Unchanged,
    Changed(Vec<PublicIdentity>),
}
impl Node {
    fn account_control(
        &self,
        channel: ConversationId,
    ) -> Result<Option<(ChannelMembershipProjection, AccountAuthorityControl)>, NodeError> {
        let projection = self.account_group_projection(channel);
        let Some(previous) = projection.authority.current_event() else {
            return Ok(None);
        };
        let state = projection
            .state
            .as_ref()
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        if state.is_dissolved()
            || !projection
                .authority
                .can_admin(&self.identity.public(), &self.own_group_proof())
        {
            return Err(NodeError::Channel(
                "only the active channel owner account can change channel authority".into(),
            ));
        }
        let control = AccountAuthorityControl {
            version: 1,
            previous: Some(previous),
            issuer: self.own_group_proof(),
            owner_account: self.account.public().ed25519_pub,
            grants: projection.authority.grants(),
            revoked: Vec::new(),
            legacy_bindings: Vec::new(),
            account_signature: Vec::new(),
        };
        Ok(Some((projection, control)))
    }
    fn append_account_control(
        &self,
        channel: ConversationId,
        control: &AccountAuthorityControl,
    ) -> Result<(), NodeError> {
        let payload = control.signed_encode(channel, &self.account);
        if AccountAuthorityControl::decode(&payload).is_none()
            || payload.len() > MAX_AUTHORITY_BYTES
        {
            return Err(NodeError::Channel(
                "channel account authority exceeds supported bounds".into(),
            ));
        }
        self.append_event(channel, EventKind::ChannelAccountAuthority, payload)?;
        self.process_channel(channel);
        Ok(())
    }
    pub(super) fn try_account_channel_remove(
        &self,
        channel: ConversationId,
        account_id: &str,
    ) -> Result<AccountChannelMutation, NodeError> {
        let Some((projection, mut control)) = self.account_control(channel)? else {
            return Ok(AccountChannelMutation::Legacy);
        };
        if projection.authority.owner_account_id().as_deref() == Some(account_id) {
            return Err(NodeError::Channel(
                "transfer channel ownership before the owner account leaves".into(),
            ));
        }
        let Some(index) = control.grants.iter().position(|grant| {
            grant.id().is_some_and(|id| {
                AccountPublic {
                    ed25519_pub: id.account_pub,
                }
                .account_id()
                    == account_id
            })
        }) else {
            return Ok(AccountChannelMutation::Unchanged);
        };
        let grant = control.grants.remove(index);
        control.revoked.push(grant.id().expect("projected grant"));
        // Closing proofs go to the prior participants; admission trims removed peers to the frozen prefix.
        let recipients = projection
            .state
            .as_ref()
            .expect("projected state")
            .members()
            .to_vec();
        self.append_account_control(channel, &control)?;
        if self
            .account_group_projection(channel)
            .authority
            .account_grant(&grant.id().expect("projected grant").account_pub)
            .is_some()
        {
            return Err(NodeError::Channel(
                "channel account removal was not admitted".into(),
            ));
        }
        Ok(AccountChannelMutation::Changed(recipients))
    }
    pub(super) fn try_account_channel_transfer(
        &self,
        channel: ConversationId,
        device_id: &str,
    ) -> Result<AccountChannelMutation, NodeError> {
        let Some((projection, mut control)) = self.account_control(channel)? else {
            return Ok(AccountChannelMutation::Legacy);
        };
        let state = projection.state.as_ref().expect("projected state");
        let account_id = state
            .member_account_id(device_id)
            .filter(|_| state.is_accepted_member(device_id))
            .ok_or_else(|| {
                NodeError::Channel(
                    "new owner account must have accepted the channel invitation".into(),
                )
            })?;
        if projection.authority.owner_account_id().as_deref() == Some(account_id) {
            return Ok(AccountChannelMutation::Unchanged);
        }
        let account = control
            .grants
            .iter()
            .filter_map(AccountGrant::id)
            .find(|id| {
                AccountPublic {
                    ed25519_pub: id.account_pub,
                }
                .account_id()
                    == account_id
            })
            .ok_or_else(|| {
                NodeError::Channel("new owner has no active channel account grant".into())
            })?;
        control.owner_account = account.account_pub;
        let recipients = state.members().to_vec();
        self.append_account_control(channel, &control)?;
        if self
            .account_group_projection(channel)
            .authority
            .owner_account_id()
            .as_deref()
            != Some(account_id)
        {
            return Err(NodeError::Channel(
                "channel account transfer was not admitted".into(),
            ));
        }
        Ok(AccountChannelMutation::Changed(recipients))
    }
    pub(super) fn try_account_channel_invite(
        &self,
        channel: ConversationId,
        new_member: &PublicIdentity,
        verified_proof: &crate::discovery::Announce,
    ) -> Result<AccountChannelMutation, NodeError> {
        let Some((projection, mut control)) = self.account_control(channel)? else {
            return Ok(AccountChannelMutation::Legacy);
        };
        let state = projection.state.as_ref().expect("projected state");
        if state
            .members()
            .iter()
            .any(|public| public.ed25519_pub == new_member.ed25519_pub && public != new_member)
        {
            return Err(NodeError::Channel("channel device identity changed".into()));
        }
        if verified_proof.public() != *new_member
            || !verified_proof.valid_presence()
            || verified_proof.post_office
            || verified_proof.account_cert.is_none()
        {
            return Err(NodeError::Channel(
                "verified account identity required before inviting a channel account".into(),
            ));
        }
        let proof = verified_proof.clone();
        let account_pub = proof
            .account_cert
            .as_ref()
            .expect("verified certificate")
            .account_ed25519_pub;
        if projection.authority.account_grant(&account_pub).is_some() {
            return Ok(AccountChannelMutation::Unchanged);
        }
        let mut members = state.members().to_vec();
        members.push(new_member.clone());
        members.sort_by_key(PublicIdentity::user_id);
        members.dedup();
        let meta = ChannelMeta {
            name: state.name().into(),
            members: members.clone(),
            epoch: state
                .epoch()
                .checked_add(1)
                .ok_or_else(|| NodeError::Channel("channel epoch exhausted".into()))?,
            owner: state.owner().into(),
        };
        let mut incarnation = [0; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut incarnation);
        control.grants.push(AccountGrant {
            incarnation,
            pending: true,
            original: proof,
            membership_event: EventId::new([0; 32]),
            acceptance_event: None,
        });
        let policy = self.channel_history_policy(channel);
        let selected = self.select_join_history(channel, policy, super::node::now_millis())?;
        self.append_invitation_with_history_and_control(
            channel,
            &meta,
            new_member,
            (selected, policy),
            |invitation| {
                control
                    .grants
                    .last_mut()
                    .expect("new grant")
                    .membership_event = invitation;
                let payload = control.signed_encode(channel, &self.account);
                if AccountAuthorityControl::decode(&payload).is_none() {
                    return Err(NodeError::Channel(
                        "channel account authority exceeds supported bounds".into(),
                    ));
                }
                Ok(Some((EventKind::ChannelAccountAuthority, payload)))
            },
        )?;
        self.process_channel(channel);
        if self
            .account_group_projection(channel)
            .authority
            .account_grant(&account_pub)
            .is_none()
        {
            return Err(NodeError::Channel(
                "channel account invitation was not admitted".into(),
            ));
        }
        Ok(AccountChannelMutation::Changed(members))
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
        assert!(f.node.channel_can_manage(f.channel));
        f.node
            .remove_channel_account_members_if(f.channel, &f.accounts[2].account_id(), |commit| {
                commit()
            })
            .await
            .unwrap();
        let projection = f.node.account_group_projection(f.channel);
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
        assert!(!f.node.channel_can_manage(f.channel));
    }

    #[tokio::test]
    async fn inherited_owner_invitation_commits_pending_account_authority_with_invitation() {
        let f = Fixture::new();
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
            &f._directory.path().join("invite-target.log"),
            &f._directory.path().join("invite-target-sent.log"),
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
        let projection = f.node.account_group_projection(f.channel);
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
        server.abort();
    }
}
