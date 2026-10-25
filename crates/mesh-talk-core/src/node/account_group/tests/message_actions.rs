//! Ordinary member actions use the same original account intent and effect WAL.
use super::*;

async fn member_action(recall: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("member"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    source
        .send_channel_message(group, b"original member message")
        .await
        .unwrap();
    let target = source
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .find(|event| event.kind == EventKind::Message)
        .unwrap()
        .id;
    let operation = if recall {
        AccountGroupOperation::Recall { target }
    } else {
        AccountGroupOperation::React {
            target,
            emoji: "👍".into(),
            remove: false,
        }
    };
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let admitted = source
        .submit_account_group_operation(&account.account_id(), id, group, operation.clone())
        .expect("a member must admit its account-owned message action");
    assert!(admitted.committed_events.is_empty());
    source.maintain_account_group_operation_effects();
    let status = source
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
    assert_eq!(status.committed_events, vec![status.primary_event.unwrap()]);
    let event = source
        .log
        .lock()
        .unwrap()
        .get(&status.primary_event.unwrap())
        .cloned()
        .unwrap();
    assert_eq!(
        event.kind,
        if recall {
            EventKind::Delete
        } else {
            EventKind::React
        }
    );
    assert!(event.verify_integrity() && event.verify_signature());
    let sealed = crate::node::channel::SealedPayload::decode(&event.ciphertext).unwrap();
    let plaintext = sealed
        .open(&source.identity, &source.identity.public().x25519_pub)
        .unwrap();
    if recall {
        assert_eq!(
            crate::node::recall::DeletePayload::decode(&plaintext)
                .unwrap()
                .target,
            target
        );
        assert!(
            source
                .recalls
                .lock()
                .unwrap()
                .targets(&group)
                .contains(&target)
        );
    } else {
        let payload = crate::node::reaction::ReactionPayload::decode(&plaintext).unwrap();
        assert_eq!(payload.target, target);
        assert_eq!(payload.emoji, "👍");
        assert!(!payload.remove);
    }
    assert_eq!(
        source
            .submit_account_group_operation(&account.account_id(), id, group, operation,)
            .unwrap(),
        status
    );
    source.maintain_account_group_operation_effects();
    assert_eq!(
        source
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind
                == if recall {
                    EventKind::Delete
                } else {
                    EventKind::React
                })
            .count(),
        1
    );
}

#[tokio::test]
async fn member_reaction_commits_one_exact_original_effect() {
    member_action(false).await;
}

#[tokio::test]
async fn member_recall_commits_one_exact_effect_and_durable_tombstone() {
    member_action(true).await;
}

fn reopen_action_node(path: &Path, account: &Account, secrets: ([u8; 32], [u8; 32])) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
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

async fn recall_original(source: &Node, group: ConversationId, deadline: Option<u64>) -> Event {
    source
        .send_channel_message_reply_expiring(group, b"recall lifecycle original", None, deadline)
        .await
        .unwrap();
    source
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .find(|event| event.kind == EventKind::Message)
        .unwrap()
        .clone()
}

fn submit_recall_request(
    source: &Node,
    account: &Account,
    original: &Event,
) -> operations::wire::RequestProjection {
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    source
        .submit_account_group_operation(
            &account.account_id(),
            id,
            original.conversation_id,
            AccountGroupOperation::Recall {
                target: original.id,
            },
        )
        .unwrap();
    source
        .operation_projection(original.conversation_id)
        .requests
        .into_iter()
        .find(|request| request.reference.id == id)
        .unwrap()
}

fn assert_no_delete_or_pending_action(source: &Node, group: ConversationId) {
    assert!(
        source
            .log
            .lock()
            .unwrap()
            .events(&group)
            .iter()
            .all(|event| event.kind != EventKind::Delete)
    );
    assert!(
        source
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
}

async fn accepted_recall_expires_before_wal(deadline_expires_first: bool) {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source");
    let account = Account::generate();
    let mut source = node(&source_path, &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let deadline = deadline_expires_first.then(|| source.expiry_now() + 60_000);
    let original = recall_original(&source, group, deadline).await;
    let request = submit_recall_request(&source, &account, &original);
    let id = request.reference.id;
    source
        .admit_account_group_request_for_test(request)
        .unwrap();
    assert_eq!(
        source
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap()
            .phase,
        AccountGroupOperationPhase::Accepted
    );
    assert_no_delete_or_pending_action(&source, group);

    let expired_at =
        deadline.unwrap_or(original.wall_clock + crate::node::recall::RECALL_WINDOW_MS) + 1;
    Arc::get_mut(&mut source).unwrap().expiry_clock_start = (std::time::Instant::now(), expired_at);
    source.maintain_account_group_operation_effects();
    let status = source
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(status.id, id);
    assert_eq!(
        status.phase,
        AccountGroupOperationPhase::Rejected {
            reason: AccountGroupOperationRejection::Expired
        },
        "a source accepted before expiry must settle durably if its first WAL admission is too late"
    );
    assert!(status.committed_events.is_empty());
    assert_no_delete_or_pending_action(&source, group);
    let secrets = source.identity.secret_bytes();
    drop(source);
    let restarted = reopen_action_node(&source_path, &account, secrets);
    assert_eq!(
        restarted
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap(),
        status,
        "the original ID's expiry rejection must survive restart"
    );
    assert_no_delete_or_pending_action(&restarted, group);
}

#[tokio::test]
async fn accepted_recall_window_expiry_before_wal_is_durably_rejected() {
    accepted_recall_expires_before_wal(false).await;
}

#[tokio::test]
async fn accepted_recall_source_deadline_before_wal_is_durably_rejected() {
    accepted_recall_expires_before_wal(true).await;
}

#[tokio::test]
async fn cancellation_and_terminal_duplicate_do_not_require_recall_target_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let original = recall_original(&source, group, None).await;
    let mut request = submit_recall_request(&source, &account, &original);
    let id = request.reference.id;
    source
        .admit_account_group_request_for_test(request.clone())
        .unwrap();
    assert_no_delete_or_pending_action(&source, group);

    // Keep real signed membership but model unavailable original public history.
    // A canonical cancellation needs only its retained original operation reference.
    let mut missing_original = crate::eventlog::persist::PersistentEventLog::open(
        &dir.path().join("missing-original.log"),
        "pw",
    )
    .unwrap();
    missing_original.append_durable(genesis).unwrap();
    *source.log.lock().unwrap() = missing_original;
    assert!(source.log.lock().unwrap().get(&original.id).is_none());
    source
        .cancel_account_group_operation(&account.account_id(), id)
        .unwrap();
    request.cancel = true;
    source
        .admit_account_group_request_for_test(request.clone())
        .expect("an admitted cancellation must not need the original message metadata");
    source.maintain_account_group_operation_effects();
    let cancelled = source
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.phase, AccountGroupOperationPhase::Cancelled);
    assert_no_delete_or_pending_action(&source, group);

    request.cancel = false;
    source.admit_account_group_request_for_test(request).expect(
        "a terminal source duplicate must return its canonical result without target lookup",
    );
    assert_eq!(
        source
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap(),
        cancelled
    );
    assert_no_delete_or_pending_action(&source, group);
}

#[tokio::test]
async fn expired_pruned_original_cannot_become_a_nonexpiring_fresh_recall() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let mut source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let deadline = source.expiry_now() + 60_000;
    let original = recall_original(&source, group, Some(deadline)).await;
    assert!(deadline < original.wall_clock + crate::node::recall::RECALL_WINDOW_MS);
    Arc::get_mut(&mut source).unwrap().expiry_clock_start =
        (std::time::Instant::now(), deadline + 1);
    assert!(source.prune_expired_received().unwrap() > 0);
    assert!(source.log.lock().unwrap().get(&original.id).is_some());
    assert!(source.sentlog.lock().unwrap().entries(&group).is_empty());
    assert!(source.received.lock().unwrap().entries(&group).is_empty());

    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let admitted = source.submit_account_group_operation(
        &account.account_id(),
        id,
        group,
        AccountGroupOperation::Recall {
            target: original.id,
        },
    );
    source.maintain_account_group_operation_effects();
    if admitted.is_ok() {
        let status = source
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        assert!(
            matches!(status.phase, AccountGroupOperationPhase::Rejected { .. }),
            "an erased source deadline must never be interpreted as a verified nonexpiring message: {:?}",
            status.phase
        );
    }
    assert_no_delete_or_pending_action(&source, group);
}

/// Stop at the real durable WAL boundary, before either runtime or startup replay.
fn stage_action_wal(
    source: &Node,
    request: &operations::wire::RequestProjection,
    sealed_input: crate::node::message_actions::MessageActionInput,
    bound_input: crate::node::message_actions::MessageActionInput,
) -> EventId {
    use crate::node::{
        delivery_store::{DeliveryTransaction, RequestRecord},
        message_actions::MessageActionAdmission,
    };
    let group = request.reference.group;
    let log = source.log.lock().unwrap();
    let target = match &sealed_input {
        crate::node::message_actions::MessageActionInput::Recall { target } => {
            Some(crate::node::message_actions::MessageActionTarget {
                original: log.get(target).unwrap().clone(),
                expires_at_ms: None,
                protected_file: false,
            })
        }
        crate::node::message_actions::MessageActionInput::React { .. } => None,
    };
    let projection =
        crate::node::channel::ChannelBook::project_membership(group, &log.events(&group));
    let prepared = source
        .prepare_channel_message_action_locked(
            group,
            &log,
            &projection,
            &sealed_input,
            MessageActionAdmission {
                own: source.own_group_proof(),
                target,
                admitted_at_ms: source.expiry_now(),
            },
        )
        .unwrap();
    log.sync().unwrap();
    let event_id = prepared.event.id;
    let transaction = DeliveryTransaction::GroupRequested {
        operation: request.reference.clone(),
        transaction: Box::new(DeliveryTransaction::Requested {
            request: RequestRecord {
                request_id: *request.reference.id.as_bytes(),
                digest: request.reference.digest,
                destination: crate::node::MessageDestination::Channel(group),
                message_id: event_id,
                accepted_at_ms: prepared.witness.admitted_at_ms,
            },
            transaction: Box::new(DeliveryTransaction::ChannelMessageAction {
                input: bound_input,
                witness: prepared.witness,
                events: vec![prepared.event],
                recall: prepared.recall,
            }),
        }),
    };
    drop(log);
    source.delivery.lock().unwrap().begin(transaction).unwrap();
    event_id
}

#[tokio::test]
async fn startup_action_recovery_persists_recall_mark_before_terminal_result() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source");
    let account = Account::generate();
    let source = node(&source_path, &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let original = recall_original(&source, group, None).await;
    let request = submit_recall_request(&source, &account, &original);
    let id = request.reference.id;
    source
        .admit_account_group_request_for_test(request.clone())
        .unwrap();
    let input = crate::node::message_actions::MessageActionInput::Recall {
        target: original.id,
    };
    let event_id = stage_action_wal(&source, &request, input.clone(), input);
    assert!(source.log.lock().unwrap().get(&event_id).is_none());
    assert!(
        !source
            .recalls
            .lock()
            .unwrap()
            .targets(&group)
            .contains(&original.id)
    );
    let secrets = source.identity.secret_bytes();
    drop(source);

    let restarted = reopen_action_node(&source_path, &account, secrets);
    assert!(restarted.log.lock().unwrap().get(&event_id).is_some());
    assert_eq!(
        restarted
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap()
            .phase,
        AccountGroupOperationPhase::Committed
    );
    assert!(
        restarted
            .recalls
            .lock()
            .unwrap()
            .targets(&group)
            .contains(&original.id),
        "startup must durably install the exact recall mark before publishing the terminal result"
    );
    assert!(
        restarted
            .delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    let secrets = restarted.identity.secret_bytes();
    drop(restarted);
    let reopened_again = reopen_action_node(&source_path, &account, secrets);
    assert!(
        reopened_again
            .recalls
            .lock()
            .unwrap()
            .targets(&group)
            .contains(&original.id)
    );
}

#[tokio::test]
async fn startup_rejects_signed_reaction_intent_with_stale_sealed_payload_before_append() {
    use crate::node::message_actions::MessageActionInput;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source");
    let account = Account::generate();
    let source = node(&source_path, &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let original = recall_original(&source, group, None).await;
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    source
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::React {
                target: original.id,
                emoji: "👍".into(),
                remove: true,
            },
        )
        .unwrap();
    let request = source
        .operation_projection(group)
        .requests
        .into_iter()
        .find(|request| request.reference.id == id)
        .unwrap();
    source
        .admit_account_group_request_for_test(request.clone())
        .unwrap();
    // Both the original request and event have real valid signatures. Only opening
    // the source's sealed entry exposes that the signed intent removed a reaction
    // while the saved event actually added it.
    let event_id = stage_action_wal(
        &source,
        &request,
        MessageActionInput::React {
            target: original.id,
            emoji: "👍".into(),
            remove: false,
        },
        MessageActionInput::React {
            target: original.id,
            emoji: "👍".into(),
            remove: true,
        },
    );
    assert!(source.log.lock().unwrap().get(&event_id).is_none());
    let secrets = source.identity.secret_bytes();
    drop(source);
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let opened = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        files,
        &source_path.join("events.log"),
        &source_path.join("sent.log"),
        "pw",
    );
    let rejected = opened.is_err();
    drop(opened);
    let log =
        crate::eventlog::persist::PersistentEventLog::open(&source_path.join("events.log"), "pw")
            .unwrap();
    assert!(
        log.get(&event_id).is_none(),
        "startup must authenticate the sealed action before appending its public event"
    );
    assert!(
        rejected,
        "a mismatched saved sealed payload must fail startup recovery"
    );
}
