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

#[path = "tests/message_actions.rs"]
mod message_actions;

#[path = "tests/message_action_reference.rs"]
mod message_action_reference;

pub(in crate::node) use selected_same_source_control_latency::ControlTestPages;

// Cold authenticated fixture arrival is established before operation admission.
// Keep its existing 30-second owner budget separate from the held 8-second
// operation/progress assertions; this helper does not drive additional traffic.
async fn cold_account_setup<T>(
    future: impl std::future::Future<Output = T>,
) -> Result<T, tokio::time::error::Elapsed> {
    tokio::time::timeout(std::time::Duration::from_secs(30), future).await
}

fn has_account_group_operation_support(
    node: &Node,
    peer: &Node,
    kind: Option<AccountGroupOperationKind>,
) -> bool {
    node.group_operation_versions
        .lock()
        .unwrap()
        .get(&peer.user_id())
        .is_some_and(|support| {
            support.public == peer.identity.public()
                && kind.is_none_or(|kind| support.kinds & kind.capability_bit() != 0)
        })
}

// Callers include explicit setup work, if any, and this observation in the same
// cold budget. This observer neither starts producers nor drives extra requests.
async fn observe_account_fixture_condition(mut ready: impl FnMut() -> bool) {
    while !ready() {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn cold_account_control_setup(
    readiness: impl std::future::Future<Output = ()>,
    role_pages: impl std::future::Future<Output = ()>,
) -> impl std::future::Future<Output = ()> {
    // Box before constructing the returned future: callers must not retain both
    // large protocol states inline even while neither future has been polled.
    let readiness = Box::pin(readiness);
    let role_pages = Box::pin(role_pages);
    async move {
        cold_account_setup(readiness)
            .await
            .expect("real capabilities and required cold state must precede role-page bootstrap");
        role_pages.await;
    }
}

// Keep cleanup completion separate from the health evidence it can invalidate.
async fn capture_account_sync_before_cleanup(
    node: &Node,
    cleanup: impl std::future::Future<Output = ()>,
) -> Vec<crate::node::DeviceSyncStatus> {
    let snapshot = node.device_sync_status();
    cleanup.await;
    snapshot
}

fn assert_no_account_document_errors(snapshot: &[crate::node::DeviceSyncStatus]) {
    assert!(
        snapshot
            .iter()
            .all(|status| status.document_error.is_none()),
        "owned preemption must not produce a document error"
    );
}

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
async fn legacy_account_leave_commits_with_owner_offline() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let member = node(&dir.path().join("member"), &account);
    let genesis = legacy_group(&owner, &member);
    install(&member, &genesis);
    let group = genesis.conversation_id;
    let id = member
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let accepted = member
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::LeaveAccount,
        )
        .expect("legacy leave must enter the actual account operation path");
    assert_eq!(accepted.id, id);
    for _ in 0..8 {
        member.maintain_account_group_operation_effects();
    }
    let status = member
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(status.committed_events.len(), 1);
    assert_eq!(
        member.account_group_access(group).unwrap().role,
        AccountGroupRole::Left
    );
    let events = member
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let leaves: Vec<_> = events
        .iter()
        .filter(|event| event.kind == EventKind::ChannelLeave)
        .collect();
    assert_eq!(leaves.len(), 1);
    assert!(leaves[0].verify_signature());
    assert!(leaves[0].ciphertext.is_empty());
    let operations = member
        .list_account_group_operations(
            &account.account_id(),
            AccountGroupOperationQuery {
                group: Some(group),
                pending_only: false,
                after: None,
                limit: 64,
            },
        )
        .unwrap();
    assert_eq!(
        operations.len(),
        1,
        "private child work must not appear as extra user operations"
    );
}

#[tokio::test]
async fn legacy_account_leave_cancelled_before_execution_emits_no_departure() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let member = node(&dir.path().join("member"), &account);
    let genesis = legacy_group(&owner, &member);
    install(&member, &genesis);
    let group = genesis.conversation_id;
    let id = member
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    member
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::LeaveAccount,
        )
        .unwrap();
    member
        .cancel_account_group_operation(&account.account_id(), id)
        .unwrap();
    for _ in 0..4 {
        member.maintain_account_group_operation_effects();
    }
    let status = member
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Cancelled);
    assert!(status.committed_events.is_empty());
    assert_eq!(
        member.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert!(
        !member
            .log
            .lock()
            .unwrap()
            .events(&group)
            .iter()
            .any(|event| event.kind == EventKind::ChannelLeave)
    );
}

#[test]
fn legacy_invitation_controls_still_reject_before_admission_without_account_authority() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let member = node(&dir.path().join("member"), &account);
    let group = new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Legacy invitation".into(),
            members: vec![owner.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let invitation = Event::new(
        &owner.identity,
        group,
        2,
        vec![genesis.id],
        2,
        2,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Legacy invitation".into(),
            members: vec![owner.identity.public(), member.identity.public()],
            epoch: 1,
            owner: owner.user_id(),
        }
        .encode_invitation(),
    );
    install(&member, &genesis);
    install(&member, &invitation);
    assert_eq!(
        member.account_group_access(group).unwrap().role,
        AccountGroupRole::Invited
    );
    for operation in [
        AccountGroupOperation::AcceptInvitation,
        AccountGroupOperation::DeclineInvitation,
    ] {
        let id = member
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        assert!(
            matches!(member.submit_account_group_operation(&account.account_id(), id, group, operation),
            Err(crate::node::NodeError::InvalidInput(reason)) if reason.contains("account-membership format update required"))
        );
        assert!(
            member
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_account_leave_coordinates_original_devices_over_noise_with_history_off() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let first = node(&dir.path().join("first"), &account);
    let second = node(&dir.path().join("second"), &account);
    let genesis = Event::new(
        &owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Legacy account departure".into(),
            members: vec![
                owner.identity.public(),
                first.identity.public(),
                second.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
    install(&first, &genesis);
    install(&second, &genesis);
    link(&first, &second);
    link(&second, &first);
    let mut servers = Vec::new();
    let mut addresses = Vec::new();
    for (name, target) in [("first", &first), ("second", &second)] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        servers.push(inbound_server(target.clone(), listener));
    }
    for (left, right, address) in [
        (&first, &second, addresses[1]),
        (&second, &first, addresses[0]),
    ] {
        left.roster.lock().unwrap().update(
            &right.signed_announce("Legacy departure peer", address.port()),
            address.ip(),
            &left.user_id(),
        );
        left.publish_account_group_access(&mut None);
    }
    let id = first
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    first
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::LeaveAccount,
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            first.maintain_account_group_operation_effects();
            second.maintain_account_group_operation_effects();
            for (target, source) in [(&second, &first), (&first, &second)] {
                let peer = target.routing_peer(&source.user_id()).unwrap();
                let mut cursor =
                    crate::node::account_documents::sync::DocumentSyncCursor::mandatory();
                for _ in 0..4 {
                    target
                        .pull_account_documents_page(&peer, &mut cursor)
                        .await
                        .unwrap();
                    if cursor.up_to_date {
                        break;
                    }
                }
            }
            if first
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .unwrap()
                .phase
                == AccountGroupOperationPhase::Committed
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the owner is offline; certified original members must finish their own departure");
    let status = first
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.committed_events.len(), 2);
    for member in [&first, &second] {
        let events = member
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let leaves: Vec<_> = events
            .iter()
            .filter(|event| event.kind == EventKind::ChannelLeave)
            .collect();
        assert_eq!(
            leaves.len(),
            1,
            "each original device signs exactly its own departure"
        );
        assert_eq!(
            leaves[0].author.ed25519_pub(),
            &member.identity.public().ed25519_pub
        );
        assert!(leaves[0].verify_signature() && leaves[0].ciphertext.is_empty());
        let operations = member
            .list_account_group_operations(
                &account.account_id(),
                AccountGroupOperationQuery {
                    group: Some(group),
                    pending_only: false,
                    after: None,
                    limit: 64,
                },
            )
            .unwrap();
        assert!(operations.iter().all(|operation| operation.id == id));
    }
    for server in servers {
        server.abort();
    }
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

async fn pull_group_control(
    target: &Arc<Node>,
    source: &Node,
    address: std::net::SocketAddr,
    group: crate::eventlog::ConversationId,
) {
    // Reuse the bounded role-page bootstrap, including exact local capacity
    // backpressure. Authentication and protocol failures still fail the fixture.
    message_action_reference::pull_reference_action_control(target, source, address, group).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_group_invites_one_account_and_starts_with_account_owner_authority() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let invited_account = Account::generate();
    let first = node(&dir.path().join("first"), &invited_account);
    let second = node(&dir.path().join("second"), &invited_account);
    let mut servers = Vec::new();
    for (index, peer) in [&first, &second].into_iter().enumerate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let privacy = dir.path().join(format!("privacy-{index}"));
        std::fs::create_dir_all(&privacy).unwrap();
        configure(peer, &privacy, address.port());
        owner.roster.lock().unwrap().update(
            &peer.signed_announce("same account", address.port()),
            address.ip(),
            &owner.user_id(),
        );
        servers.push(inbound_server(peer.clone(), listener));
    }
    let group = owner
        .create_account_channel_invited_if(
            "Account invitations",
            vec![first.account_id(), second.account_id()],
            |commit| commit(),
        )
        .await
        .unwrap();
    let projection = owner.account_group_projection(group);
    assert!(
        projection.authority.current_event().is_some(),
        "new groups must not wait for legacy migration"
    );
    assert_eq!(
        projection.invited.len(),
        1,
        "two devices are one invitation"
    );
    assert_eq!(
        owner.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(
        projection
            .authority
            .account_grant(&invited_account.public().ed25519_pub)
            .unwrap()
            .pending
    );
    for server in servers {
        server.abort();
    }
}

#[tokio::test]
async fn new_account_group_rejects_unknown_binding_and_retired_admission_before_creation() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path(), &Account::generate());
    let unknown = Account::generate().account_id();
    assert!(
        owner
            .create_account_channel_invited_if("unknown", vec![unknown], |commit| commit())
            .await
            .is_err()
    );
    assert!(
        owner
            .create_account_channel_invited_if("retired", vec![], |_| Err(
                super::super::NodeError::Authorization("retired".into())
            ))
            .await
            .is_err()
    );
    assert!(
        owner.list_channels().is_empty(),
        "failed creation cannot leave a partial group"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_account_group_rejects_a_route_whose_authenticated_account_changed() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let peer = node(&dir.path().join("peer"), &Account::generate());
    let advertised_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    owner.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &peer.identity,
            &advertised_account,
            "same nickname",
            address.port(),
        ),
        address.ip(),
        &owner.user_id(),
    );
    let server = inbound_server(peer, listener);
    assert!(
        owner
            .create_account_channel_invited_if(
                "Must not invite a replacement account",
                vec![advertised_account.account_id()],
                |commit| commit(),
            )
            .await
            .is_err()
    );
    assert!(owner.list_channels().is_empty());
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn new_account_group_preflight_has_one_deadline_and_creates_no_partial_group() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path(), &Account::generate());
    let invited = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    for _ in 0..6 {
        owner.roster.lock().unwrap().update(
            &Announce::new_with_account(
                &DeviceIdentity::generate(),
                &invited,
                "Pending account verification",
                address.port(),
            ),
            address.ip(),
            &owner.user_id(),
        );
    }
    // Hold real asynchronous dial admission. Every route would otherwise get
    // a fresh five-second query timeout, multiplying the caller's lifetime.
    let _policy = owner.privacy.gate.write().await;
    let committed = std::cell::Cell::new(false);
    let result = tokio::time::timeout(
        crate::node::route_timing::MAX_AUTHENTICATION_DEADLINE + std::time::Duration::from_secs(1),
        owner.create_account_channel_invited_if(
            "Must not create a partial group",
            vec![invited.account_id()],
            |commit| {
                committed.set(true);
                commit()
            },
        ),
    )
    .await
    .expect("all target verification must share one authentication deadline");
    assert!(matches!(result, Err(crate::node::NodeError::Channel(_))));
    assert!(
        !committed.get(),
        "expired verification must not enter host admission"
    );
    assert!(owner.list_channels().is_empty());
    assert!(owner.log.lock().unwrap().conversations().is_empty());
}

#[tokio::test]
async fn new_account_group_crash_before_publication_cannot_leave_device_only_genesis() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path(), &Account::generate());
    owner
        .log
        .lock()
        .unwrap()
        .before_rewrite_rename(Box::new(|| {
            panic!("injected process loss before atomic genesis publication");
        }));
    let result = tokio::spawn(async move {
        owner
            .create_account_channel_invited_if("Atomic account group", vec![], |commit| commit())
            .await
    })
    .await;
    assert!(
        result.is_err_and(|error| error.is_panic()),
        "creation must use the atomic genesis publication boundary"
    );
    let reopened =
        crate::eventlog::persist::PersistentEventLog::open(&dir.path().join("events.log"), "pw")
            .unwrap();
    assert!(
        reopened.conversations().is_empty(),
        "a failed new group must not restart as a device-only group"
    );
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
    pull_group_control(&reference, &member, member_address, genesis.conversation_id).await;
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
            pull_group_control(&reference, &owner, owner_address, genesis.conversation_id).await;
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
        pull_group_control(&member, &owner, owner_address, genesis.conversation_id).await;
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
        pull_group_control(&reference, &member, member_address, genesis.conversation_id).await;
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
        pull_group_control(&reference, &owner, owner_address, genesis.conversation_id).await;
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
        pull_group_control(target, &owner, address, genesis.conversation_id).await;
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
    pull_group_control(&member, &owner, address, genesis.conversation_id).await;
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
    pull_group_control(&member, &owner, address, genesis.conversation_id).await;
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

#[tokio::test]
async fn pending_operation_status_only_wakes_sync_when_its_retry_nonce_advances() {
    use futures_util::FutureExt;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = legacy_group(&owner, &external);
    install(&owner, &genesis);
    let id = owner
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let admitted = owner
        .submit_account_group_operation(
            &account.account_id(),
            id,
            genesis.conversation_id,
            AccountGroupOperation::Rename {
                name: "pending status polling".into(),
            },
        )
        .unwrap();
    assert!(admitted.committed_events.is_empty());
    assert!(
        owner
            .account_sync_notify
            .notified()
            .now_or_never()
            .is_some(),
        "fresh admission must wake the operation owner"
    );
    owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    assert!(
        owner
            .account_sync_notify
            .notified()
            .now_or_never()
            .is_some(),
        "the first status read advances the retry query and wakes synchronization"
    );
    let nonce = owner.operation_query_nonce(id);
    for _ in 0..32 {
        owner
            .account_group_operation_status(&account.account_id(), id)
            .unwrap();
        assert_eq!(owner.operation_query_nonce(id), nonce);
        assert!(
            owner
                .account_sync_notify
                .notified()
                .now_or_never()
                .is_none(),
            "unchanged status query must not repeatedly schedule all-peer reconciliation"
        );
    }
    owner
        .operation_queries
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .1 = std::time::Instant::now() - std::time::Duration::from_secs(2);
    owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    assert_ne!(owner.operation_query_nonce(id), nonce);
    assert!(
        owner
            .account_sync_notify
            .notified()
            .now_or_never()
            .is_some(),
        "an elapsed retry interval must still wake synchronization"
    );
}

#[tokio::test]
async fn reference_owner_rename_commits_one_original_owner_effect_over_noise() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
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
            name: "Before account rename".into(),
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
    let group = genesis.conversation_id;
    for target in [&owner, &member, &external] {
        install(target, &genesis);
    }
    // Keep this an actual legacy group even when all running peers learn modern
    // capability support. The real owner's pending invitation blocks migration.
    let pending = DeviceIdentity::generate();
    let invitation = Event::new(
        &owner.identity,
        group,
        2,
        vec![genesis.id],
        2,
        2,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Before account rename".into(),
            members: vec![
                owner.identity.public(),
                member.identity.public(),
                external.identity.public(),
                pending.public(),
            ],
            epoch: 1,
            owner: owner.user_id(),
        }
        .encode_invitation(),
    );
    for target in [&owner, &member, &external] {
        install(target, &invitation);
    }
    for left in [&owner, &member, &reference] {
        for right in [&owner, &member, &reference] {
            if left.user_id() != right.user_id() {
                link(left, right);
            }
        }
        left.set_device_sync_category(SyncCategory::History, false)
            .unwrap();
    }
    let mut servers = tokio::task::JoinSet::new();
    let mut addresses = Vec::new();
    let mut listeners = Vec::new();
    for (name, target) in [
        ("owner", &owner),
        ("member", &member),
        ("reference", &reference),
        ("external", &external),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push(listener);
    }
    for left in [&owner, &member, &reference, &external] {
        for (right, address) in [&owner, &member, &reference, &external]
            .into_iter()
            .zip(&addresses)
        {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Rename peer", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    // The first scheduler tick must see the real listening routes. This is
    // setup, distinct from the action's unchanged eight-second deadline.
    let mut peer_runtimes = Vec::new();
    let mut reference_launch = None;
    let mut external_runtime = None;
    for (target, listener) in [&owner, &member, &reference, &external]
        .into_iter()
        .zip(listeners)
    {
        if target.user_id() != reference.user_id() {
            let runtime =
                message_action_reference::ActionPeerRuntime::start(target.clone(), listener).await;
            if target.user_id() == external.user_id() {
                external_runtime = Some(runtime.runtime_handle());
            }
            peer_runtimes.push(runtime);
        } else {
            let (launch, launched) = tokio::sync::oneshot::channel();
            let target = target.clone();
            servers.spawn(async move {
                launched.await.unwrap();
                Box::pin(target.run_accept_loop(listener)).await;
            });
            reference_launch = Some(launch);
        }
    }
    for runtime in &mut peer_runtimes {
        runtime.launch();
    }
    if let Some(launch) = reference_launch {
        launch.send(()).unwrap();
    }
    let external_runtime =
        external_runtime.expect("the external owner-rename peer owns its runtime");
    owner.publish_account_group_access(&mut None);
    pull_group_control(&reference, &owner, addresses[0], genesis.conversation_id).await;
    pull_group_control(&member, &owner, addresses[0], genesis.conversation_id).await;
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert_eq!(
        member.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(reference.channel_members(group).is_empty());
    assert!(!reference.channel_can_manage(group));
    cold_account_setup(async {
        loop {
            let requester_ready = reference.group_operation_versions.lock().unwrap()
                .get(&owner.user_id()).is_some_and(|support| support.public == owner.identity.public());
            let source_ready = owner.group_operation_versions.lock().unwrap()
                .get(&reference.user_id()).is_some_and(|support| support.public == reference.identity.public());
            if requester_ready && source_ready { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("cold authenticated capability exchanges must progress in both directions without submitting work or manual polling");
    // Observe only the selected route. Task IDs correlate concurrent lanes
    // without changing the authenticated exchange or adding any traffic.
    let private_trace =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let private_trace_omitted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let private_trace_started = std::time::Instant::now();
    for (label, current, peer) in [
        ("source", &owner, reference.identity.public()),
        ("requester", &reference, owner.identity.public()),
    ] {
        let trace = private_trace.clone();
        let omitted = private_trace_omitted.clone();
        let client_peer = peer.clone();
        let control_tasks = std::sync::Mutex::new(std::collections::VecDeque::new());
        let queue_sample = std::sync::Mutex::new(None::<String>);
        *current.account_client_test_hook.lock().unwrap() =
            Some(std::sync::Arc::new(move |observed, phase| {
                let publication = phase.starts_with("operation_local_published");
                if observed != client_peer && !publication {
                    return;
                }
                let task = tokio::task::try_id();
                if phase.starts_with("negotiation_begin domain=Some(GroupControl)")
                    && let Some(task) = task
                {
                    let mut tasks = control_tasks.lock().unwrap();
                    if !tasks.contains(&task) {
                        if tasks.len() == 16 {
                            tasks.pop_front();
                        }
                        tasks.push_back(task);
                    }
                }
                let selected_task =
                    task.is_some_and(|task| control_tasks.lock().unwrap().contains(&task));
                if !publication
                    && !phase.starts_with("wake_")
                    && !phase.starts_with("control_")
                    && !(selected_task
                        && (phase.starts_with("negotiation_")
                            || phase.starts_with("session_")
                            || phase.starts_with("dial_")))
                {
                    return;
                }
                let safe = phase
                    .split_ascii_whitespace()
                    .enumerate()
                    .filter_map(|(index, token)| {
                        (index == 0
                            || [
                                "outcome=",
                                "domain=",
                                "elapsed_ms=",
                                "retry_ms=",
                                "pending=",
                                "after=",
                                "deferred=",
                                "reused=",
                                "priority=",
                                "revision=",
                                "selected=",
                                "hint=",
                                "ready=",
                                "active_same=",
                                "active_other=",
                                "protected=",
                            ]
                            .iter()
                            .any(|prefix| token.starts_with(prefix)))
                        .then_some(token)
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if phase.starts_with("control_queue") {
                    let normalized = safe
                        .split_ascii_whitespace()
                        .map(|token| {
                            if token.starts_with("retry_ms=") && token != "retry_ms=0" {
                                "retry_ms=pending"
                            } else {
                                token
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let mut previous = queue_sample.lock().unwrap();
                    if previous.as_ref() == Some(&normalized) {
                        return;
                    }
                    *previous = Some(normalized);
                }
                let mut trace = trace.lock().unwrap();
                if trace.len() == 256 {
                    trace.pop_front();
                    omitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                trace.push_back((
                    private_trace_started.elapsed().as_millis(),
                    label,
                    task,
                    safe,
                ));
            }));
        let trace = private_trace.clone();
        let omitted = private_trace_omitted.clone();
        let channel_key = hex::encode(group.as_bytes());
        *current.account_session_test_hook.lock().unwrap() = Some(std::sync::Arc::new(
            move |observed, frame| {
                let trace = trace.clone();
                let omitted = omitted.clone();
                let channel_key = channel_key.clone();
                let expected = peer.clone();
                Box::pin(async move {
                    if observed != expected {
                        return;
                    }
                    use crate::node::account_sync_wire::AccountHistoryFrame;
                    let record = match frame {
                    AccountHistoryFrame::DocumentsRequest { body } => crate::node::account_documents::wire::DocumentRequest::decode(&body)
                        .filter(|request| request.domain == crate::node::AccountDocumentDomain::GroupControl)
                        .map(|request| format!("server_request after={} known_zero={} operation_nonce={:?}", request.after.is_some(), request.known_digest == [0;32], request.operation.map(|probe| hex::encode(probe.nonce)))),
                    AccountHistoryFrame::DocumentHeader { body } => serde_json::from_slice::<crate::node::AccountDocumentMeta>(&body).ok().and_then(|meta| {
                        let (kind, channel, source_device) = match &meta.key {
                            crate::node::AccountDocumentKey::GroupOperation { channel, source_device } => ("operation", channel, source_device),
                            crate::node::AccountDocumentKey::GroupControl { channel, source_device } => ("role", channel, source_device),
                            _ => return None,
                        };
                        (channel == &channel_key).then(|| format!("server_header kind={kind} source={source_device} revision={} hash={}", meta.revision, hex::encode(meta.content_hash)))
                    }),
                    AccountHistoryFrame::DocumentsComplete { body } => serde_json::from_slice::<serde_json::Value>(&body).ok()
                        .filter(|complete| complete["domain"] == "group_control")
                        .map(|complete| format!("server_complete next={} digest={} operation_nonce={}", !complete["next"].is_null(), complete["digest"], complete["operation"]["nonce"])),
                    _ => None,
                };
                    if let Some(record) = record {
                        let mut trace = trace.lock().unwrap();
                        if trace.len() == 256 {
                            trace.pop_front();
                            omitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        trace.push_back((
                            private_trace_started.elapsed().as_millis(),
                            label,
                            tokio::task::try_id(),
                            record,
                        ));
                    }
                })
            },
        ));
    }
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let submitted = reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "  After account rename  ".into(),
            },
            |commit| commit(),
        )
        .expect(
            "a certified reference-only owner sibling must durably submit the account operation",
        );
    assert!(matches!(
        submitted.phase,
        AccountGroupOperationPhase::WaitingForExecutor
            | AccountGroupOperationPhase::AwaitingAcceptance
            | AccountGroupOperationPhase::Accepted
    ));
    assert!(
        submitted.committed_events.is_empty(),
        "source acceptance is not a fabricated public EventId"
    );
    let private_started = std::time::Instant::now();
    let committed = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let status = reference
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            if let Some(status) = status
                && status.phase == AccountGroupOperationPhase::Committed
                && status.committed_events.len() == 1
                && owner.account_group_operation_status(&account.account_id(), id).unwrap()
                    .is_some_and(|source| source.phase == AccountGroupOperationPhase::Committed
                        && source.committed_events == status.committed_events)
                && reference.account_group_operation_inputs().into_iter().any(|(_, _, frame)| {
                    frame.group == group && frame.proof.public() == owner.identity.public()
                        && frame.projection.results.iter().any(|result| result.reference.id == id
                            && result.reference.group == group
                            && result.reference.origin.public() == reference.identity.public()
                            && result.reference.executor.device == owner.identity.public()
                            && result.status.phase == AccountGroupOperationPhase::Committed
                            && result.status.committed_events == status.committed_events)
                })
            {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let requester_phase = reference.account_group_operation_status(&account.account_id(), id)
            .unwrap().map(|status| status.phase);
        let source_phase = owner.account_group_operation_status(&account.account_id(), id)
            .unwrap().map(|status| status.phase);
        let requester_projection = reference.operation_projection(group);
        let source_projection = owner.operation_projection(group);
        let operation_metadata: Vec<_> = [("source", &owner), ("requester", &reference)].into_iter().map(|(label, node)| {
            let versions: Vec<_> = [("source", owner.user_id()), ("requester", reference.user_id())].into_iter().map(|(writer, source_device)| {
                let meta = node.account_document_meta(&crate::node::AccountDocumentKey::GroupOperation { channel: hex::encode(group.as_bytes()), source_device });
                (writer, meta.map(|meta| (meta.revision, meta.content_hash)))
            }).collect();
            (label, versions, node.sessions.test_capacity_snapshot())
        }).collect();
        eprintln!("owner rename private metadata={operation_metadata:?} omitted={} phases={:?}", private_trace_omitted.load(std::sync::atomic::Ordering::Relaxed), private_trace.lock().unwrap());
        panic!("real owner rename did not converge: requester={requester_phase:?}, source={source_phase:?}, request_heads={}, result_heads={}, requester_ready={}, source_ready={}",
            requester_projection.requests.len(), source_projection.results.len(),
            reference.group_operation_versions.lock().unwrap().contains_key(&owner.user_id()),
            owner.group_operation_versions.lock().unwrap().contains_key(&reference.user_id()));
    });
    for current in [&owner, &reference] {
        *current.account_client_test_hook.lock().unwrap() = None;
        *current.account_session_test_hook.lock().unwrap() = None;
    }
    eprintln!(
        "owner rename private receipt elapsed_ms={}",
        private_started.elapsed().as_millis()
    );
    // Durable private commitment does not certify public fanout or directory
    // arrival. Observe those through their existing authenticated owners after
    // the original eight-second private request/result barrier has completed.
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        let observing = external.clone();
        let owner_public = owner.identity.public();
        let public_receipt = async move {
            let peer = observing.routing_peer(&owner_public.user_id()).unwrap();
            assert_eq!(peer.public, owner_public);
            let mut connection = observing
                .privacy_sync_dial(peer.addr, &peer.public)
                .await
                .expect("the real external member must authenticate the original owner");
            let store = observing.pull_sync_store(connection.peer_identity());
            crate::node::session::request_round(&mut connection, &store, group)
                .await
                .expect("authorized public sync must retain the original owner rename");
            drop(store);
            observing.recycle_sync_channel(connection);
            observing.emit_new_messages(group);
        };
        external_runtime.spawn(public_receipt).await.unwrap();
        loop {
            if reference
                .channel_shared_reference(group)
                .is_some_and(|view| {
                    view.name == "After account rename" && !view.directory_reconciling
                })
                && external
                    .channels
                    .lock()
                    .unwrap()
                    .state(&group)
                    .is_some_and(|state| state.name() == "After account rename")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("public rename receipt and private directory observation must converge independently");
    let events: Vec<_> = external
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .filter(|event| event.kind == EventKind::ChannelRename)
        .cloned()
        .collect();
    assert_eq!(events.len(), 1);
    assert!(events[0].verify_signature() && events[0].verify_integrity());
    assert_eq!(
        events[0].author.ed25519_pub(),
        &owner.identity.public().ed25519_pub
    );
    assert_eq!(committed.committed_events, vec![events[0].id]);
    let duplicate = reference
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "After account rename".into(),
            },
        )
        .unwrap();
    assert_eq!(duplicate.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(duplicate.committed_events, committed.committed_events);
    assert!(
        reference.channel_members(group).is_empty(),
        "delegation cannot install a public membership grant"
    );
    reference.runtime_work.close();
    servers.abort_all();
    while servers.join_next().await.is_some() {}
    reference.sessions.clear();
    reference.runtime_work.drain().await;
    for runtime in peer_runtimes {
        runtime.stop().await;
    }
}

#[test]
fn retired_session_cannot_admit_an_account_group_operation_for_the_same_account() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = legacy_group(&owner, &external);
    install(&owner, &genesis);
    let id = owner
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let before = owner
        .log
        .lock()
        .unwrap()
        .fingerprint_and_len(&genesis.conversation_id);
    let result = owner.submit_account_group_operation_if(
        &account.account_id(),
        id,
        genesis.conversation_id,
        AccountGroupOperation::Rename {
            name: "Retired session rename".into(),
        },
        |_| {
            Err(crate::node::NodeError::Authorization(
                "runtime owner retired".into(),
            ))
        },
    );
    assert!(matches!(
        result,
        Err(crate::node::NodeError::Authorization(_))
    ));
    assert!(
        owner
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        owner
            .log
            .lock()
            .unwrap()
            .fingerprint_and_len(&genesis.conversation_id),
        before
    );
    assert!(
        owner
            .list_account_group_operations(
                &account.account_id(),
                AccountGroupOperationQuery {
                    group: Some(genesis.conversation_id),
                    pending_only: true,
                    after: None,
                    limit: 16,
                }
            )
            .unwrap()
            .is_empty()
    );
}

#[path = "tests/mandatory_control_latency.rs"]
mod mandatory_control_latency;
#[path = "tests/mandatory_latency.rs"]
mod mandatory_latency;
#[path = "tests/optional_custody_latency.rs"]
mod optional_custody_latency;

#[tokio::test]
async fn modern_inherited_owner_executes_rename_through_the_account_facade() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let sibling = node(&dir.path().join("sibling"), &account);
    let group = owner
        .create_channel("Modern inherited owner", vec![])
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
    link(&owner, &sibling);
    link(&sibling, &owner);
    for event in owner.log.lock().unwrap().events(&group) {
        install(&sibling, event);
    }
    // Use the existing signed inheritance control against the real account
    // grant produced above. No physical member or authority is invented.
    let projection = sibling.account_group_projection(group);
    let authority = projection.authority.current_event().unwrap();
    let grant = projection
        .authority
        .account_grant(&account.public().ed25519_pub)
        .and_then(crate::node::channel_account_authority::AccountGrant::id)
        .unwrap();
    let inheritance = crate::node::channel_account_bootstrap::DeviceControl::Inherit {
        authority,
        grant,
        device: sibling.own_group_proof(),
        account_signature: account
            .sign(
                &crate::node::channel_account_bootstrap::inheritance_account_message(
                    group,
                    authority,
                    grant,
                    &sibling.identity.public(),
                ),
            )
            .to_vec(),
    };
    sibling
        .append_event(
            group,
            EventKind::ChannelDeviceInheritance,
            inheritance.encode(),
        )
        .unwrap();
    sibling.process_channel(group);
    assert!(
        sibling.channel_can_manage(group),
        "the existing modern authority owner must authorize its inherited sibling"
    );
    assert_ne!(sibling.channel_owner(group), sibling.user_id());
    let id = sibling
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    sibling
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "Modern account renamed".into(),
            },
        )
        .unwrap();
    sibling.maintain_account_group_operations().await;
    assert_eq!(
        sibling
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap()
            .phase,
        AccountGroupOperationPhase::Committed,
        "a current account owner may execute through its inherited public device grant"
    );
    assert_eq!(
        sibling
            .channels
            .lock()
            .unwrap()
            .state(&group)
            .unwrap()
            .name(),
        "Modern account renamed"
    );
}

#[path = "tests/simple_controls.rs"]
mod simple_controls;

#[path = "tests/recovered_fanout.rs"]
mod recovered_fanout;

#[path = "tests/messages.rs"]
mod messages;

#[path = "tests/lock_order.rs"]
mod lock_order;

#[path = "tests/owner_targets.rs"]
mod owner_targets;

#[path = "tests/files.rs"]
mod files;

#[path = "tests/result_lifecycle.rs"]
mod result_lifecycle;

#[path = "tests/legacy_leave_reference.rs"]
mod legacy_leave_reference;

#[path = "tests/selected_source_control_latency.rs"]
mod selected_source_control_latency;

#[path = "tests/selected_source_request_latency.rs"]
mod selected_source_request_latency;

mod selected_same_source_control_latency;

mod selected_queued_source_control_latency;
