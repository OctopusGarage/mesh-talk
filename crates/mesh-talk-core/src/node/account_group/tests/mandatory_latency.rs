//! Actual Noise regression: an acknowledged optional page cannot delay controls.
use super::*;
use crate::node::account_sync_wire::AccountHistoryFrame;
use crate::node::{AccountDocumentKey, DocumentConversation};
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

#[tokio::test]
async fn reference_owner_rename_overtakes_blocked_optional_document_page_over_noise() {
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
        install(target, &genesis);
        // A real pending invitation prevents migration from hiding legacy-owner
        // execution behind a modern account membership grant.
        install(target, &invitation);
    }
    for left in [&owner, &member, &reference] {
        for right in [&owner, &member, &reference] {
            if left.user_id() != right.user_id() {
                link(left, right);
            }
        }
        // The member must not obtain Draft 65 and supply a bypass around the
        // exact owner -> requester page whose acknowledged response is blocked.
        left.set_device_sync_category(SyncCategory::History, left.user_id() != member.user_id())
            .unwrap();
    }
    let drafts: Vec<_> = (1..=65)
        .map(|index| {
            let key = AccountDocumentKey::Draft {
                conversation: DocumentConversation::Account(format!("{index:032x}")),
            };
            let payload = format!("bounded optional draft {index}").into_bytes();
            let meta = owner
                .put_account_document_if(key.clone(), payload.clone(), None, |commit| commit())
                .unwrap();
            (key, payload, meta)
        })
        .collect();
    let blocked_after = drafts[63].0.clone();
    let exact_requester = reference.identity.public();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(Semaphore::new(0));
    let (arrived, acknowledged_page) = oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    *owner.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let observed = observed.clone();
        let release = release.clone();
        move |peer, frame| {
            let observed = observed.clone();
            let release = release.clone();
            let arrived = arrived.clone();
            let blocked_after = blocked_after.clone();
            let exact_requester = exact_requester.clone();
            Box::pin(async move {
                if peer != exact_requester {
                    return;
                }
                let AccountHistoryFrame::DocumentsRequest { body } = frame else {
                    return;
                };
                let request = crate::node::account_documents::wire::DocumentRequest::decode(&body)
                    .expect("the session admitted a valid document request");
                if request.domain != AccountDocumentDomain::Draft {
                    return;
                }
                observed.lock().unwrap().push(request.after.clone());
                if request.after.as_ref() == Some(&blocked_after) {
                    let signal = arrived.lock().unwrap().take();
                    if let Some(signal) = signal {
                        let _ = signal.send(());
                    }
                    // The session has actually sent SessionReady. Only this
                    // optional response waits; Wake and mandatory requests keep
                    // their real transport and response owners.
                    let _released = release.acquire().await.unwrap();
                }
            })
        }
    }));
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
        listeners.push((target.clone(), listener));
    }
    for left in [&owner, &member, &reference, &external] {
        for (right, address) in [&owner, &member, &reference, &external]
            .into_iter()
            .zip(&addresses)
        {
            // The real legacy member remains certified and serves production
            // sessions, but is offline in this route graph. Otherwise its free
            // mandatory worker can relay the owner's result around the held
            // owner -> requester exchange even with optional History disabled.
            if left.user_id() != right.user_id()
                && left.user_id() != member.user_id()
                && right.user_id() != member.user_id()
            {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Rename peer", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    // Every scheduler's initial tick sees its complete route fixture. Each
    // production accept loop already owns exactly one history scheduler.
    for (target, listener) in listeners {
        servers.spawn(target.run_accept_loop(listener));
    }
    owner.publish_account_group_access(&mut None);
    tokio::time::timeout(Duration::from_secs(30), acknowledged_page)
        .await
        .unwrap_or_else(|_| {
            let imported = drafts.iter().filter(|(key, _, _)| reference.account_document_meta(key).is_some()).count();
            panic!(
                "the real requester must reach the acknowledged second Draft page: requests={:?}, imported_drafts={imported}, requester_sync={:?}, source_sync={:?}",
                observed.lock().unwrap(), reference.device_sync_status(), owner.device_sync_status(),
            )
        })
        .unwrap();
    for (key, payload, meta) in &drafts[..64] {
        let imported = reference.account_document(key).unwrap().unwrap();
        assert_eq!(imported.payload(), payload);
        assert_eq!(&imported.meta, meta);
    }
    assert!(reference.account_document_meta(&drafts[64].0).is_none());
    assert!(member.account_document_meta(&drafts[64].0).is_none());
    for target in [&owner, &reference, &external] {
        assert!(target.routing_peer(&member.user_id()).is_none());
        assert!(member.routing_peer(&target.user_id()).is_none());
    }
    assert_eq!(
        *observed.lock().unwrap(),
        vec![None, Some(drafts[63].0.clone())],
        "the optional sweep must have reached its second page without restarting"
    );
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert!(reference.channel_members(group).is_empty());
    assert!(!reference.channel_can_manage(group));
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    reference
        .submit_account_group_operation_if(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "  After account rename  ".into(),
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
            if let (Some(requester), Some(source)) = (requester, source)
                && requester.phase == AccountGroupOperationPhase::Committed
                && source.phase == AccountGroupOperationPhase::Committed
                && external
                    .channels
                    .lock()
                    .unwrap()
                    .state(&group)
                    .is_some_and(|state| state.name() == "After account rename")
            {
                break (requester, source);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // Release only after the unchanged acceptance deadline. Even a failing RED
    // tears down its real serving tasks without leaving a response gate behind.
    let requests_while_blocked = observed.lock().unwrap().clone();
    let still_missing = reference.account_document_meta(&drafts[64].0).is_none();
    release.add_permits(1);
    assert!(
        still_missing,
        "the optional page must remain blocked until both commits"
    );
    assert!(
        member
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .is_none(),
        "the offline legacy member must not relay private operation requests or results"
    );
    for target in [&owner, &reference, &external] {
        assert!(target.routing_peer(&member.user_id()).is_none());
        assert!(member.routing_peer(&target.user_id()).is_none());
    }
    let (requester, source) = converged.unwrap_or_else(|_| {
        panic!(
            "mandatory owner rename exceeded 8s while an acknowledged optional Draft page was held: requester={:?}, source={:?}, requests={requests_while_blocked:?}",
            reference.account_group_operation_status(&account.account_id(), id).unwrap().map(|status| status.phase),
            owner.account_group_operation_status(&account.account_id(), id).unwrap().map(|status| status.phase),
        )
    });
    assert_eq!(
        requests_while_blocked,
        vec![None, Some(drafts[63].0.clone())],
        "mandatory progress must retain the optional cursor rather than restart it"
    );
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
    assert_eq!(requester.committed_events, vec![events[0].id]);
    assert_eq!(source.committed_events, requester.committed_events);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(imported) = reference.account_document(&drafts[64].0).unwrap() {
                assert_eq!(imported.payload(), drafts[64].1);
                assert_eq!(imported.meta, drafts[64].2);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the held optional second page must resume and import Draft 65");
    assert!(reference.channel_members(group).is_empty());
    *owner.account_session_test_hook.lock().unwrap() = None;
    servers.abort_all();
    while servers.join_next().await.is_some() {}
}
