//! Ordinary account members use the same durable sender path as physical senders.
use super::*;
use crate::node::{AccountGroupStickerInput, AccountGroupTextInput, MessageProtection};

async fn assert_member_message(sticker: bool, protected: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let member = node(&dir.path().join("member"), &account);
    let genesis = legacy_group(&owner, &member);
    let group = genesis.conversation_id;
    install(&member, &genesis);
    assert_eq!(
        member.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    let id = member
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let text = AccountGroupTextInput {
        text: b"same account message".to_vec(),
        reply_to: None,
        expires_at_ms: None,
        protection: protected.then_some(MessageProtection::Click),
    };
    let sticker_input = AccountGroupStickerInput {
        sticker_id: "1f602".into(),
        fallback: "😂".as_bytes().to_vec(),
        expires_at_ms: None,
    };
    let submit = || {
        if sticker {
            member.submit_account_group_sticker_if(
                &account.account_id(),
                id,
                group,
                sticker_input.clone(),
                |commit| commit(),
            )
        } else {
            member.submit_account_group_text_if(
                &account.account_id(),
                id,
                group,
                text.clone(),
                |commit| commit(),
            )
        }
    };
    let admitted = submit().expect("an ordinary account member must admit its message");
    assert!(admitted.committed_events.is_empty());
    member.maintain_account_group_operations().await;
    let status = member
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    let effects: Vec<_> = member
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .filter(|event| event.kind == EventKind::Message)
        .cloned()
        .collect();
    assert_eq!(effects.len(), 1);
    assert!(effects[0].verify_integrity() && effects[0].verify_signature());
    assert_eq!(
        effects[0].author.ed25519_pub(),
        &member.identity.public().ed25519_pub
    );
    assert!(status.committed_events.contains(&effects[0].id));
    assert_eq!(submit().unwrap(), status);
    member.maintain_account_group_operations().await;
    assert_eq!(
        member
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::Message)
            .count(),
        1
    );
    let sent = member
        .sentlog
        .lock()
        .unwrap()
        .entries(&group)
        .into_iter()
        .find(|entry| entry.seq == effects[0].seq);
    assert!(
        sent.is_some(),
        "the exact original plaintext sidecar must be durable"
    );
}

#[tokio::test]
async fn ordinary_member_text_uses_durable_account_sender() {
    assert_member_message(false, false).await;
}
#[tokio::test]
async fn ordinary_member_protected_text_retry_keeps_original_effect() {
    assert_member_message(false, true).await;
}
#[tokio::test]
async fn ordinary_member_sticker_uses_durable_account_sender() {
    assert_member_message(true, false).await;
}

/// Timeout diagnostics expose protocol stages only, never message or intent data.
pub(super) fn account_send_timeout_stage(
    source: &Node,
    reference: &Node,
    external: &Node,
    account: &Account,
    id: AccountGroupOperationId,
) -> &'static str {
    let Ok(Some(status)) = source.account_group_operation_status(&account.account_id(), id) else {
        return "action timeout: selected source has not imported the request";
    };
    if status.phase != AccountGroupOperationPhase::Committed {
        return "action timeout: selected source has not committed the effect";
    }
    let Some(event) = status.primary_event else {
        return "action timeout: selected source has no original effect receipt";
    };
    if external.log.lock().unwrap().get(&event).is_none() {
        return "action timeout: committed original effect has not reached the external member";
    }
    if reference
        .account_group_operation_inputs()
        .into_iter()
        .any(|(_, _, frame)| {
            frame.projection.results.iter().any(|result| {
                result.reference.id == id
                    && result.status.phase == AccountGroupOperationPhase::Committed
            })
        })
    {
        return "action timeout: committed source frame awaits private-copy adoption";
    }
    "action timeout: committed source frame has not reached the reference device"
}

async fn reference_member_send_fixture(fail_receive: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("offline_owner"), &Account::generate());
    let source = node(&dir.path().join("source"), &account);
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
            name: "Member send fixture".into(),
            members: vec![
                owner.identity.public(),
                source.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
    install(&source, &genesis);
    install(&external, &genesis);
    link(&source, &reference);
    link(&reference, &source);
    let mut tasks = tokio::task::JoinSet::new();
    let mut addresses = Vec::new();
    let mut listeners = Vec::new();
    for (name, target) in [
        ("source", &source),
        ("reference", &reference),
        ("external", &external),
    ] {
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(target, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push(listener);
    }
    for left in [&source, &reference, &external] {
        for (right, address) in [&source, &reference, &external].into_iter().zip(&addresses) {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Send fixture", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    for (target, listener) in [&source, &reference, &external].into_iter().zip(listeners) {
        tasks.spawn(target.clone().run_accept_loop(listener));
    }
    source.publish_account_group_access(&mut None);
    pull_group_control(&reference, &source, addresses[0]).await;
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert!(reference.channel_members(group).is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let forward = reference
                .group_operation_versions
                .lock()
                .unwrap()
                .get(&source.user_id())
                .is_some_and(|support| {
                    support.public == source.identity.public()
                        && support.kinds & AccountGroupOperationKind::Text.capability_bit() != 0
                });
            let reverse = source
                .group_operation_versions
                .lock()
                .unwrap()
                .get(&reference.user_id())
                .is_some_and(|support| support.public == reference.identity.public());
            if forward && reverse {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("current action-specific operation support must authenticate before the action budget");
    if fail_receive {
        reference
            .received
            .lock()
            .unwrap()
            .fail_appends_for_test(true)
            .unwrap();
    }
    for sticker in [false, true] {
        let id = reference
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        let submit = || {
            if sticker {
                reference.submit_account_group_sticker_if(
                    &account.account_id(),
                    id,
                    group,
                    AccountGroupStickerInput {
                        sticker_id: "1f602".into(),
                        fallback: "😂".as_bytes().to_vec(),
                        expires_at_ms: None,
                    },
                    |commit| commit(),
                )
            } else {
                reference.submit_account_group_text_if(
                    &account.account_id(),
                    id,
                    group,
                    AccountGroupTextInput {
                        text: b"delegated member text".to_vec(),
                        reply_to: None,
                        expires_at_ms: None,
                        protection: None,
                    },
                    |commit| commit(),
                )
            }
        };
        let admitted =
            submit().expect("a reference-only member must admit its own account message");
        assert!(admitted.committed_events.is_empty());
        assert_eq!(admitted.executions[0].device_id, source.user_id());
        if fail_receive && !sticker {
            let (key, hash) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
                loop {
                    if let Some((key, hash, _)) = reference
                        .account_group_operation_inputs()
                        .into_iter()
                        .find(|(_, _, frame)| {
                            frame.projection.results.iter().any(|result| {
                                result.reference.id == id
                                    && result.status.phase == AccountGroupOperationPhase::Committed
                            })
                        })
                    {
                        break (key, hash);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{}",
                    account_send_timeout_stage(&source, &reference, &external, &account, id)
                )
            });
            reference.maintain_account_group_operation_effects();
            assert_ne!(
                reference
                    .account_group_operation_status(&account.account_id(), id)
                    .unwrap()
                    .unwrap()
                    .phase,
                AccountGroupOperationPhase::Committed
            );
            assert!(
                reference
                    .operation_projection(group)
                    .requests
                    .iter()
                    .any(|request| request.reference.id == id && request.body.is_some())
            );
            assert_ne!(
                reference.operation_seen.lock().unwrap().get(&key),
                Some(&hash),
                "failed private persistence must not consume the source frame or original body"
            );
            reference
                .received
                .lock()
                .unwrap()
                .fail_appends_for_test(false)
                .unwrap();
        }
        let status = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let status = reference
                    .account_group_operation_status(&account.account_id(), id)
                    .unwrap()
                    .unwrap();
                if status.phase == AccountGroupOperationPhase::Committed
                    && status.primary_event.is_some_and(|event| {
                        external
                            .history(group, 20)
                            .iter()
                            .any(|row| row.id == event)
                    })
                {
                    break status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{}",
                account_send_timeout_stage(&source, &reference, &external, &account, id)
            )
        });
        let event_id = status
            .primary_event
            .expect("a send has one original message ID");
        let event = external
            .log
            .lock()
            .unwrap()
            .get(&event_id)
            .cloned()
            .unwrap();
        assert_eq!(event.kind, EventKind::Message);
        assert_eq!(
            event.author.ed25519_pub(),
            &source.identity.public().ed25519_pub
        );
        assert!(event.verify_integrity() && event.verify_signature());
        assert_eq!(submit().unwrap(), status);
        let own=reference.history(group,20).into_iter().find(|row|row.id==event_id)
            .expect("a committed explicit own send must survive in private history even when optional history is disabled");
        assert!(own.from_me);
        assert_eq!(own.who, "you");
        assert_eq!(
            reference
                .received
                .lock()
                .unwrap()
                .entry(event_id)
                .unwrap()
                .from,
            source.user_id()
        );
        assert_eq!(own.sticker.is_some(), sticker);
        assert!(
            reference.channel_members(group).is_empty(),
            "private own copies cannot install public membership"
        );
    }
    assert_eq!(
        external
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::Message)
            .count(),
        2
    );
    tasks.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reference_member_sends_over_noise_without_public_membership_or_optional_history() {
    reference_member_send_fixture(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_private_send_copy_retries_the_same_source_frame_before_terminal_adoption() {
    reference_member_send_fixture(true).await;
}

#[tokio::test]
async fn old_operation_source_never_receives_an_unsupported_send_body() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = legacy_group(&source, &external);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    link(&source, &reference);
    link(&reference, &source);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    configure(&source, &dir.path().join("source"), address.port());
    configure(&reference, &dir.path().join("reference"), 9);
    source.publish_account_group_access(&mut None);
    let server = inbound_server(source.clone(), listener);
    pull_group_control(&reference, &source, address).await;
    reference.group_operation_versions.lock().unwrap().insert(
        source.user_id(),
        crate::node::account_documents::sync::GroupOperationSupport {
            public: source.identity.public(),
            observed_at: std::time::Instant::now(),
            kinds: AccountGroupOperationKind::legacy_capabilities(),
        },
    );
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let status = reference
        .submit_account_group_text_if(
            &account.account_id(),
            id,
            group,
            AccountGroupTextInput {
                text: b"wait for a capable executor".to_vec(),
                reply_to: None,
                expires_at_ms: None,
                protection: None,
            },
            |commit| commit(),
        )
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::WaitingForExecutor);
    assert!(
        reference.operation_projection(group).requests.is_empty(),
        "an old v1 source must not receive a body which it can only reject as unsupported"
    );
    reference
        .group_operation_versions
        .lock()
        .unwrap()
        .get_mut(&source.user_id())
        .unwrap()
        .kinds |= AccountGroupOperationKind::Text.capability_bit();
    let requests = reference.operation_projection(group).requests;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].reference.id, id);
    assert!(requests[0].body.is_some());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn source_rejects_a_valid_signed_send_beyond_the_ordinary_expiry_horizon() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let executor = source
        .account_group_access(group)
        .unwrap()
        .sources
        .into_iter()
        .find(|candidate| candidate.device == source.identity.public())
        .unwrap();
    assert_eq!(executor.role, AccountGroupRole::Member);
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let body = crate::node::MessageBody::new(b"remote future deadline".to_vec(), None).encode();
    let deadline = crate::node::node::now_millis() + 60 * 24 * 60 * 60 * 1000;
    let operation = AccountGroupOperation::Send {
        message: PreparedAccountGroupMessage::from_plaintext(
            AccountGroupOperationKind::Text,
            crate::expiry::wrap(&body, Some(deadline)),
        )
        .unwrap(),
    };
    let reference = crate::node::account_group::operations::journal::OperationReference::new(
        &source, id, group, &operation, executor,
    );
    assert!(reference.valid(&account.public().ed25519_pub));
    source.seed_account_group_request_for_test(reference, operation);
    source.maintain_account_group_operation_effects();
    assert_eq!(
        source
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap()
            .phase,
        AccountGroupOperationPhase::Rejected {
            reason: AccountGroupOperationRejection::InvalidPayload
        }
    );
    assert_eq!(
        source
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::Message)
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn an_existing_live_send_copy_must_be_synced_before_terminal_adoption() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let requester = node(&dir.path().join("requester"), &account);
    let received_dir = dir.path().join("received-only");
    std::fs::create_dir(&received_dir).unwrap();
    let received_path = received_dir.join("messages.log");
    *requester.received.lock().unwrap() =
        crate::node::received_log::ReceivedLog::open(&received_path, "pw").unwrap();
    let group = new_channel_id();
    let event_id = EventId::new([0x69; 32]);
    let plaintext = crate::node::MessageBody::new(b"already received live".to_vec(), None).encode();
    requester
        .received
        .lock()
        .unwrap()
        .record(group, requester.user_id(), 1, &plaintext, event_id)
        .unwrap();
    let row = crate::node::received_log::ReceivedEntry {
        event_id,
        conversation: group,
        from: requester.user_id(),
        wall_clock: 2,
        plaintext,
    };
    let moved = dir.path().join("temporarily-moved-received");
    std::fs::rename(&received_dir, &moved).unwrap();
    let result = requester.import_own_account_channel_copy(&row);
    // Always restore the owned directory before asserting or closing handles.
    std::fs::rename(&moved, &received_dir).unwrap();
    assert!(
        result.is_err(),
        "an existing unsynced live row cannot acknowledge a durable private send copy"
    );
    requester.import_own_account_channel_copy(&row).unwrap();
    assert_eq!(
        requester
            .received
            .lock()
            .unwrap()
            .entry(event_id)
            .unwrap()
            .wall_clock,
        1,
        "sync must preserve the original local receive timestamp"
    );
}

#[tokio::test]
async fn prepared_member_send_recovers_exact_sidecars_and_terminal_on_node_restart() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let path = dir.path().join("source");
    let source = node(&path, &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let input = AccountGroupTextInput {
        text: b"original restart body".to_vec(),
        reply_to: None,
        expires_at_ms: None,
        protection: Some(MessageProtection::Click),
    };
    source
        .submit_account_group_text_if(&account.account_id(), id, group, input.clone(), |commit| {
            commit()
        })
        .unwrap();
    assert!(
        source
            .interrupt_account_group_completion_for_test(group, id)
            .is_err()
    );
    assert_eq!(
        source.delivery.lock().unwrap().pending_transactions().len(),
        1
    );
    let effect = source
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .find(|event| event.kind == EventKind::Message)
        .cloned()
        .unwrap();
    assert_eq!(source.sentlog.lock().unwrap().entries(&group).len(), 1);
    let secrets = source.identity.secret_bytes();
    drop(source);
    let (dm, _) = mpsc::unbounded_channel();
    let (channels, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let restarted = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channels,
        files,
        &path.join("events.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap();
    let status = restarted
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(status.primary_event, Some(effect.id));
    assert!(
        restarted
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    assert_eq!(restarted.sentlog.lock().unwrap().entries(&group).len(), 1);
    assert_eq!(
        restarted
            .submit_account_group_text_if(&account.account_id(), id, group, input, |commit| commit(
            ))
            .unwrap(),
        status
    );
    restarted.maintain_account_group_operation_effects();
    assert_eq!(
        restarted
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::Message)
            .count(),
        1
    );
}

#[tokio::test]
async fn private_send_completion_rejects_forged_or_unrelated_public_event_proofs() {
    use crate::node::account_group::operations::wire::OperationFrame;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    source
        .submit_account_group_text_if(
            &account.account_id(),
            id,
            group,
            AccountGroupTextInput {
                text: b"bound to original event".to_vec(),
                reply_to: None,
                expires_at_ms: None,
                protection: None,
            },
            |commit| commit(),
        )
        .unwrap();
    source.maintain_account_group_operation_effects();
    let original = source.operation_projection(group);
    assert!(original.results[0].message_event.is_some());
    assert!(
        OperationFrame::decode(
            &OperationFrame::new(&source, group, 1, original.clone()).encode(),
            &account.public().ed25519_pub
        )
        .is_some()
    );
    for case in 0..3 {
        let mut changed = original.clone();
        let event = changed.results[0].message_event.as_mut().unwrap();
        match case {
            0 => *event = genesis.clone(),
            1 => event.wall_clock += 1,
            _ => {
                *event = Event::new(
                    &owner.identity,
                    group,
                    3,
                    vec![genesis.id],
                    3,
                    3,
                    EventKind::Message,
                    b"another valid signer".to_vec(),
                );
                let foreign_id = event.id;
                changed.results[0].status.primary_event = Some(foreign_id);
                changed.results[0].status.committed_events = vec![foreign_id];
            }
        }
        let frame = OperationFrame::new(&source, group, 2, changed);
        assert!(
            OperationFrame::decode(&frame.encode(), &account.public().ed25519_pub).is_none(),
            "a newly signed private head cannot forge its public source event proof"
        );
    }
}
