//! Explicit control receipts must not wait behind a held account directory page.
use super::*;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

struct ReleaseDirectoryOnDrop(Arc<Semaphore>);
impl Drop for ReleaseDirectoryOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_source_receipt_overtakes_another_siblings_held_control_over_noise() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let other = node(&dir.path().join("other"), &account);
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
            name: "Before directory hold".into(),
            members: vec![owner.identity.public(), external.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
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
            name: "Before directory hold".into(),
            members: vec![
                owner.identity.public(),
                external.identity.public(),
                pending.public(),
            ],
            epoch: 1,
            owner: owner.user_id(),
        }
        .encode_invitation(),
    );
    for target in [&owner, &external] {
        install(target, &genesis);
        // A real pending invitation keeps this an owner-signed legacy effect.
        install(target, &invitation);
    }
    link(&owner, &reference);
    link(&reference, &owner);
    for (left, right) in [
        (&owner, &other),
        (&other, &owner),
        (&reference, &other),
        (&other, &reference),
    ] {
        link(left, right);
    }
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for (name, target) in [
        ("owner", &owner),
        ("reference", &reference),
        ("external", &external),
        ("other", &other),
    ] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push((target.clone(), listener));
    }
    for left in [&owner, &reference, &external, &other] {
        for (right, address) in [&owner, &reference, &external, &other]
            .into_iter()
            .zip(&addresses)
        {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Mandatory control fixture", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    let mut servers = tokio::task::JoinSet::new();
    for (target, listener) in listeners {
        servers.spawn(target.run_accept_loop(listener));
    }
    owner.publish_account_group_access(&mut None);
    super::message_action_reference::pull_reference_action_control(
        &reference,
        &owner,
        addresses[0],
        group,
    )
    .await;
    super::cold_account_setup(async {
        loop {
            let ready = [&owner, &reference].into_iter().all(|node| {
                let peer = if node.user_id() == owner.user_id() {
                    &reference
                } else {
                    &owner
                };
                node.group_operation_versions
                    .lock()
                    .unwrap()
                    .get(&peer.user_id())
                    .is_some_and(|support| {
                        support.public == peer.identity.public()
                            && support.kinds & AccountGroupOperationKind::Rename.capability_bit()
                                != 0
                    })
            });
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both certified devices must bootstrap real operation support");
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(reference.channel_members(group).is_empty());

    let release = Arc::new(Semaphore::new(0));
    let _release_on_drop = ReleaseDirectoryOnDrop(release.clone());
    let (arrived, held) = oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let checkpoints = Arc::new(Mutex::new(Vec::new()));
    let exact_reference = reference.identity.public();
    *other.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let release = release.clone();
        let checkpoints = checkpoints.clone();
        move |peer, frame| {
            let release = release.clone();
            let arrived = arrived.clone();
            let checkpoints = checkpoints.clone();
            let exact_reference = exact_reference.clone();
            Box::pin(async move {
                if peer != exact_reference {
                    return;
                }
                let AccountHistoryFrame::DocumentsRequest { body } = frame else {
                    return;
                };
                let request = crate::node::account_documents::wire::DocumentRequest::decode(&body)
                    .expect("authenticated request must decode");
                if request.domain == AccountDocumentDomain::GroupControl {
                    checkpoints.lock().unwrap().push((
                        request.version,
                        request.after.clone(),
                        request.known_digest,
                        request.operation.as_ref().map(|probe| probe.known_digest),
                    ));
                    let signal = arrived.lock().unwrap().take();
                    if let Some(signal) = signal {
                        let _ = signal.send(());
                        // SessionReady was sent; hold only the unselected sibling control response.
                        let _ = release.acquire().await;
                    }
                }
            })
        }
    }));
    owner.publish_group_inventory_for(group);
    // Bootstrap may already have published this exact Inventory head. Its
    // unchanged publication correctly emits no wake; schedule the reference's
    // next ordinary refresh only after the response hook is installed.
    reference.account_sync_notify.notify_one();
    super::cold_account_setup(held)
        .await
        .expect("the reference must reach the unrelated authenticated Control response")
        .unwrap();

    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "After directory hold".into(),
            },
            |commit| commit(),
        )
        .unwrap();
    let converged = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let requester = reference
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            let source = owner
                .account_group_operation_status(&account.account_id(), id)
                .unwrap();
            let external_received = external
                .channels
                .lock()
                .unwrap()
                .state(&group)
                .is_some_and(|state| state.name() == "After directory hold");
            if let (Some(requester), Some(source)) = (requester, source)
                && requester.phase == AccountGroupOperationPhase::Committed
                && source.phase == AccountGroupOperationPhase::Committed
                && external_received
            {
                break (requester, source);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let source_while_held = owner
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let reference_while_held = reference
        .account_group_operation_status(&account.account_id(), id)
        .unwrap();
    let external_while_held = external
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EventKind::ChannelRename)
        .collect::<Vec<_>>();
    // Always release, verify directory recovery and drain owned work before the
    // held-period assertion can fail. No direct pull or result adoption is used.
    release.close();
    let control_resumed = tokio::time::timeout(Duration::from_secs(8), async {
        while checkpoints.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    *other.account_session_test_hook.lock().unwrap() = None;
    let preserved_checkpoint = {
        let checkpoints = checkpoints.lock().unwrap();
        checkpoints.len() >= 2 && checkpoints[0] == checkpoints[1]
    };
    let inventory_key = crate::node::AccountDocumentKey::GroupInventory {
        channel: hex::encode(group.as_bytes()),
        source_device: owner.user_id(),
    };
    let resumed =
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if reference.account_document(&inventory_key).unwrap().is_some_and(|document| {
                serde_json::from_slice::<serde_json::Value>(document.payload()).unwrap()["name"]
                    == "After directory hold"
            }) {
                break;
            }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
    for node in [&owner, &reference, &external, &other] {
        node.runtime_work.close();
    }
    servers.shutdown().await;
    for node in [&owner, &reference, &external, &other] {
        tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .expect("the released fixture must drain its owned work");
    }
    assert!(resumed.is_ok(), "the released Inventory page must resume");
    assert!(
        control_resumed.is_ok(),
        "unrelated sibling control must resume after the selected operation settles"
    );
    assert!(
        preserved_checkpoint,
        "preemption must retain the exact protocol scope, cursor and digests"
    );
    assert!(
        reference
            .device_sync_status()
            .iter()
            .all(|status| status.document_error.is_none()),
        "owned preemption is not a network failure"
    );
    assert!(
        source_while_held
            .as_ref()
            .is_some_and(|status| status.phase == AccountGroupOperationPhase::Committed),
        "the original owner must commit through the real request path while Inventory is held"
    );
    assert_eq!(external_while_held.len(), 1);
    let effect = &external_while_held[0];
    assert!(effect.verify_integrity() && effect.verify_signature());
    assert_eq!(
        effect.author.ed25519_pub(),
        &owner.identity.public().ed25519_pub
    );
    let (requester, source) = converged.unwrap_or_else(|_| {
        panic!("selected source receipt waited behind another sibling Control: requester={:?}, source={:?}, external_effects={}", reference_while_held.map(|status| status.phase), source_while_held.map(|status| status.phase), external_while_held.len())
    });
    assert_eq!(requester.committed_events, vec![effect.id]);
    assert_eq!(source.committed_events, requester.committed_events);
    assert!(reference.channel_members(group).is_empty());
}
