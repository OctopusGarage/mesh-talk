//! Account controls must use the real public producer and durable operation owner.
use super::*;
use crate::{channel::HistoryPolicy, eventlog::ConversationId, node::AccountGroupOperation};
use futures_util::FutureExt;

type ControlFixtureTrace =
    Arc<std::sync::Mutex<std::collections::VecDeque<(u128, &'static str, String)>>>;

fn trace_control_fixture(
    target: &Arc<Node>,
    label: &'static str,
    trace: ControlFixtureTrace,
    started: std::time::Instant,
) {
    fn key_kind(key: &crate::node::AccountDocumentKey) -> &'static str {
        match key {
            crate::node::AccountDocumentKey::GroupOperation { .. } => "operation",
            crate::node::AccountDocumentKey::GroupControl { .. } => "control",
            crate::node::AccountDocumentKey::GroupInventory { .. } => "inventory",
            _ => "optional",
        }
    }
    *target.account_session_test_hook.lock().unwrap() = Some(Arc::new(move |_peer, frame| {
        let trace = trace.clone();
        Box::pin(async move {
            use crate::node::account_sync_wire::AccountHistoryFrame;
            let event = match frame {
                AccountHistoryFrame::DocumentsRequest { body } => {
                    crate::node::account_documents::wire::DocumentRequest::decode(&body).map(|request| {
                        format!("request version={} domain={:?} after={} digest_zero={} probe={} operation_after={} operation_digest_zero={}",
                            request.version, request.domain,
                            request.after.as_ref().map_or("none", key_kind), request.known_digest == [0; 32],
                            request.operation.is_some(), request.operation.as_ref().is_some_and(|probe| probe.after.is_some()),
                            request.operation.as_ref().is_none_or(|probe| probe.known_digest == [0; 32]))
                    })
                }
                AccountHistoryFrame::DocumentHeader { body } => {
                    serde_json::from_slice::<crate::node::AccountDocumentMeta>(&body).ok().map(|meta|
                        format!("header kind={} revision={}", key_kind(&meta.key), meta.revision))
                }
                _ => None,
            };
            if let Some(event) = event {
                let mut trace = trace.lock().unwrap();
                if trace.len() >= 128 {
                    trace.pop_front();
                }
                trace.push_back((started.elapsed().as_millis(), label, event));
            }
        })
    }));
}

async fn owner_fixture(modern: bool) -> (tempfile::TempDir, Account, Arc<Node>, ConversationId) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    if !modern {
        let external = node(&dir.path().join("external"), &Account::generate());
        let genesis = legacy_group(&owner, &external);
        install(&owner, &genesis);
        return (dir, account, owner, genesis.conversation_id);
    }
    let group = owner
        .create_channel("Modern simple control", vec![])
        .await
        .unwrap();
    owner
        .maintain_account_channels(
            &mut crate::node::channel_account_maintenance::ChannelAccountMaintenance::default(),
        )
        .await;
    settle_group_metadata(&owner, group);
    let sibling = node(&dir.path().join("inherited"), &account);
    link(&owner, &sibling);
    link(&sibling, &owner);
    for event in owner.log.lock().unwrap().events(&group).unwrap() {
        install(&sibling, &event);
    }
    let projection = sibling.account_group_projection(group).unwrap();
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
    settle_group_metadata(&sibling, group);
    assert_ne!(sibling.channel_owner(group), sibling.user_id());
    assert!(
        sibling.channel_can_manage(group).unwrap(),
        "real modern account inheritance is a fixture prerequisite"
    );
    (dir, account, sibling, group)
}

async fn assert_local_control(
    modern: bool,
    operation: AccountGroupOperation,
    event_kind: EventKind,
) {
    let (_dir, account, owner, group) = owner_fixture(modern).await;
    assert_eq!(
        owner.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    let id = owner
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let submitted = owner
        .submit_account_group_operation(&account.account_id(), id, group, operation.clone())
        .expect("an account owner must admit this supported control before execution");
    assert!(
        submitted.committed_events.is_empty(),
        "admission cannot invent a public effect"
    );
    owner.maintain_account_group_operations().await;
    let status = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    let effects: Vec<_> = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == event_kind)
        .collect();
    assert_eq!(effects.len(), 1);
    assert!(effects[0].verify_integrity() && effects[0].verify_signature());
    assert_eq!(
        effects[0].author.ed25519_pub(),
        &owner.identity.public().ed25519_pub
    );
    assert_eq!(status.committed_events, vec![effects[0].id]);
    match &operation {
        AccountGroupOperation::SetHistoryPolicy { policy } => {
            assert_eq!(owner.channel_history_policy(group).unwrap(), *policy)
        }
        AccountGroupOperation::Dissolve => assert!(
            owner
                .channels
                .lock()
                .unwrap()
                .state(&group)
                .unwrap()
                .is_dissolved()
        ),
        _ => unreachable!(),
    }
    let retry = owner
        .submit_account_group_operation(&account.account_id(), id, group, operation)
        .unwrap();
    assert_eq!(
        retry, status,
        "an exact retry returns the original terminal even after dissolution"
    );
    owner.maintain_account_group_operations().await;
    assert_eq!(
        owner
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == event_kind)
            .count(),
        1
    );
}

#[tokio::test]
async fn legacy_owner_history_policy_uses_the_account_control_facade() {
    assert_local_control(
        false,
        AccountGroupOperation::SetHistoryPolicy {
            policy: HistoryPolicy::LastTen,
        },
        EventKind::ChannelHistoryPolicy,
    )
    .await;
}
#[tokio::test]
async fn inherited_modern_owner_history_policy_uses_the_account_control_facade() {
    assert_local_control(
        true,
        AccountGroupOperation::SetHistoryPolicy {
            policy: HistoryPolicy::LastThreeDays,
        },
        EventKind::ChannelHistoryPolicy,
    )
    .await;
}
#[tokio::test]
async fn legacy_owner_dissolve_uses_the_account_control_facade() {
    assert_local_control(
        false,
        AccountGroupOperation::Dissolve,
        EventKind::ChannelDissolve,
    )
    .await;
}
#[tokio::test]
async fn inherited_modern_owner_dissolve_uses_the_account_control_facade() {
    assert_local_control(
        true,
        AccountGroupOperation::Dissolve,
        EventKind::ChannelDissolve,
    )
    .await;
}

async fn assert_reference_controls(modern: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let group = if modern {
        let group = owner
            .create_channel("Modern reference controls", vec![])
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
                .unwrap()
                .authority
                .current_event()
                .is_some()
        );
        group
    } else {
        let external = node(&dir.path().join("external"), &Account::generate());
        let genesis = legacy_group(&owner, &external);
        install(&owner, &genesis);
        let invitation = Event::new(
            &owner.identity,
            genesis.conversation_id,
            2,
            vec![genesis.id],
            2,
            2,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Owner account fixture".into(),
                members: vec![
                    owner.identity.public(),
                    external.identity.public(),
                    DeviceIdentity::generate().public(),
                ],
                epoch: 1,
                owner: owner.user_id(),
            }
            .encode_invitation(),
        );
        install(&owner, &invitation);
        genesis.conversation_id
    };
    link(&owner, &reference);
    link(&reference, &owner);
    let mut servers = Vec::new();
    let mut addresses = Vec::new();
    for (name, target) in [("owner", &owner), ("reference", &reference)] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        // Full channel recovery legitimately downloads a modern sibling's public
        // authority even with History OFF. Keep this fixture reference-only by
        // running the real Noise server and account-operation owner directly.
        servers.push(inbound_server(target.clone(), listener));
        servers.push(tokio::spawn(target.clone().run_account_history_loop()));
        servers.push(tokio::spawn(target.clone().run_account_wake_loop()));
    }
    for (left, right, address) in [
        (&owner, &reference, addresses[1]),
        (&reference, &owner, addresses[0]),
    ] {
        left.roster.lock().unwrap().update(
            &right.signed_announce("Simple control peer", address.port()),
            address.ip(),
            &left.user_id(),
        );
    }
    let document_trace: ControlFixtureTrace =
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let trace_started = std::time::Instant::now();
    trace_control_fixture(&owner, "owner", document_trace.clone(), trace_started);
    trace_control_fixture(
        &reference,
        "reference",
        document_trace.clone(),
        trace_started,
    );
    owner.publish_account_group_access(&mut None);
    super::cold_account_control_setup(
        super::observe_account_fixture_condition(|| {
            [(&owner, &reference), (&reference, &owner)]
                .into_iter()
                .all(|(node, peer)| {
                    [
                        AccountGroupOperationKind::HistoryPolicy,
                        AccountGroupOperationKind::Dissolve,
                    ]
                    .into_iter()
                    .all(|kind| super::has_account_group_operation_support(node, peer, Some(kind)))
                })
        }),
        pull_group_control(&reference, &owner, addresses[0], group),
    )
    .await;
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(reference.channel_members(group).is_empty());
    for (operation, kind) in [
        (
            AccountGroupOperation::SetHistoryPolicy {
                policy: HistoryPolicy::LastTen,
            },
            EventKind::ChannelHistoryPolicy,
        ),
        (AccountGroupOperation::Dissolve, EventKind::ChannelDissolve),
    ] {
        assert!(
            reference.channel_members(group).is_empty(),
            "private role and operation synchronization must preserve the reference-only fixture"
        );
        let id = reference
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        let admitted = reference
            .submit_account_group_operation(&account.account_id(), id, group, operation.clone())
            .expect("a reference-only account owner must admit the control");
        assert!(admitted.committed_events.is_empty());
        assert_eq!(admitted.executions.len(), 1);
        let executor = if admitted.executions[0].device_id == owner.user_id() {
            &owner
        } else {
            assert!(
                modern,
                "legacy reference controls must retain the original owner source"
            );
            assert_eq!(admitted.executions[0].device_id, reference.user_id());
            &reference
        };
        // A modern sibling may acquire the real canonical control history while
        // this test runs. In that case its account grant can legitimately make
        // it the next local executor; a private role observation alone cannot.
        let projection = executor.account_group_projection(group).unwrap();
        let canonical_source = Node::public_account_group_source(
            group,
            &projection,
            &executor.identity.public(),
            &executor.own_group_proof(),
        )
        .expect("the pinned executor must possess real current public authority");
        assert_eq!(canonical_source.role, AccountGroupRole::Owner);
        assert_eq!(canonical_source.device, executor.identity.public());
        if modern {
            assert!(canonical_source.authority.authority.is_some());
        }
        let status = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                if let Some(status) = reference
                    .account_group_operation_status(&account.account_id(), id)
                    .unwrap()
                    && status.phase == AccountGroupOperationPhase::Committed
                {
                    break status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            let requester = reference.account_group_operation_status(&account.account_id(), id).unwrap().map(|status| status.phase);
            let source = executor.account_group_operation_status(&account.account_id(), id).unwrap().map(|status| status.phase);
            let source_projection = executor.operation_projection(group).unwrap();
            let source_heads = (source_projection.requests.len(), source_projection.results.len());
            let effects = executor.log.lock().unwrap().events(&group).unwrap().into_iter().filter(|event| event.kind == kind).count();
            let remote_key = crate::node::AccountDocumentKey::GroupOperation {
                channel: hex::encode(group.as_bytes()), source_device: executor.user_id(),
            };
            let remote_revision = reference.account_document_meta(&remote_key).map(|meta| meta.revision);
            let published_source_revision = executor.account_document_meta(&remote_key).map(|meta| meta.revision);
            let requester_gate = reference.delivery_suspended.load(std::sync::atomic::Ordering::Acquire);
            let source_gate = executor.delivery_suspended.load(std::sync::atomic::Ordering::Acquire);
            let document_trace = document_trace.lock().unwrap().clone();
            let remote_frame = reference.account_document(&remote_key).ok().flatten().and_then(|document| {
                crate::node::account_group::operations::wire::OperationFrame::decode(document.payload(), &account.public().ed25519_pub)
            });
            let current_binding = remote_frame.as_ref().map(|frame| reference.account_group_binding_current(&frame.proof));
            let remote_result = remote_frame.as_ref().and_then(|frame| frame.projection.results.iter().find(|result| result.reference.id == id));
            let remote_result_phase = remote_result.map(|result| result.status.phase.clone());
            let pending_reference = reference.operation_projection(group).unwrap().requests.into_iter().find(|request| request.reference.id == id).map(|request| request.reference);
            let reference_match = remote_result.zip(pending_reference.as_ref()).map(|(result, pending)| {
                serde_json::to_vec(&result.reference).unwrap() == serde_json::to_vec(pending).unwrap()
            });
            let input_current = reference.account_group_operation_inputs().iter().any(|(_, _, frame)| {
                frame.group == group && frame.proof.public() == executor.identity.public()
            });
            let requester_sync: Vec<_> = reference.device_sync_status().into_iter()
                .map(|status| (status.phase, status.document_phase, status.document_requires_upgrade, status.document_error)).collect();
            let source_sync: Vec<_> = executor.device_sync_status().into_iter()
                .map(|status| (status.phase, status.document_phase, status.document_requires_upgrade, status.document_error)).collect();
            panic!("reference control did not converge: modern={modern}, kind={:?}, requester={requester:?}, source={source:?}, source_heads={source_heads:?}, effects={effects}, executor_is_owner={}, requester_is_executor={}, requester_ready={}, source_ready={}, requester_sync={requester_sync:?}, source_sync={source_sync:?}, remote_revision={remote_revision:?}, published_source_revision={published_source_revision:?}, requester_gate={requester_gate}, source_gate={source_gate}, document_trace={document_trace:?}, remote_result={remote_result_phase:?}, reference_match={reference_match:?}, current_binding={current_binding:?}, input_current={input_current}", operation.kind(),
                executor.identity.public() == owner.identity.public(), reference.identity.public() == executor.identity.public(),
                reference.identity.public() == executor.identity.public() || reference.group_operation_versions.lock().unwrap().contains_key(&executor.user_id()),
                executor.identity.public() == reference.identity.public() || executor.group_operation_versions.lock().unwrap().contains_key(&reference.user_id()));
        });
        assert_eq!(status.executions.len(), 1);
        assert_eq!(
            status.executions[0].device_id,
            executor.user_id(),
            "completion must retain its original pinned executor"
        );
        let effects: Vec<_> = executor
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == kind)
            .collect();
        assert_eq!(
            effects.len(),
            1,
            "modern={modern}, kind={kind:?}, status={status:?}, actual_policy={:?}",
            owner.channel_history_policy(group).unwrap()
        );
        assert!(effects[0].verify_integrity() && effects[0].verify_signature());
        assert_eq!(
            effects[0].author.ed25519_pub(),
            &executor.identity.public().ed25519_pub
        );
        assert_eq!(status.committed_events, vec![effects[0].id]);
        assert_eq!(
            reference
                .submit_account_group_operation(&account.account_id(), id, group, operation)
                .unwrap(),
            status
        );
    }
    if !modern {
        assert!(
            reference.channel_members(group).is_empty(),
            "legacy delegated controls do not invent a public grant"
        );
    }
    for server in &servers {
        server.abort();
    }
    for server in servers {
        let _ = server.await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_reference_owner_policy_and_dissolve_commit_real_source_effects_over_noise() {
    assert_reference_controls(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modern_reference_owner_policy_and_dissolve_commit_real_source_effects_over_noise() {
    assert_reference_controls(true).await;
}

/// A peer accepts TCP but never answers Noise, exercising the real stalled fanout.
/// Durable private results and canonical source metadata must already be visible.
async fn assert_private_publication_precedes_slow_fanout(operation: AccountGroupOperation) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = legacy_group(&owner, &external);
    let group = genesis.conversation_id;
    install(&owner, &genesis);
    let owner_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(
        &owner,
        &dir.path().join("owner"),
        owner_listener.local_addr().unwrap().port(),
    );
    let stalled_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = stalled_listener.local_addr().unwrap();
    owner.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &external.identity,
            &external.account,
            "Stalled member",
            address.port(),
        ),
        address.ip(),
        &owner.user_id(),
    );
    owner.publish_account_group_access(&mut None);
    owner.publish_group_inventory(&mut None);
    let id = owner
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    owner
        .submit_account_group_operation(&account.account_id(), id, group, operation.clone())
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    let source = owner.clone();
    tasks.spawn(async move { source.maintain_account_group_operations().await });
    let observation = std::panic::AssertUnwindSafe(async {
        let (_held_connection, _) = super::cold_account_setup(stalled_listener.accept())
            .await
            .expect("real fanout must reach the stalled member")
            .unwrap();
        assert!(
            tasks.try_join_next().is_none(),
            "fanout must remain in progress"
        );
        let status = owner
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
        let published = owner
            .account_document(&crate::node::AccountDocumentKey::GroupOperation {
                channel: hex::encode(group.as_bytes()),
                source_device: owner.user_id(),
            })
            .unwrap()
            .unwrap();
        let frame = crate::node::account_group::operations::wire::OperationFrame::decode(
            published.payload(),
            &account.public().ed25519_pub,
        )
        .unwrap();
        assert!(
            frame
                .projection
                .results
                .iter()
                .any(|result| result.status == status),
            "the exact durable result must be published before slow fanout"
        );
        let published = owner
            .account_document(&crate::node::AccountDocumentKey::GroupInventory {
                channel: hex::encode(group.as_bytes()),
                source_device: owner.user_id(),
            })
            .unwrap()
            .unwrap();
        let inventory: crate::node::GroupInventoryReference =
            serde_json::from_slice(published.payload()).unwrap();
        match operation {
            AccountGroupOperation::Rename { name } => assert_eq!(
                inventory.name, name,
                "canonical renamed inventory must precede slow fanout"
            ),
            AccountGroupOperation::SetHistoryPolicy { policy } => assert_eq!(
                inventory.observed_history_policy(),
                Some(policy),
                "canonical policy inventory must precede slow fanout"
            ),
            AccountGroupOperation::Dissolve => {
                assert!(
                    inventory.dissolved,
                    "canonical dissolved inventory must precede slow fanout"
                );
                let head = owner
                    .group_control_heads(group)
                    .into_iter()
                    .find(|head| head.proof.public() == owner.identity.public())
                    .unwrap();
                assert_eq!(
                    head.access.source.role,
                    AccountGroupRole::Dissolved,
                    "canonical source authority must precede slow fanout"
                );
            }
            _ => unreachable!(),
        }
    })
    .catch_unwind()
    .await;
    owner.runtime_work.close();
    owner.sessions.clear();
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        owner.runtime_work.drain(),
    )
    .await
    .expect("the publication fixture must drain its actual admitted work");
    if let Err(panic) = observation {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn renamed_inventory_publishes_before_slow_control_fanout() {
    assert_private_publication_precedes_slow_fanout(AccountGroupOperation::Rename {
        name: "Canonical renamed group".into(),
    })
    .await;
}
#[tokio::test]
async fn policy_inventory_publishes_before_slow_control_fanout() {
    assert_private_publication_precedes_slow_fanout(AccountGroupOperation::SetHistoryPolicy {
        policy: HistoryPolicy::LastTen,
    })
    .await;
}
#[tokio::test]
async fn dissolved_authority_publishes_before_slow_control_fanout() {
    assert_private_publication_precedes_slow_fanout(AccountGroupOperation::Dissolve).await;
}

async fn assert_simple_noop_preserves_the_public_log(modern: bool) {
    let (_dir, account, owner, group) = owner_fixture(modern).await;
    let name = owner
        .channels
        .lock()
        .unwrap()
        .state(&group)
        .unwrap()
        .name()
        .to_owned();
    let before: Vec<_> = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .map(|event| event.id)
        .collect();
    for operation in [
        AccountGroupOperation::Rename {
            name: format!("  {name}  "),
        },
        AccountGroupOperation::SetHistoryPolicy {
            policy: owner.channel_history_policy(group).unwrap(),
        },
    ] {
        let id = owner
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        owner
            .submit_account_group_operation(&account.account_id(), id, group, operation.clone())
            .unwrap();
        owner.maintain_account_group_operations().await;
        let status = owner
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
        assert!(status.committed_events.is_empty());
        assert!(status.primary_event.is_none());
        assert_eq!(
            owner
                .submit_account_group_operation(&account.account_id(), id, group, operation)
                .unwrap(),
            status
        );
        owner.maintain_account_group_operations().await;
        let after: Vec<_> = owner
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .map(|event| event.id)
            .collect();
        assert_eq!(
            after, before,
            "canonical no-op and its retry must not allocate a public event"
        );
    }
}

#[tokio::test]
async fn legacy_simple_noop_commits_without_an_invented_public_effect() {
    assert_simple_noop_preserves_the_public_log(false).await;
}
#[tokio::test]
async fn inherited_modern_simple_noop_commits_without_an_invented_public_effect() {
    assert_simple_noop_preserves_the_public_log(true).await;
}

#[tokio::test]
async fn an_awaiting_control_cannot_use_transferred_owner_authority() {
    let (_dir, account, owner, group) = owner_fixture(false).await;
    let next_owner = owner
        .channel_members(group)
        .into_iter()
        .find(|member| member.user_id() != owner.user_id())
        .unwrap();
    let id = owner
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    owner
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "Must not become canonical".into(),
            },
        )
        .unwrap();
    owner
        .transfer_channel_owner(group, &next_owner.user_id())
        .await
        .unwrap();
    assert!(
        !owner.channel_can_manage(group).unwrap(),
        "real public ownership transfer must be a prerequisite"
    );
    let before: Vec<_> = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .map(|event| event.id)
        .collect();
    owner.maintain_account_group_operations().await;
    let status = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(
        status.phase,
        AccountGroupOperationPhase::Rejected {
            reason: crate::node::AccountGroupOperationRejection::NotAuthorized,
        }
    );
    assert!(status.committed_events.is_empty());
    let after: Vec<_> = owner
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .map(|event| event.id)
        .collect();
    assert_eq!(
        after, before,
        "a pinned obsolete owner cannot append a public effect"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consecutive_account_controls_progress_while_public_fanout_is_stalled() {
    consecutive_control_fanout_fixture(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consecutive_controls_establish_a_cold_public_hold_before_measuring_progress() {
    consecutive_control_fanout_fixture(true).await;
}

async fn consecutive_control_fanout_fixture(delayed_responder: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = legacy_group(&owner, &external);
    let group = genesis.conversation_id;
    for target in [&owner, &external] {
        install(target, &genesis);
    }
    // A genuine pending invitation keeps this scenario on the legacy producer.
    let invitation = Event::new(
        &owner.identity,
        group,
        2,
        vec![genesis.id],
        2,
        2,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Owner account fixture".into(),
            members: vec![
                owner.identity.public(),
                external.identity.public(),
                DeviceIdentity::generate().public(),
            ],
            epoch: 1,
            owner: owner.user_id(),
        }
        .encode_invitation(),
    );
    for target in [&owner, &external] {
        install(target, &invitation);
    }
    let owner_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let external_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let owner_address = owner_listener.local_addr().unwrap();
    let external_address = external_listener.local_addr().unwrap();
    configure(&owner, &dir.path().join("owner"), owner_address.port());
    configure(
        &external,
        &dir.path().join("external"),
        external_address.port(),
    );
    owner.roster.lock().unwrap().update(
        &external.signed_announce("Stalled member", external_address.port()),
        external_address.ip(),
        &owner.user_id(),
    );
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct StopOnDrop(
        Arc<tokio::sync::Semaphore>,
        Arc<std::sync::atomic::AtomicBool>,
    );
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.1.store(true, std::sync::atomic::Ordering::SeqCst);
            self.0.close();
        }
    }
    let _stop_on_drop = StopOnDrop(release.clone(), stopping.clone());
    let (arrived, stalled_response) = tokio::sync::oneshot::channel::<std::sync::Weak<()>>();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let (begin_response, response_may_start) = tokio::sync::oneshot::channel();
    let responder_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(owner.clone().run_accept_loop(owner_listener));
    let peer = external.clone();
    let expected = owner.identity.public();
    let peer_release = release.clone();
    let peer_stopping = stopping.clone();
    let started = responder_started.clone();
    tasks.spawn(async move {
        if delayed_responder {
            response_may_start.await.expect("the first committed effect must start the controlled responder");
            // A genuine cold Noise/public request begins after the old 2s
            // arrival window, without changing either source progress budget.
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        }
        started.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                incoming = external_listener.accept() => {
                    let (stream, _) = incoming.unwrap();
                    let peer = peer.clone();
                    let release = peer_release.clone();
                    let stopping = peer_stopping.clone();
                    let arrived = arrived.clone();
                    let expected = expected.clone();
                    connections.spawn(Box::pin(async move {
                        let mut channel = match peer.privacy_accept(stream).await {
                            Ok(channel) => channel,
                            Err(_) if stopping.load(std::sync::atomic::Ordering::SeqCst) => return,
                            Err(error) => panic!("public fixture authentication failed before cleanup: {error}"),
                        };
                        assert_eq!(channel.peer_identity(), &expected);
                        let first = match channel.recv().await {
                            Ok(first) => first,
                            Err(_) if stopping.load(std::sync::atomic::Ordering::SeqCst) => return,
                            Err(error) => panic!("public fixture request closed before cleanup: {error}"),
                        };
                        if let Some(request) = crate::node::channel_account_bootstrap::AccountGroupRequest::decode(&first) {
                            peer.serve_account_group_request(&mut channel, request).await;
                        } else {
                            assert!(crate::node::session::requests_response(&first),
                                "the held authenticated stream must be a public sync response");
                            let live = Arc::new(());
                            let signal = arrived.lock().unwrap().take();
                            if let Some(signal) = signal { let _ = signal.send(Arc::downgrade(&live)); }
                            tokio::select! {
                                _ = release.acquire() => {}
                                closed = channel.recv() => {
                                    if !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                                        let outcome = if closed.is_ok() { "unexpected frame" } else { "peer closed" };
                                        panic!("the held public response retired before fixture cleanup: {outcome}");
                                    }
                                }
                            }
                            drop(live);
                        }
                    }));
                }
                joined = connections.join_next(), if !connections.is_empty() => {
                    joined.unwrap().expect("public response fixture connection must not panic");
                }
            }
        }
    });
    let observation = std::panic::AssertUnwindSafe(async {
        let first = owner
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        owner
            .submit_account_group_operation(
                &account.account_id(),
                first,
                group,
                AccountGroupOperation::Rename {
                    name: "First durable name".into(),
                },
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while owner
                .account_group_operation_status(&account.account_id(), first)
                .unwrap()
                .unwrap()
                .phase
                != AccountGroupOperationPhase::Committed
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the first source effect must be durably committed");
        if delayed_responder {
            begin_response
                .send(())
                .expect("the delayed responder owner must remain live");
        }
        let held_stream = super::cold_account_setup(stalled_response)
            .await
            .expect("a real authenticated public sync response must be held")
            .unwrap();
        assert!(
            !delayed_responder || responder_started.load(std::sync::atomic::Ordering::SeqCst),
            "the controlled late responder must finish its actual startup before the public hold"
        );
        assert!(
            held_stream.upgrade().is_some(),
            "the first real public response must still be held before second admission"
        );
        let second = owner
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        owner
            .submit_account_group_operation(
                &account.account_id(),
                second,
                group,
                AccountGroupOperation::Rename {
                    name: "Second durable name".into(),
                },
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while owner
                .account_group_operation_status(&account.account_id(), second)
                .unwrap()
                .unwrap()
                .phase
                != AccountGroupOperationPhase::Committed
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a held public fanout must not block the next mandatory account control");
        assert!(
            held_stream.upgrade().is_some(),
            "the same real public response must remain held until the second committed effect"
        );
        let effects: Vec<_> = owner
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == EventKind::ChannelRename)
            .collect();
        assert_eq!(effects.len(), 2);
        for (id, name) in [
            (first, "First durable name"),
            (second, "Second durable name"),
        ] {
            let status = owner
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .unwrap();
            assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
            let event = effects
                .iter()
                .find(|event| event.ciphertext == name.as_bytes())
                .unwrap();
            assert_eq!(status.committed_events, vec![event.id]);
            assert!(event.verify_integrity() && event.verify_signature());
            assert_eq!(
                event.author.ed25519_pub(),
                &owner.identity.public().ed25519_pub
            );
        }
        // Committed is durable control completion; the final strict authority
        // read independently requires this selected channel's current metadata.
        owner.await_channel_metadata(group).await.unwrap();
        assert!(
            owner
                .account_group_projection(group)
                .unwrap()
                .authority
                .current_event()
                .is_none(),
            "public fanout isolation cannot manufacture a modern account grant"
        );
    })
    .catch_unwind()
    .await;
    stopping.store(true, std::sync::atomic::Ordering::SeqCst);
    release.close();
    for current in [&owner, &external] {
        current.runtime_work.close();
        current.sessions.clear();
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    for current in [&owner, &external] {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            current.runtime_work.drain(),
        )
        .await
        .expect("the public hold fixture must drain its actual admitted work");
    }
    if let Err(panic) = observation {
        std::panic::resume_unwind(panic);
    }
}
