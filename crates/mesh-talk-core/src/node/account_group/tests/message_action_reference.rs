//! Explicit account actions work through Noise while optional public history is off.
use super::*;
use crate::node::AccountGroupTextInput;

#[path = "reaction_account_reduction.rs"]
mod reaction_account_reduction;

async fn wait_action_fixture(ready: impl FnMut() -> bool, message: &'static str) {
    wait_action_fixture_diagnosed(ready, || message).await;
}

async fn wait_action_fixture_diagnosed(
    mut ready: impl FnMut() -> bool,
    diagnosis: impl Fn() -> &'static str,
) {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while !ready() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{}", diagnosis()));
}

/// Role-page publication can invalidate the authenticated page mid-transfer.
/// Retain the cursor and retry only that precise transport interruption; other
/// errors remain fixture failures, and operation execution keeps its own budget.
async fn pull_reference_action_control(
    target: &Arc<Node>,
    source: &Node,
    address: std::net::SocketAddr,
) {
    target.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &source.identity,
            &source.account,
            "Reference action control",
            address.port(),
        ),
        address.ip(),
        &target.user_id(),
    );
    let peer = target.routing_peer(&source.user_id()).unwrap();
    assert_eq!(peer.public, source.identity.public());
    assert!(target.certified_sibling(&peer.public));
    let mut cursor = crate::node::account_documents::sync::DocumentSyncCursor::default();
    let mut interruptions = 0;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match target.pull_account_documents_page(&peer, &mut cursor).await {
                Ok(()) => {
                    assert!(
                        cursor.error.is_none() && !cursor.requires_upgrade,
                        "control bootstrap must not hide a protocol error or capability rejection"
                    );
                    if target
                        .account_documents(AccountDocumentDomain::GroupControl, None, 64)
                        .unwrap()
                        .items
                        .iter()
                        .any(|meta| meta.writer_device == source.user_id())
                    {
                        break;
                    }
                    // A GroupControl interruption may leave a private 100ms
                    // domain cooldown while another mandatory domain succeeds.
                    // That success does not prove this source's role row arrived.
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => {
                    let page_interrupted = matches!(&error,
                        crate::node::NodeError::Session(
                            crate::node::session::SessionError::Transport(
                                crate::transport::TransportError::Io(io)
                            )
                        ) if io.kind() == std::io::ErrorKind::UnexpectedEof
                    ) && cursor.pending
                        && !cursor.up_to_date
                        && cursor.error.is_none()
                        && !cursor.requires_upgrade
                        && target.certified_sibling(&peer.public)
                        && peer.public == source.identity.public();
                    assert!(
                        page_interrupted,
                        "only an authenticated pending GroupControl EOF is retryable: {error}"
                    );
                    interruptions += 1;
                    assert!(
                        interruptions <= 3,
                        "role page repeatedly invalidated during bootstrap"
                    );
                    // Honor both public retry_at and the owner's private short
                    // GroupControl cooldown without discarding cursor progress.
                    let delay = cursor
                        .retry_at
                        .map(|at| at.saturating_duration_since(std::time::Instant::now()))
                        .unwrap_or_default()
                        .max(std::time::Duration::from_millis(100));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    })
    .await
    .expect(
        "the exact authenticated source GroupControl row must arrive within the bootstrap budget",
    );
}

async fn reference_message_action(recall: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("offline-owner"), &Account::generate());
    let source = node(&dir.path().join("source"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let sibling = node(&dir.path().join("sibling"), &account);
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
            name: "Reference message actions".into(),
            members: vec![
                owner.identity.public(),
                source.identity.public(),
                sibling.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
    for target in [&source, &sibling, &external] {
        install(target, &genesis);
    }
    link(&source, &reference);
    link(&reference, &source);
    let nodes = [&source, &reference, &external, &sibling];
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for (name, target) in [
        ("source", &source),
        ("reference", &reference),
        ("external", &external),
        ("sibling", &sibling),
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
    // Authenticate A/B/C first. D has real physical membership but is not yet
    // introduced as a same-account executor, so the original send selects A.
    for left in nodes.iter().take(3) {
        for (right, address) in nodes.iter().take(3).zip(&addresses) {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Reference action fixture", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    let mut tasks = tokio::task::JoinSet::new();
    for (target, listener) in nodes.into_iter().zip(listeners) {
        tasks.spawn(target.clone().run_accept_loop(listener));
    }
    source.publish_account_group_access(&mut None);
    pull_reference_action_control(&reference, &source, addresses[0]).await;
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert!(reference.channel_members(group).is_empty());
    wait_action_fixture(
        || {
            reference
                .group_operation_versions
                .lock()
                .unwrap()
                .get(&source.user_id())
                .is_some_and(|support| {
                    support.public == source.identity.public()
                        && support.kinds & AccountGroupOperationKind::Text.capability_bit() != 0
                })
                && source
                    .group_operation_versions
                    .lock()
                    .unwrap()
                    .get(&reference.user_id())
                    .is_some_and(|support| support.public == reference.identity.public())
        },
        "real Noise capability negotiation must precede explicit send admission",
    )
    .await;

    let send_id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let sent = reference
        .submit_account_group_text_if(
            &account.account_id(),
            send_id,
            group,
            AccountGroupTextInput {
                text: b"original account action target".to_vec(),
                reply_to: None,
                expires_at_ms: None,
                protection: None,
            },
            |commit| commit(),
        )
        .unwrap();
    assert_eq!(sent.executions[0].device_id, source.user_id());
    wait_action_fixture(
        || {
            reference
                .account_group_operation_status(&account.account_id(), send_id)
                .unwrap()
                .is_some_and(|status| {
                    status.phase == AccountGroupOperationPhase::Committed
                        && status.primary_event.is_some_and(|id| {
                            external.history(group, 20).iter().any(|row| row.id == id)
                        })
                })
        },
        "the original A-authored send and private receipt must arrive over Noise",
    )
    .await;
    let target = reference
        .account_group_operation_status(&account.account_id(), send_id)
        .unwrap()
        .unwrap()
        .primary_event
        .unwrap();
    assert!(
        reference
            .history(group, 20)
            .iter()
            .any(|row| row.id == target && row.from_me)
    );
    assert!(
        reference.log.lock().unwrap().get(&target).is_none(),
        "History OFF must leave B with the original private send receipt, not public message history"
    );
    let original = external.log.lock().unwrap().get(&target).cloned().unwrap();
    assert_eq!(
        original.author.ed25519_pub(),
        &source.identity.public().ed25519_pub
    );
    assert!(original.verify_integrity() && original.verify_signature());

    // Introduce an independently capable same-account physical executor only
    // after A's original provenance is fixed. These are real device certificates
    // and authenticated operation support, with no synthetic membership grants.
    for left in [&source, &reference, &sibling] {
        for right in [&source, &reference, &sibling] {
            if left.user_id() != right.user_id() {
                link(left, right);
            }
        }
    }
    for left in nodes {
        for (right, address) in nodes.into_iter().zip(&addresses) {
            if left.user_id() != right.user_id() {
                left.roster.lock().unwrap().update(
                    &right.signed_announce("Capable sibling", address.port()),
                    address.ip(),
                    &left.user_id(),
                );
            }
        }
    }
    sibling.publish_account_group_access(&mut None);
    pull_reference_action_control(&reference, &sibling, addresses[3]).await;
    let kind = if recall {
        AccountGroupOperationKind::Recall
    } else {
        AccountGroupOperationKind::Reaction
    };
    wait_action_fixture(
        || {
            [source.as_ref(), sibling.as_ref()]
                .into_iter()
                .all(|candidate| {
                    reference
                        .group_operation_versions
                        .lock()
                        .unwrap()
                        .get(&candidate.user_id())
                        .is_some_and(|support| {
                            support.public == candidate.identity.public()
                                && support.kinds & kind.capability_bit() != 0
                        })
                })
                && reference
                    .account_group_access(group)
                    .unwrap()
                    .sources
                    .iter()
                    .any(|candidate| candidate.device == sibling.identity.public())
        },
        "both physical account executors must authenticate action support",
    )
    .await;

    let turns = if recall { 1 } else { 2 };
    let mut effect_ids = Vec::new();
    for turn in 0..turns {
        let operation = if recall {
            AccountGroupOperation::Recall { target }
        } else {
            AccountGroupOperation::React {
                target,
                emoji: "👍".into(),
                remove: turn == 1,
            }
        };
        let id = reference
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        // Establish A's add first, then make A unavailable for later React
        // actions. D's remove must address the same logical account
        // reaction while preserving each original physical event author.
        // Recall still pins A even when D is more ready.
        let held_device = if recall || turn > 0 {
            source.user_id()
        } else {
            sibling.user_id()
        };
        let original_kinds = {
            let mut support = reference.group_operation_versions.lock().unwrap();
            let selected = support.get_mut(&held_device).unwrap();
            let original = selected.kinds;
            selected.kinds &= !kind.capability_bit();
            original
        };
        let admitted = reference
            .submit_account_group_operation(&account.account_id(), id, group, operation.clone())
            .unwrap();
        reference
            .group_operation_versions
            .lock()
            .unwrap()
            .get_mut(&held_device)
            .unwrap()
            .kinds = original_kinds;
        assert_eq!(
            admitted.executions[0].device_id,
            if recall || turn == 0 {
                source.user_id()
            } else {
                sibling.user_id()
            },
            "React must switch to capable D; Recall must retain original author A"
        );
        wait_action_fixture_diagnosed(
            || {
                reference
                    .account_group_operation_status(&account.account_id(), id)
                    .unwrap()
                    .is_some_and(|status| {
                        status.phase == AccountGroupOperationPhase::Committed
                            && status.primary_event.is_some_and(|event| {
                                external.log.lock().unwrap().get(&event).is_some()
                            })
                    })
            },
            || {
                super::messages::panic_account_send_timeout(
                    if recall || turn == 0 {
                        &source
                    } else {
                        &sibling
                    },
                    &reference,
                    &external,
                    &account,
                    id,
                )
            },
        )
        .await;
        let status = reference
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        let effect_id = status.primary_event.unwrap();
        effect_ids.push(effect_id);
        assert_eq!(status.id, id);
        assert_eq!(status.committed_events, vec![effect_id]);
        let effect = external
            .log
            .lock()
            .unwrap()
            .get(&effect_id)
            .cloned()
            .unwrap();
        assert!(effect.verify_integrity() && effect.verify_signature());
        assert_eq!(
            effect.kind,
            if recall {
                EventKind::Delete
            } else {
                EventKind::React
            }
        );
        let sealed = crate::node::channel::SealedPayload::decode(&effect.ciphertext).unwrap();
        let executor = if effect.author.ed25519_pub() == &source.identity.public().ed25519_pub {
            &source
        } else {
            assert_eq!(
                effect.author.ed25519_pub(),
                &sibling.identity.public().ed25519_pub
            );
            &sibling
        };
        let plaintext = sealed
            .open(&external.identity, &executor.identity.public().x25519_pub)
            .unwrap();
        if recall {
            assert_eq!(effect.author, original.author);
            assert_eq!(
                crate::node::recall::DeletePayload::decode(&plaintext)
                    .unwrap()
                    .target,
                target
            );
            assert!(
                external
                    .history(group, 20)
                    .iter()
                    .any(|row| row.id == target && row.recalled)
            );
            assert!(
                reference
                    .recalls
                    .lock()
                    .unwrap()
                    .targets(&group)
                    .contains(&target),
                "the committed explicit Recall must durably project privately on B with optional History OFF"
            );
            assert!(
                reference
                    .history(group, 20)
                    .iter()
                    .any(|row| row.id == target && row.recalled)
            );
        } else {
            let payload = crate::node::reaction::ReactionPayload::decode(&plaintext).unwrap();
            assert_eq!(payload.target, target);
            assert_eq!(payload.emoji, "👍");
            assert_eq!(payload.remove, turn == 1);
            let external_views = external.reactions(group);
            let private_views = reference.reactions(group);
            if turn == 0 {
                assert!(
                    external_views
                        .iter()
                        .any(|row| row.target == target.to_hex()
                            && row.emoji == "👍"
                            && row.who.len() == 1),
                    "the original A add must count as one logical reaction"
                );
                assert!(
                    private_views.iter().any(|row| row.target == target.to_hex()
                        && row.emoji == "👍"
                        && row.who.len() == 1),
                    "the requester must reduce certified source authors by their authenticated account"
                );
            } else {
                assert!(
                    external_views
                        .iter()
                        .all(|row| row.target != target.to_hex() || row.emoji != "👍")
                );
                assert!(
                    private_views
                        .iter()
                        .all(|row| row.target != target.to_hex() || row.emoji != "👍")
                );
            }
            let rows = reference
                .my_dm_reactions
                .lock()
                .unwrap()
                .channel_entries(group);
            assert!(
                rows.iter().any(|(author, saved_id, _, saved)| *author
                    == executor.identity.public()
                    && *saved_id == effect_id
                    && *saved == payload),
                "terminal adoption must retain the exact original effect in the durable channel projection"
            );
            if turn > 0 {
                assert_eq!(
                    rows.len(),
                    2,
                    "account reduction must retain A and D physical provenance"
                );
                assert!(rows.iter().any(|(author, saved_id, _, saved)| *author
                    == source.identity.public()
                    && *saved_id == effect_ids[0]
                    && !saved.remove));
                assert!(rows.iter().any(|(author, saved_id, _, saved)| *author
                    == sibling.identity.public()
                    && *saved_id == effect_id
                    && saved.remove == (turn == 1)));
            }
        }
        assert_eq!(
            reference
                .submit_account_group_operation(&account.account_id(), id, group, operation)
                .unwrap(),
            status
        );
        assert!(reference.channel_members(group).is_empty());
        assert!(
            reference.log.lock().unwrap().get(&effect_id).is_none(),
            "private action results must not install public history or physical membership on B"
        );
    }
    for holder in [&source, &sibling, &external] {
        let log = holder.log.lock().unwrap();
        for effect_id in &effect_ids {
            let count = log
                .events(&group)
                .into_iter()
                .filter(|event| event.id == *effect_id)
                .count();
            if holder.user_id() == external.user_id()
                || holder.user_id() == source.user_id() && recall
            {
                assert_eq!(
                    count, 1,
                    "same original operation retries must not duplicate public effects"
                );
            } else {
                assert!(count <= 1);
            }
        }
    }
    tasks.shutdown().await;
}

// Admission temporarily overrides cached support without awaiting. A single
// runtime thread keeps live capability responses from replacing that override
// between cache mutation and synchronous executor selection.
#[tokio::test]
async fn reference_member_reacts_over_noise_with_private_projection_and_history_off() {
    reference_message_action(false).await;
}

#[tokio::test]
async fn reference_member_recall_pins_original_author_over_ready_sibling_with_history_off() {
    reference_message_action(true).await;
}
