use super::*;
use crate::{
    channel::{ChannelMeta, new_channel_id},
    discovery::{Announce, Roster},
    eventlog::{Event, EventKind},
    identity::{account::Account, device::DeviceIdentity},
    node::{AccountDevice, AccountDocumentDomain, Node, SyncCategory},
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

fn node(path: &Path, account: &Account) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        DeviceIdentity::generate(),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        files,
        &path.join("events.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap()
}

fn legacy_group(owner: &Node, member: &Node) -> Event {
    Event::new(
        &owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Owner account fixture".into(),
            members: vec![owner.identity.public(), member.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    )
}

fn install(node: &Node, event: &Event) {
    node.log.lock().unwrap().append(event.clone()).unwrap();
    node.process_channel(event.conversation_id);
}

#[test]
fn local_legacy_owner_access_preserves_the_real_membership_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &Account::generate());
    let genesis = legacy_group(&owner, &member);
    install(&owner, &genesis);
    let access = owner.account_group_access(genesis.conversation_id).unwrap();
    assert_eq!(access.role, AccountGroupRole::Owner);
    assert_eq!(access.execution, AccountGroupExecution::Local);
    assert_eq!(access.sources.len(), 1);
    assert_eq!(access.sources[0].lifetime.member, owner.identity.public());
    assert_eq!(access.sources[0].lifetime.membership, genesis.id);
    assert_eq!(access.sources[0].lifetime.acceptance, None);
    assert!(!access.can_leave_account());
    assert!(genesis.verify_integrity() && genesis.verify_signature());
}

#[test]
fn a_certificate_for_an_owner_device_alone_cannot_claim_its_account_role() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let member = node(&dir.path().join("member"), &account);
    let genesis = legacy_group(&owner, &member);
    install(&member, &genesis);
    member
        .account_devices
        .lock()
        .unwrap()
        .insert(crate::node::AccountDevice {
            public: owner.identity.public(),
            certificate: account.certify(&owner.identity.public().ed25519_pub),
        })
        .unwrap();
    let access = member
        .account_group_access(genesis.conversation_id)
        .unwrap();
    assert_eq!(access.role, AccountGroupRole::Member);
    assert!(access.can_leave_account());
    assert!(!member.channel_can_manage(genesis.conversation_id));
    assert_eq!(
        member.channel_owner(genesis.conversation_id),
        owner.user_id()
    );
}

#[tokio::test]
async fn modern_owner_access_reuses_the_existing_account_authority() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(dir.path(), &account);
    let group = owner
        .create_channel("Modern owner account", vec![])
        .await
        .unwrap();
    owner
        .maintain_account_channels(
            &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
        )
        .await;
    assert!(
        owner
            .account_group_projection(group)
            .authority
            .current_event()
            .is_some()
    );
    let access = owner.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Owner);
    assert_eq!(access.execution, AccountGroupExecution::Local);
    assert_eq!(access.sources.len(), 1);
    assert_eq!(
        access.sources[0].authority.authority,
        owner
            .account_group_projection(group)
            .authority
            .current_event()
    );
    assert!(access.sources[0].authority.authority.is_some());
    assert!(!access.can_leave_account());
}

fn configure(node: &Node, path: &Path, port: u16) {
    node.configure_privacy(
        path,
        "pw",
        &node.signed_announce("Account role fixture", port),
        Arc::new(crate::discovery::DiscoveryVisibility::default()),
    )
    .unwrap();
}

fn link(left: &Node, right: &Node) {
    left.account_devices
        .lock()
        .unwrap()
        .insert(AccountDevice {
            public: right.identity.public(),
            certificate: left.account.certify(&right.identity.public().ed25519_pub),
        })
        .unwrap();
}

async fn pull_group_control(target: &Arc<Node>, source: &Node, address: std::net::SocketAddr) {
    target.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &source.identity,
            &source.account,
            "Owner source",
            address.port(),
        ),
        address.ip(),
        &target.user_id(),
    );
    let peer = target.routing_peer(&source.user_id()).unwrap();
    let mut cursor = crate::node::account_documents::sync::DocumentSyncCursor::default();
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    assert!(
        target
            .account_documents(AccountDocumentDomain::GroupControl, None, 64)
            .unwrap()
            .items
            .iter()
            .any(|meta| meta.writer_device == source.user_id())
    );
}

fn inbound_server(node: Arc<Node>, listener: TcpListener) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, _) = result.unwrap();
                    let node = node.clone();
                    connections.spawn(Box::pin(async move {
                        if let Ok(channel) = node.privacy_accept(stream).await {
                            node.serve_connection(channel).await;
                        }
                    }));
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    })
}

async fn owner_binding_relay_scenario(owner_online: bool, certificate_only: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner_account = if certificate_only {
        Account::generate()
    } else {
        Account::from_secret_bytes(account.secret_bytes())
    };
    let owner = node(&dir.path().join("owner"), &owner_account);
    let member = node(&dir.path().join("member"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = Event::new(
        &owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Relayed owner fixture".into(),
            members: vec![
                owner.identity.public(),
                member.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    install(&owner, &genesis);
    install(&member, &genesis);
    link(&member, &owner);
    link(&reference, &owner);
    link(&member, &reference);
    link(&reference, &member);
    if !certificate_only {
        link(&owner, &member);
        link(&owner, &reference);
    }
    let owner_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let owner_address = owner_listener.local_addr().unwrap();
    let member_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let member_address = member_listener.local_addr().unwrap();
    let reference_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&owner, &dir.path().join("owner"), owner_address.port());
    configure(&member, &dir.path().join("member"), member_address.port());
    configure(
        &reference,
        &dir.path().join("reference"),
        reference_listener.local_addr().unwrap().port(),
    );
    let mut owner_server = if certificate_only {
        None
    } else {
        Some(inbound_server(owner.clone(), owner_listener))
    };
    if !certificate_only {
        let channel = member
            .privacy_dial(owner_address, &owner.identity.public())
            .await
            .unwrap();
        assert_eq!(channel.peer_identity(), &owner.identity.public());
        assert_eq!(
            channel.peer_announcement().and_then(Announce::account_id),
            Some(account.account_id())
        );
        drop(channel);
    }
    assert!(
        member
            .account_documents(AccountDocumentDomain::GroupControl, None, 64)
            .unwrap()
            .items
            .is_empty(),
        "the member must not have obtained an Owner source head"
    );
    if !owner_online {
        if let Some(server) = owner_server.take() {
            server.abort();
            let _ = server.await;
        }
        member
            .roster
            .lock()
            .unwrap()
            .forget(&owner.identity.public());
    }
    let expected = if certificate_only {
        AccountGroupRole::Member
    } else {
        AccountGroupRole::Owner
    };
    assert_eq!(
        member
            .account_group_access(genesis.conversation_id)
            .unwrap()
            .role,
        expected,
        "a real signed owner binding and validated public ownership must work without an Owner v5 head; a certificate alone must not"
    );
    for node in [&member, &reference] {
        node.set_device_sync_category(SyncCategory::History, false)
            .unwrap();
    }
    owner.publish_account_group_access(&mut None);
    let old_owner = owner
        .account_document(&crate::node::AccountDocumentKey::GroupControl {
            channel: hex::encode(genesis.conversation_id.as_bytes()),
            source_device: owner.user_id(),
        })
        .unwrap()
        .unwrap();
    member.publish_account_group_access(&mut None);
    let document = member
        .account_document(&crate::node::AccountDocumentKey::GroupControl {
            channel: hex::encode(genesis.conversation_id.as_bytes()),
            source_device: member.user_id(),
        })
        .unwrap()
        .unwrap();
    let head =
        wire::GroupControlHead::decode(document.payload(), &account.public().ed25519_pub).unwrap();
    assert_eq!(
        head.access.source.role,
        AccountGroupRole::Member,
        "relaying owner facts must not change the publisher's physical role"
    );
    let member_server = inbound_server(member.clone(), member_listener);
    pull_group_control(&reference, &member, member_address).await;
    let access = reference
        .account_group_access(genesis.conversation_id)
        .unwrap();
    assert_eq!(access.role, expected);
    assert!(
        reference
            .channel_members(genesis.conversation_id)
            .is_empty()
    );
    assert!(!reference.channel_can_manage(genesis.conversation_id));
    if !certificate_only {
        assert!(!access.can_leave_account());
        assert_eq!(access.execution, AccountGroupExecution::WaitingForDevice);
        let sending = access
            .sources
            .iter()
            .find(|source| source.device == member.identity.public())
            .unwrap();
        assert_eq!(sending.role, AccountGroupRole::Member);
        assert_eq!(
            sending.execution,
            AccountGroupExecution::ViaDevice(member.identity.public())
        );
        if owner_online {
            reference.roster.lock().unwrap().update(
                &owner.signed_announce("Online unprobed owner", owner_address.port()),
                owner_address.ip(),
                &reference.user_id(),
            );
            assert_eq!(
                reference
                    .account_group_access(genesis.conversation_id)
                    .unwrap()
                    .execution,
                AccountGroupExecution::Reconciling,
                "a live upgraded peer without an authenticated v5 probe is not known to require an upgrade"
            );
            owner.publish_account_group_access(&mut None);
            pull_group_control(&reference, &owner, owner_address).await;
            assert_eq!(
                reference
                    .account_group_access(genesis.conversation_id)
                    .unwrap()
                    .execution,
                AccountGroupExecution::ViaDevice(owner.identity.public())
            );
        }
    }
    if !certificate_only {
        // The online physical member learns the exact old Owner head before
        // its full public view advances. This makes supersession explicit.
        let restore_offline = owner_server.is_none();
        if restore_offline {
            owner_server = Some(inbound_server(
                owner.clone(),
                TcpListener::bind(owner_address).await.unwrap(),
            ));
        }
        pull_group_control(&member, &owner, owner_address).await;
        if restore_offline {
            let server = owner_server.take().unwrap();
            server.abort();
            let _ = server.await;
            member
                .roster
                .lock()
                .unwrap()
                .forget(&owner.identity.public());
        }
        owner
            .append_event(
                genesis.conversation_id,
                EventKind::ChannelOwnerTransfer,
                external.user_id().into_bytes(),
            )
            .unwrap();
        owner.process_channel(genesis.conversation_id);
        let transfer = owner
            .log
            .lock()
            .unwrap()
            .events(&genesis.conversation_id)
            .into_iter()
            .find(|event| event.kind == EventKind::ChannelOwnerTransfer)
            .unwrap()
            .clone();
        install(&member, &transfer);
        member.publish_account_group_access(&mut None);
        pull_group_control(&reference, &member, member_address).await;
        assert_eq!(
            reference
                .account_group_access(genesis.conversation_id)
                .unwrap()
                .role,
            AccountGroupRole::Member,
            "new relayed public transfer revokes inherited owner role"
        );
        assert_eq!(
            owner.account_document_meta(&old_owner.meta.key),
            Some(old_owner.meta.clone()),
            "the delayed source must still serve its pre-transfer signed Owner head"
        );
        if owner_server.is_none() {
            owner_server = Some(inbound_server(
                owner.clone(),
                TcpListener::bind(owner_address).await.unwrap(),
            ));
        }
        pull_group_control(&reference, &owner, owner_address).await;
        assert_eq!(
            reference
                .account_group_access(genesis.conversation_id)
                .unwrap()
                .role,
            AccountGroupRole::Member,
            "a delayed Owner writer cannot outrank a newer public transfer from another writer"
        );
        assert!(
            reference
                .channel_members(genesis.conversation_id)
                .is_empty()
        );
    }
    reference.publish_account_group_access(&mut None);
    assert!(
        reference
            .account_documents(AccountDocumentDomain::GroupControl, None, 64)
            .unwrap()
            .items
            .iter()
            .all(|meta| meta.writer_device != reference.user_id()),
        "a reference-only device must never publish borrowed authority as physical membership"
    );
    member_server.abort();
    let _ = member_server.await;
    if let Some(server) = owner_server {
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn offline_owner_account_observation_reaches_a_new_reference_through_a_real_member() {
    owner_binding_relay_scenario(false, false).await;
}

#[tokio::test]
async fn a_relayed_owner_certificate_without_the_owners_signature_never_grants_owner_role() {
    owner_binding_relay_scenario(false, true).await;
}

#[tokio::test]
async fn online_relayed_owner_is_reconciling_until_an_authenticated_v5_probe_succeeds() {
    owner_binding_relay_scenario(true, false).await;
}

#[tokio::test]
async fn legacy_owner_account_has_the_same_owner_role_on_member_and_reference_siblings_with_history_off()
 {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let genesis = legacy_group(&owner, &member);
    install(&owner, &genesis);
    install(&member, &genesis);
    for target in [&member, &reference] {
        link(&owner, target);
        link(target, &owner);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&owner, &dir.path().join("owner"), address.port());
    let member_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reference_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(
        &member,
        &dir.path().join("member"),
        member_listener.local_addr().unwrap().port(),
    );
    configure(
        &reference,
        &dir.path().join("reference"),
        reference_listener.local_addr().unwrap().port(),
    );
    for target in [&owner, &member, &reference] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
    }
    owner.publish_account_group_access(&mut None);
    let server = tokio::spawn(owner.clone().run_accept_loop(listener));
    for target in [&member, &reference] {
        pull_group_control(target, &owner, address).await;
        let access = target
            .account_group_access(genesis.conversation_id)
            .unwrap();
        assert_eq!(access.role, AccountGroupRole::Owner);
        assert_eq!(
            access.execution,
            AccountGroupExecution::ViaDevice(owner.identity.public())
        );
        assert!(
            !access.can_leave_account(),
            "the owner account cannot leave through another physical member"
        );
        assert!(
            !target.channel_can_manage(genesis.conversation_id),
            "account permission must not turn a sibling into the public owner signer"
        );
    }
    let member_access = member
        .account_group_access(genesis.conversation_id)
        .unwrap();
    assert_eq!(
        member_access
            .sources
            .iter()
            .find(|source| source.device == member.identity.public())
            .unwrap()
            .role,
        AccountGroupRole::Member
    );
    assert!(
        reference
            .channel_members(genesis.conversation_id)
            .is_empty()
    );
    assert_eq!(
        member.channel_owner(genesis.conversation_id),
        owner.user_id()
    );
    server.abort();
    let _ = server.await;
    for target in [&member, &reference] {
        target
            .roster
            .lock()
            .unwrap()
            .forget(&owner.identity.public());
        let access = target
            .account_group_access(genesis.conversation_id)
            .unwrap();
        assert_eq!(access.role, AccountGroupRole::Owner);
        assert_eq!(access.execution, AccountGroupExecution::WaitingForDevice);
        assert!(!access.can_leave_account());
    }
    assert!(genesis.verify_integrity() && genesis.verify_signature());
}

#[tokio::test]
async fn a_newer_validated_owner_transfer_prevents_old_source_evidence_restoring_owner() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = Event::new(
        &owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Public owner transfer".into(),
            members: vec![
                owner.identity.public(),
                member.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    install(&owner, &genesis);
    install(&member, &genesis);
    link(&owner, &member);
    link(&member, &owner);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&owner, &dir.path().join("owner"), address.port());
    let member_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(
        &member,
        &dir.path().join("member"),
        member_listener.local_addr().unwrap().port(),
    );
    owner.publish_account_group_access(&mut None);
    let server = tokio::spawn(owner.clone().run_accept_loop(listener));
    pull_group_control(&member, &owner, address).await;
    assert_eq!(
        member
            .account_group_access(genesis.conversation_id)
            .unwrap()
            .role,
        AccountGroupRole::Owner
    );
    server.abort();
    let _ = server.await;
    owner
        .append_event(
            genesis.conversation_id,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(genesis.conversation_id);
    assert_eq!(
        owner.channel_owner(genesis.conversation_id),
        external.user_id()
    );
    let transfer = owner
        .log
        .lock()
        .unwrap()
        .events(&genesis.conversation_id)
        .into_iter()
        .find(|event| event.kind == EventKind::ChannelOwnerTransfer)
        .unwrap()
        .clone();
    assert!(transfer.verify_signature());
    install(&member, &transfer);
    for _ in 0..2 {
        let access = member
            .account_group_access(genesis.conversation_id)
            .unwrap();
        assert_eq!(
            access.role,
            AccountGroupRole::Member,
            "the retained old Owner head must lose to newer validated public ownership"
        );
        assert!(access.can_leave_account());
    }
    assert_eq!(
        member.channel_owner(genesis.conversation_id),
        external.user_id()
    );
}

#[tokio::test]
async fn a_newer_source_member_observation_wins_before_its_public_transfer_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = Event::new(
        &owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Transfer arrives through private observation first".into(),
            members: vec![
                owner.identity.public(),
                member.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    install(&owner, &genesis);
    install(&member, &genesis);
    assert!(owner.channel_can_manage(genesis.conversation_id));
    owner
        .append_event(
            genesis.conversation_id,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(genesis.conversation_id);
    assert_eq!(
        owner.channel_owner(genesis.conversation_id),
        external.user_id()
    );
    assert_eq!(
        member.channel_owner(genesis.conversation_id),
        owner.user_id()
    );
    link(&owner, &member);
    link(&member, &owner);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let member_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&owner, &dir.path().join("owner"), address.port());
    configure(
        &member,
        &dir.path().join("member"),
        member_listener.local_addr().unwrap().port(),
    );
    owner.publish_account_group_access(&mut None);
    let server = tokio::spawn(owner.clone().run_accept_loop(listener));
    pull_group_control(&member, &owner, address).await;
    server.abort();
    let _ = server.await;
    let access = member
        .account_group_access(genesis.conversation_id)
        .unwrap();
    assert_eq!(
        access.role,
        AccountGroupRole::Member,
        "a known old membership root does not prove old local ownership is newer than the source observation"
    );
    assert!(access.can_leave_account());
    assert_eq!(
        member.channel_owner(genesis.conversation_id),
        owner.user_id(),
        "this fixture must still lack the public transfer"
    );
}
