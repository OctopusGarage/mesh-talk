//! Account-targeted owner controls exercise the durable operation facade.
use super::*;
use crate::{
    eventlog::{ConversationId, EventId},
    identity::device::PublicIdentity,
    node::{AccountGroupOperation, AccountGroupOperationStatus},
};
use std::collections::HashSet;
use tokio::task::JoinSet;

struct Fixture {
    // Dropping the task set aborts accept loops and their owned connection sets,
    // including when an admission or assertion panics.
    _servers: JoinSet<()>,
    _dir: tempfile::TempDir,
    nodes: Vec<Arc<Node>>,
    addresses: Vec<std::net::SocketAddr>,
    group: ConversationId,
}

fn serve(servers: &mut JoinSet<()>, node: Arc<Node>, listener: TcpListener) {
    servers.spawn(async move {
        let mut connections = JoinSet::new();
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
    });
}

async fn fixture(modern: bool) -> Fixture {
    fixture_with_owner_sibling(modern, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_invitation_runs_in_existing_data_workers_and_reuses_its_original_id() {
    let mut fixture = fixture(true).await;
    let owner = fixture.nodes[0].clone();
    let target = fixture.nodes[3].clone();
    let account = owner.account_id();
    let group = fixture.group;
    let old_grant = owner
        .account_group_projection(group)
        .authority
        .account_grant(&target.account.public().ed25519_pub)
        .unwrap()
        .id()
        .unwrap();
    {
        let _membership = owner.channel_membership_commit.lock().unwrap();
        owner
            .try_account_channel_remove(group, &target.account_id())
            .unwrap();
    }
    owner
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    owner
        .set_device_sync_category(SyncCategory::Attachments, false)
        .unwrap();
    let id = owner.new_account_group_operation_id(&account).unwrap();
    let operation = AccountGroupOperation::Invite {
        target_account_id: target.account_id(),
    };
    owner
        .submit_account_group_operation(&account, id, group, operation.clone())
        .unwrap();
    fixture
        ._servers
        .spawn(owner.clone().run_account_history_loop());
    let status = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let status = owner
                .account_group_operation_status(&account, id)
                .unwrap()
                .unwrap();
            if status.phase == AccountGroupOperationPhase::Committed {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the existing data worker must commit the exact original invitation");
    assert!(status.primary_event.is_none());
    assert_eq!(status.committed_events.len(), 2);
    let projection = owner.account_group_projection(group);
    let grant = projection
        .authority
        .account_grant(&target.account.public().ed25519_pub)
        .unwrap();
    assert!(grant.pending);
    assert_ne!(grant.id(), Some(old_grant));
    let snapshot = owner.log.lock().unwrap().fingerprint_and_len(&group);
    assert_eq!(
        owner
            .submit_account_group_operation(&account, id, group, operation)
            .unwrap(),
        status
    );
    assert_eq!(
        owner.log.lock().unwrap().fingerprint_and_len(&group),
        snapshot
    );
}

#[tokio::test]
async fn retired_source_cannot_allocate_an_admitted_account_control() {
    let fixture = fixture(true).await;
    let owner = &fixture.nodes[0];
    let account = owner.account_id();
    let id = owner.new_account_group_operation_id(&account).unwrap();
    owner
        .submit_account_group_operation(
            &account,
            id,
            fixture.group,
            AccountGroupOperation::Rename {
                name: "retired runtime must not publish".into(),
            },
        )
        .unwrap();
    let request = owner
        .operation_projection(fixture.group)
        .requests
        .into_iter()
        .find(|request| request.reference.id == id)
        .unwrap();
    owner.admit_account_group_request_for_test(request).unwrap();
    let before = owner
        .log
        .lock()
        .unwrap()
        .fingerprint_and_len(&fixture.group);
    owner.runtime_work.close();
    owner.maintain_account_group_operation_effects();
    assert_eq!(
        owner
            .log
            .lock()
            .unwrap()
            .fingerprint_and_len(&fixture.group),
        before,
        "retirement must close effect allocation after asynchronous source preparation"
    );
    assert_ne!(
        owner
            .account_group_operation_status(&account, id)
            .unwrap()
            .unwrap()
            .phase,
        AccountGroupOperationPhase::Committed
    );
}

#[tokio::test]
async fn unchanged_source_admission_preserves_original_journal_revision() {
    let fixture = fixture(true).await;
    let owner = &fixture.nodes[0];
    let account = owner.account_id();
    let id = owner.new_account_group_operation_id(&account).unwrap();
    owner
        .submit_account_group_operation(
            &account,
            id,
            fixture.group,
            AccountGroupOperation::Rename {
                name: "same original request".into(),
            },
        )
        .unwrap();
    let request = owner
        .operation_projection(fixture.group)
        .requests
        .into_iter()
        .find(|request| request.reference.id == id)
        .unwrap();
    owner
        .admit_account_group_request_for_test(request.clone())
        .unwrap();
    let revision = owner.group_operation_journal_revision_for_test(id);
    for _ in 0..3 {
        owner
            .admit_account_group_request_for_test(request.clone())
            .unwrap();
    }
    assert_eq!(
        owner.group_operation_journal_revision_for_test(id),
        revision,
        "reobserving an unchanged original request is not new source progress"
    );
}

async fn fixture_with_owner_sibling(modern: bool, sibling: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let owner_account = Account::generate();
    let target_account = Account::generate();
    let foreign_account = Account::generate();
    let mut nodes = vec![
        node(&dir.path().join("owner"), &owner_account),
        node(&dir.path().join("target-first"), &target_account),
        node(&dir.path().join("target-second"), &target_account),
        node(&dir.path().join("foreign"), &foreign_account),
    ];
    if sibling {
        nodes.push(node(&dir.path().join("owner-sibling"), &owner_account));
        link(&nodes[0], &nodes[4]);
        link(&nodes[4], &nodes[0]);
    }
    let mut servers = JoinSet::new();
    let mut addresses = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let privacy = dir.path().join(format!("privacy-{index}"));
        std::fs::create_dir_all(&privacy).unwrap();
        configure(node, &privacy, address.port());
        addresses.push(address);
        serve(&mut servers, node.clone(), listener);
    }
    for observer in &nodes {
        for (peer, address) in nodes.iter().zip(&addresses) {
            if observer.user_id() != peer.user_id() {
                observer.roster.lock().unwrap().update(
                    &Announce::new_with_account(
                        &peer.identity,
                        &peer.account,
                        "Identical display name",
                        address.port(),
                    ),
                    address.ip(),
                    &observer.user_id(),
                );
            }
        }
        observer.cache_discovered_peers().unwrap();
    }
    let owner = &nodes[0];
    let genesis = Event::new(
        &owner.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Account-target owner fixture".into(),
            members: nodes.iter().map(|node| node.identity.public()).collect(),
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    for node in &nodes {
        install(node, &genesis);
    }
    let group = genesis.conversation_id;
    if modern {
        // Use the existing local provenance owner; account authority itself must
        // be produced by maintenance after real Noise capability responses.
        owner.record_channel_owner_origin(group).unwrap();
        let mut work =
            crate::node::channel_account_maintenance::ChannelAccountMaintenance::default();
        for _ in 0..12 {
            owner.maintain_account_channels(&mut work).await;
            if owner
                .account_group_projection(group)
                .authority
                .current_event()
                .is_some()
            {
                break;
            }
        }
        let projection = owner.account_group_projection(group);
        assert!(
            projection.authority.current_event().is_some(),
            "real capability migration prerequisite"
        );
        assert_eq!(projection.authority.grants().len(), 3);
        assert_eq!(projection.accepted.len(), nodes.len());
        for node in &nodes {
            assert!(
                projection
                    .accepted
                    .iter()
                    .any(|member| member.public == node.identity.public())
            );
        }
    }
    let access = owner.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Owner);
    assert_eq!(access.execution, AccountGroupExecution::Local);
    assert!(access.sources.iter().any(|source| {
        source.device == owner.identity.public() && source.role == AccountGroupRole::Owner
    }));
    Fixture {
        _servers: servers,
        _dir: dir,
        nodes,
        addresses,
        group,
    }
}

fn relevant_events(owner: &Node, group: ConversationId, kind: EventKind) -> Vec<Event> {
    owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .filter(|event| event.kind == kind)
        .cloned()
        .collect()
}

fn accepted(owner: &Node, group: ConversationId) -> Vec<PublicIdentity> {
    let projection = owner.account_group_projection(group);
    let state = projection
        .state
        .as_ref()
        .expect("effective membership state");
    let mut members: Vec<_> = state
        .members()
        .iter()
        .filter(|member| state.is_accepted_member(&member.user_id()))
        .cloned()
        .collect();
    members.sort_by_key(|public| (public.user_id(), public.ed25519_pub, public.x25519_pub));
    members
}

async fn commit(
    owner: &Arc<Node>,
    group: ConversationId,
    operation: AccountGroupOperation,
    kind: EventKind,
) -> (AccountGroupOperationStatus, Event, HashSet<EventId>) {
    let baseline = relevant_events(owner, group, kind)
        .into_iter()
        .map(|event| event.id)
        .collect::<HashSet<_>>();
    let account = owner.account_id();
    let id = owner.new_account_group_operation_id(&account).unwrap();
    let admitted = owner
        .submit_account_group_operation(&account, id, group, operation)
        .expect("the owner facade must admit account-targeted controls");
    assert!(admitted.committed_events.is_empty());
    owner.maintain_account_group_operations().await;
    let status = owner
        .account_group_operation_status(&account, id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    let effects: Vec<_> = relevant_events(owner, group, kind)
        .into_iter()
        .filter(|event| !baseline.contains(&event.id))
        .collect();
    assert_eq!(
        effects.len(),
        1,
        "one public effect beyond genesis or migration"
    );
    let effect = effects.into_iter().next().unwrap();
    assert!(effect.verify_integrity() && effect.verify_signature());
    assert_eq!(
        effect.author.ed25519_pub(),
        &owner.identity.public().ed25519_pub
    );
    assert_eq!(status.committed_events, vec![effect.id]);
    assert_eq!(
        status.primary_event, None,
        "administrative controls have no message primary event"
    );
    (status, effect, baseline)
}

async fn retry(
    owner: &Arc<Node>,
    group: ConversationId,
    operation: AccountGroupOperation,
    status: AccountGroupOperationStatus,
    effect: &Event,
    baseline: HashSet<EventId>,
) {
    let account = owner.account_id();
    assert_eq!(
        owner
            .submit_account_group_operation(&account, status.id, group, operation)
            .unwrap(),
        status
    );
    owner.maintain_account_group_operations().await;
    assert_eq!(
        owner
            .account_group_operation_status(&account, status.id)
            .unwrap(),
        Some(status)
    );
    let effects: Vec<_> = relevant_events(owner, group, effect.kind)
        .into_iter()
        .filter(|event| !baseline.contains(&event.id))
        .map(|event| event.id)
        .collect();
    assert_eq!(
        effects,
        vec![effect.id],
        "an exact retry cannot emit another effect"
    );
}

async fn remove_account(modern: bool) {
    let fixture = fixture(modern).await;
    let owner = &fixture.nodes[0];
    let target = &fixture.nodes[1];
    let group = fixture.group;
    let before = owner.account_group_projection(group);
    let before_epoch = before.state.as_ref().unwrap().epoch();
    let target_pub = target.account.public().ed25519_pub;
    let other_grants: Vec<_> = before
        .authority
        .grants()
        .into_iter()
        .filter(|grant| grant.id().unwrap().account_pub != target_pub)
        .collect();
    assert_eq!(accepted(owner, group).len(), 4);
    assert_eq!(target.account_id(), fixture.nodes[2].account_id());
    assert_ne!(target.account_id(), fixture.nodes[3].account_id());
    let operation = AccountGroupOperation::RemoveAccount {
        target_account_id: target.account_id(),
    };
    let kind = if modern {
        EventKind::ChannelAccountAuthority
    } else {
        EventKind::MembershipChange
    };
    let (status, effect, baseline) = commit(owner, group, operation.clone(), kind).await;
    let after = owner.account_group_projection(group);
    let members = accepted(owner, group);
    assert_eq!(members.len(), 2);
    assert!(members.contains(&owner.identity.public()));
    assert!(members.contains(&fixture.nodes[3].identity.public()));
    assert!(!members.contains(&target.identity.public()));
    assert!(!members.contains(&fixture.nodes[2].identity.public()));
    if modern {
        assert!(after.authority.account_grant(&target_pub).is_none());
        assert_eq!(after.authority.grants(), other_grants);
        let control = crate::node::channel_account_authority::AccountAuthorityControl::decode(
            &effect.ciphertext,
        )
        .unwrap();
        assert_eq!(control.previous, before.authority.current_event());
        assert_eq!(after.authority.current_event(), Some(effect.id));
    } else {
        assert_eq!(after.state.as_ref().unwrap().epoch(), before_epoch + 1);
        assert!(after.authority.current_event().is_none());
    }
    retry(owner, group, operation, status, &effect, baseline).await;
}

async fn transfer_ownership(modern: bool) {
    let fixture = fixture(modern).await;
    let owner = &fixture.nodes[0];
    let target = &fixture.nodes[1];
    let group = fixture.group;
    let before = owner.account_group_projection(group);
    let before_epoch = before.state.as_ref().unwrap().epoch();
    let members = accepted(owner, group);
    let candidates: Vec<_> = members
        .iter()
        .filter(|member| {
            owner
                .channel_member_account_binding(group, member)
                .as_deref()
                == Some(target.account_id().as_str())
        })
        .cloned()
        .collect();
    assert_eq!(
        candidates.len(),
        2,
        "the account has two accepted device targets"
    );
    let elected = candidates[0].clone();
    let operation = AccountGroupOperation::TransferOwnership {
        target_account_id: target.account_id(),
    };
    let kind = if modern {
        EventKind::ChannelAccountAuthority
    } else {
        EventKind::ChannelOwnerTransfer
    };
    let (status, effect, baseline) = commit(owner, group, operation.clone(), kind).await;
    let after = owner.account_group_projection(group);
    assert_eq!(accepted(owner, group), members);
    assert_eq!(after.state.as_ref().unwrap().epoch(), before_epoch);
    assert_eq!(
        owner.channel_member_account_binding(group, &elected),
        Some(target.account_id())
    );
    if modern {
        assert_eq!(
            after.authority.owner_account_id(),
            Some(target.account_id())
        );
        assert_eq!(after.authority.grants(), before.authority.grants());
        assert_eq!(after.authority.revoked(), before.authority.revoked());
        let control = crate::node::channel_account_authority::AccountAuthorityControl::decode(
            &effect.ciphertext,
        )
        .unwrap();
        assert_eq!(control.owner_account, target.account.public().ed25519_pub);
        assert_eq!(control.previous, before.authority.current_event());
        assert_eq!(after.authority.current_event(), Some(effect.id));
    } else {
        assert_eq!(owner.channel_owner(group), elected.user_id());
        assert!(
            candidates
                .iter()
                .any(|member| member.user_id() == owner.channel_owner(group))
        );
        assert!(after.authority.current_event().is_none());
    }
    assert_ne!(
        owner.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    retry(owner, group, operation, status, &effect, baseline).await;
}

#[tokio::test]
async fn legacy_owner_removes_all_devices_of_the_target_account() {
    remove_account(false).await;
}

#[tokio::test]
async fn modern_owner_removes_only_the_target_account_grant() {
    remove_account(true).await;
}

#[tokio::test]
async fn legacy_owner_transfers_to_a_deterministic_accepted_target_device() {
    transfer_ownership(false).await;
}

#[tokio::test]
async fn modern_owner_transfers_to_the_target_account_without_changing_grants() {
    transfer_ownership(true).await;
}

/// Synchronize only the public group over the authenticated production session.
async fn public_group_from(
    target: &Arc<Node>,
    source: &Node,
    address: std::net::SocketAddr,
    group: ConversationId,
) {
    let mut channel = target
        .privacy_sync_dial(address, &source.identity.public())
        .await
        .unwrap();
    let store = target.sync_store(channel.peer_identity());
    crate::node::session::request_round(&mut channel, &store, group)
        .await
        .unwrap();
    drop(channel);
    target.process_channel(group);
}

/// Mandatory private documents share the real capability probe and never pull
/// public group events or optional history as a side effect.
async fn private_control_from(target: &Arc<Node>, source: &Node) {
    let peer = target.routing_peer(&source.user_id()).unwrap();
    let mut cursor = crate::node::account_documents::sync::DocumentSyncCursor::mandatory();
    for _ in 0..8 {
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .unwrap();
    }
}

async fn private_ahead_owner_fixture() -> Fixture {
    let fixture = fixture_with_owner_sibling(true, true).await;
    let a = &fixture.nodes[0];
    let b = &fixture.nodes[4];
    let group = fixture.group;
    public_group_from(b, a, fixture.addresses[0], group).await;
    assert!(b.channel_can_manage(group));
    a.publish_account_group_access(&mut None);
    b.publish_account_group_access(&mut None);
    private_control_from(a, b).await;
    private_control_from(b, a).await;
    // The same-account sibling has the genuine current public authority, while
    // the requester deliberately retains its prior public cut.
    let old_authority = a.account_group_projection(group).authority.current_event();
    {
        let _membership = b.channel_membership_commit.lock().unwrap();
        assert!(matches!(
            b.try_account_channel_remove(group, &fixture.nodes[3].account_id())
                .unwrap(),
            crate::node::channel_account_controls::AccountChannelMutation::Changed(_)
        ));
    }
    let new_authority = b.account_group_projection(group).authority.current_event();
    assert_ne!(new_authority, old_authority);
    b.publish_account_group_access(&mut None);
    private_control_from(a, b).await;
    assert_eq!(
        a.account_group_projection(group).authority.current_event(),
        old_authority,
        "the private observation must precede its public authority event"
    );
    let access = a.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Owner);
    assert!(
        access
            .sources
            .iter()
            .any(|source| source.device == a.identity.public()
                && source.role == AccountGroupRole::Owner
                && source.execution == AccountGroupExecution::Reconciling)
    );
    assert!(
        access
            .sources
            .iter()
            .any(|source| source.device == b.identity.public()
                && source.role == AccountGroupRole::Owner
                && source.execution == AccountGroupExecution::ViaDevice(b.identity.public()))
    );
    assert!(
        a.account_group_operation_ready_for(
            &b.identity.public(),
            crate::node::AccountGroupOperationKind::RemoveAccount
        ),
        "real authenticated capability response must advertise the owner control"
    );
    fixture
}

#[tokio::test]
async fn ready_remote_owner_overtakes_a_reconciling_local_owner() {
    let fixture = private_ahead_owner_fixture().await;
    let a = &fixture.nodes[0];
    let b = &fixture.nodes[4];
    let group = fixture.group;
    let account = a.account_id();
    let id = a.new_account_group_operation_id(&account).unwrap();
    let admitted = a
        .submit_account_group_operation(
            &account,
            id,
            group,
            AccountGroupOperation::RemoveAccount {
                target_account_id: fixture.nodes[1].account_id(),
            },
        )
        .unwrap();
    assert_eq!(
        admitted.executions[0].device_id,
        b.user_id(),
        "the ready remote owner must execute while local public authority is reconciling"
    );
    a.publish_account_group_operations();
    private_control_from(b, a).await;
    b.maintain_account_group_operation_effects();
    let source = b
        .account_group_operation_status(&account, id)
        .unwrap()
        .unwrap();
    assert_eq!(source.phase, AccountGroupOperationPhase::Committed);
    private_control_from(a, b).await;
    a.maintain_account_group_operation_effects();
    let result = a
        .account_group_operation_status(&account, id)
        .unwrap()
        .unwrap();
    assert_eq!(result.id, id);
    assert_eq!(result.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(result.committed_events, source.committed_events);
    assert_eq!(result.committed_events.len(), 1);
    public_group_from(a, b, fixture.addresses[4], group).await;
    assert!(
        a.account_group_projection(group)
            .authority
            .account_grant(&fixture.nodes[1].account.public().ed25519_pub)
            .is_none()
    );
}

#[tokio::test]
async fn reconciling_local_owner_without_a_ready_peer_retains_the_original_intent() {
    let fixture = private_ahead_owner_fixture().await;
    let a = &fixture.nodes[0];
    let b = &fixture.nodes[4];
    a.group_operation_versions
        .lock()
        .unwrap()
        .remove(&b.user_id());
    a.roster.lock().unwrap().forget(&b.identity.public());
    let account = a.account_id();
    let id = a.new_account_group_operation_id(&account).unwrap();
    a.submit_account_group_operation(
        &account,
        id,
        fixture.group,
        AccountGroupOperation::RemoveAccount {
            target_account_id: fixture.nodes[1].account_id(),
        },
    )
    .unwrap();
    a.maintain_account_group_operation_effects();
    let status = a
        .account_group_operation_status(&account, id)
        .unwrap()
        .unwrap();
    assert_eq!(status.id, id);
    assert!(
        !matches!(
            status.phase,
            AccountGroupOperationPhase::Rejected { .. }
                | AccountGroupOperationPhase::Cancelled
                | AccountGroupOperationPhase::Committed
        ),
        "missing execution readiness must remain retryable"
    );
    assert!(status.committed_events.is_empty());
    public_group_from(a, b, fixture.addresses[4], fixture.group).await;
    let baseline: HashSet<_> =
        relevant_events(a, fixture.group, EventKind::ChannelAccountAuthority)
            .into_iter()
            .map(|event| event.id)
            .collect();
    a.maintain_account_group_operation_effects();
    let completed = a
        .account_group_operation_status(&account, id)
        .unwrap()
        .unwrap();
    assert_eq!(completed.id, id);
    assert_eq!(completed.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(completed.committed_events.len(), 1);
    assert!(
        a.account_group_projection(fixture.group)
            .authority
            .account_grant(&fixture.nodes[1].account.public().ed25519_pub)
            .is_none()
    );
    a.maintain_account_group_operation_effects();
    assert_eq!(
        a.account_group_operation_status(&account, id).unwrap(),
        Some(completed.clone())
    );
    let effects: Vec<_> = relevant_events(a, fixture.group, EventKind::ChannelAccountAuthority)
        .into_iter()
        .filter(|event| !baseline.contains(&event.id))
        .map(|event| event.id)
        .collect();
    assert_eq!(
        effects, completed.committed_events,
        "the original waiting intent must commit exactly once after public synchronization"
    );
}
