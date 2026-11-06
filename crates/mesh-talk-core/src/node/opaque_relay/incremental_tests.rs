use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};
use crate::postoffice::{IndexedPostOffice, federation::IndexedRelayEndpoint};
use crate::storage::profile::ProfileStorageLease;

async fn exchange(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
    conversation: ConversationId,
) {
    // This fixture explicitly selects the federated mailbox and therefore
    // exercises the same bridge permission required by production routing.
    node.set_bridge_discovery_enabled(true);
    let keys = relay.secret_bytes();
    let endpoint = IndexedRelayEndpoint::new(office, None, None);
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let channel = crate::transport::SecureChannel::accept(server, &identity)
            .await
            .unwrap();
        endpoint
            .serve_connection_checked(channel, None)
            .await
            .unwrap();
    });
    let mut channel =
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
    let mailbox = RelayMailbox::federated(&node.identity, peer).unwrap();
    let confirm = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .authored_header(
            &conversation,
            crate::eventlog::Author::from_ed25519(node.identity.public().ed25519_pub),
            1,
        )
        .unwrap()
        .map(|header| header.id);
    let uploaded = node
        .opaque_mailbox_round_confirmed(
            &mut channel,
            peer,
            Some(conversation),
            mailbox,
            confirm,
            MailboxIntent::Duplex,
        )
        .await
        .unwrap();
    if let Some(first) = confirm {
        assert!(
            uploaded.iter().any(|(inner, _)| *inner == first),
            "exact custody target must survive bounded recent results, including old targets beyond 64 events"
        );
    }
    drop(channel);
    task.await.unwrap();
}

#[tokio::test]
async fn incremental_mailbox_revalidates_only_the_selected_consent_offer() {
    use crate::{
        discovery::Announce, eventlog::sync::network::NetworkSyncStore, file::FileManifest,
    };
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let sender = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&sender, &Account::generate(), "Sender", 47475);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.set_bridge_discovery_enabled(true);
    node.set_attachment_global_auto(true).unwrap();
    node.cached_peer_snapshot_async().await.unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&sender.public(), &node.identity.public());
    for index in 1..=8u8 {
        let manifest = FileManifest {
            name: "empty.txt".into(),
            size: 0,
            mime: "text/plain".into(),
            checksum: crate::file::file_checksum(&[]),
            file_key: [3; 32],
            file_conv: ConversationId::new([index; 32]),
            chunk_count: 1,
        };
        let original = Event::new(
            &sender,
            conversation,
            u64::from(index),
            vec![],
            u64::from(index),
            1,
            EventKind::FileManifest,
            crate::dm::seal(
                &sender,
                &node.identity.public().x25519_pub,
                &manifest.encode(),
            )
            .unwrap(),
        );
        node.accept_manifest_event(
            &original,
            &proof,
            super::super::received_log::ReceivedEntry {
                event_id: original.id,
                conversation,
                from: sender.user_id(),
                wall_clock: 1,
                plaintext: manifest.encode(),
            },
        )
        .unwrap();
    }
    let expected = node.file_consent_events(&sender.public()).unwrap();
    assert_eq!(
        expected.len(),
        8,
        "all eight actual controls must be eligible"
    );
    let relay = DeviceIdentity::generate();
    let office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    super::super::file_consent::offered_observations::reset();
    counted_scope_exchange(
        &node,
        office.clone(),
        &relay,
        &sender.public(),
        conversation,
        true,
        None,
    )
    .await;
    let checks = super::super::file_consent::offered_observations::count();
    let mailbox = RelayMailbox::federated(&node.identity, &sender.public()).unwrap();
    for control in &expected {
        let wrapper = mailbox.wrap(control).unwrap();
        assert!(
            NetworkSyncStore::durable_have(&office, &mailbox.id(), &wrapper.id).unwrap(),
            "every current signed consent control must reach actual relay custody"
        );
    }
    assert!(
        (8..=16).contains(&checks),
        "one discovery of eight offers plus one fresh singleton check per selected control must examine at most sixteen offers, actual={checks}"
    );
    let progress = OpaqueMailboxProgress::new(
        &relay.public(),
        &sender.public(),
        Some(conversation),
        mailbox,
    );
    let selected = SelectedOpaqueControl::from_event(&expected[0]).unwrap();
    assert_eq!(
        node.selected_opaque_control(&progress, &selected, &|| true)
            .unwrap()
            .unwrap()
            .id,
        selected.id,
    );
    assert!(
        node.selected_opaque_control(&progress, &selected, &|| false)
            .unwrap()
            .is_none()
    );
    node.set_attachment_global_auto(false).unwrap();
    assert!(
        node.selected_opaque_control(&progress, &selected, &|| true)
            .unwrap()
            .is_none(),
        "a newly signed decision must not be sent under an old queued control ID"
    );
    let decision = super::super::file_consent::Decision::open(&expected[0]).unwrap();
    let replacement = node
        .file_consent_controls(
            &sender.public(),
            &[(decision.file, decision.manifest)],
            false,
        )
        .unwrap()
        .pop()
        .unwrap();
    assert_ne!(replacement.id, selected.id);
    let replacement = SelectedOpaqueControl::from_event(&replacement).unwrap();
    assert!(
        node.selected_opaque_control(&progress, &replacement, &|| true)
            .unwrap()
            .is_some()
    );
    node.delivery
        .lock()
        .unwrap()
        .erase_file(conversation, decision.manifest)
        .unwrap();
    assert!(
        node.selected_opaque_control(&progress, &replacement, &|| true)
            .unwrap()
            .is_none(),
        "erasing an offer must revoke its already queued control"
    );
    let other_decision = super::super::file_consent::Decision::open(&expected[1]).unwrap();
    let other = node
        .file_consent_controls(
            &sender.public(),
            &[(other_decision.file, other_decision.manifest)],
            false,
        )
        .unwrap()
        .pop()
        .unwrap();
    let other = SelectedOpaqueControl::from_event(&other).unwrap();
    assert!(
        node.selected_opaque_control(&progress, &other, &|| true)
            .unwrap()
            .is_some()
    );
    node.configure_privacy(
        directory.path(),
        "pw",
        &node.signed_announce("Local", 47474),
        std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let relay_account = Account::generate();
    let relay_proof = crate::discovery::Announce::new_post_office_with_account(
        &relay,
        &relay_account,
        "Relay",
        47476,
    );
    node.roster
        .lock()
        .unwrap()
        .update(&relay_proof, "127.0.0.1".parse().unwrap(), &node.user_id());
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_allowed(&relay_account.account_id(), true)
        .await
        .unwrap();
    node.set_allowed(&proof.account_id().unwrap(), true)
        .await
        .unwrap();
    node.set_invisible(true).await.unwrap();
    assert!(node.relay_send_allowed(&relay.public()));
    assert!(
        node.selected_opaque_control(&progress, &other, &|| true)
            .unwrap()
            .is_some()
    );
    node.set_allowed(&proof.account_id().unwrap(), false)
        .await
        .unwrap();
    assert!(node.relay_send_allowed(&relay.public()));
    assert!(
        node.selected_opaque_control(&progress, &other, &|| true)
            .unwrap()
            .is_none(),
        "current account revocation must deny queued controls"
    );
}

#[tokio::test]
async fn incremental_selected_identity_control_rechecks_current_binding() {
    let directory = tempfile::tempdir().unwrap();
    let remote = DeviceIdentity::generate();
    let remote_account = Account::generate();
    let proof =
        crate::discovery::Announce::new_with_account(&remote, &remote_account, "Remote", 47475);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    let source = directory.path().join("identity-control.bin");
    std::fs::write(&source, b"selected identity control").unwrap();
    let (card, _) = node
        .enqueue_file_to_account(
            &remote_account.account_id(),
            &source,
            crate::file::FileKind::File,
        )
        .await
        .unwrap();
    let bundle = node.file_message_identity_bundle(card).unwrap().unwrap();
    let selected = SelectedOpaqueControl::from_event(&bundle).unwrap();
    let SelectedOpaqueControlKind::Identity(references) = &selected.kind else {
        panic!("fixture must exercise the selected identity descriptor branch");
    };
    assert!(!references.is_empty());
    let relay = DeviceIdentity::generate();
    let mailbox = RelayMailbox::new(&node.identity, &remote.public(), &relay.public()).unwrap();
    let progress = OpaqueMailboxProgress::new(
        &relay.public(),
        &remote.public(),
        Some(bundle.conversation_id),
        mailbox,
    );
    let current = node
        .selected_opaque_control(&progress, &selected, &|| true)
        .unwrap()
        .unwrap();
    assert_eq!(current.id, bundle.id);
    assert_eq!(current.ciphertext, bundle.ciphertext);
    assert!(
        node.selected_opaque_control(&progress, &selected, &|| false)
            .unwrap()
            .is_none()
    );

    node.configure_privacy(
        directory.path(),
        "pw",
        &node.signed_announce("Local", 47474),
        std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let relay_account = Account::generate();
    let relay_proof = crate::discovery::Announce::new_post_office_with_account(
        &relay,
        &relay_account,
        "Relay",
        47476,
    );
    node.roster
        .lock()
        .unwrap()
        .update(&relay_proof, "127.0.0.1".parse().unwrap(), &node.user_id());
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_allowed(&relay_account.account_id(), true)
        .await
        .unwrap();
    node.set_allowed(&remote_account.account_id(), true)
        .await
        .unwrap();
    node.set_invisible(true).await.unwrap();
    assert!(node.relay_send_allowed(&relay.public()));
    assert!(
        node.selected_opaque_control(&progress, &selected, &|| true)
            .unwrap()
            .is_some()
    );
    node.set_allowed(&remote_account.account_id(), false)
        .await
        .unwrap();
    assert!(node.relay_send_allowed(&relay.public()));
    assert!(
        node.selected_opaque_control(&progress, &selected, &|| true)
            .unwrap()
            .is_none()
    );
    node.set_allowed(&remote_account.account_id(), true)
        .await
        .unwrap();
    assert!(
        node.selected_opaque_control(&progress, &selected, &|| true)
            .unwrap()
            .is_some()
    );

    // Every alias must lose its current grant; one surviving alias still authorizes this bundle.
    for (_, manifest) in references {
        let parent = node
            .log
            .lock()
            .unwrap()
            .event_conversation(manifest)
            .unwrap()
            .unwrap();
        node.delivery
            .lock()
            .unwrap()
            .erase_file(parent, *manifest)
            .unwrap();
        assert!(node.delivery.lock().unwrap().file_erased(parent, *manifest));
    }
    assert!(
        node.selected_opaque_control(&progress, &selected, &|| true)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn incremental_mailbox_warm_history_does_not_fetch_old_bodies() {
    for count in [32, 128] {
        let directory = tempfile::tempdir().unwrap();
        let relay_directory = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new_with_account(
            &remote,
            &Account::generate(),
            "Remote",
            47475,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let relay = DeviceIdentity::generate();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
            &relay_directory.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let conversation = super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &remote.public(),
        );
        let mut previous = None;
        for seq in 1..=count {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                previous.into_iter().collect(),
                seq,
                0,
                EventKind::Message,
                vec![7; 8192],
            );
            previous = Some(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        loop {
            if node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .prepare_kind_metadata_step(
                    crate::eventlog::sync::network::PreparationBudget::default(),
                )
                .unwrap()
                .ready
            {
                break;
            }
        }
        exchange(
            &node,
            office.clone(),
            &relay,
            &remote.public(),
            conversation,
        )
        .await;
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        assert!(
            !office
                .mailbox_delta_page(&mailbox.id(), None, 64)
                .unwrap()
                .ids
                .is_empty(),
            "fixture must actually upload history"
        );
        let view =
            crate::eventlog::sync::network::NetworkSyncStore::open_view(&office, &mailbox.id())
                .unwrap();
        assert_eq!(
            crate::eventlog::sync::network::NetworkSyncStore::metadata(&office, &view)
                .unwrap()
                .count,
            count,
            "initial exchange must place every historical wrapper in custody"
        );
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        exchange(
            &node,
            office.clone(),
            &relay,
            &remote.public(),
            conversation,
        )
        .await;
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "unchanged sync must not read or decode historical event bodies; history={count}"
        );
        let tail = Event::new(
            &node.identity,
            conversation,
            count + 1,
            previous.into_iter().collect(),
            count + 1,
            0,
            EventKind::Message,
            vec![8; 8192],
        );
        let tail_outer = mailbox.wrap(&tail).unwrap().id;
        node.log.lock().unwrap().append_durable(tail).unwrap();
        exchange(
            &node,
            office.clone(),
            &relay,
            &remote.public(),
            conversation,
        )
        .await;
        let after = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(
            after.0 - before.0 <= 8 && after.1 - before.1 <= 8,
            "one append must fetch only bounded new bodies: before={before:?}, after={after:?}, history={count}"
        );
        assert!(
            crate::eventlog::sync::network::NetworkSyncStore::durable_have(
                &office,
                &mailbox.id(),
                &tail_outer
            )
            .unwrap(),
            "tail must reach durable custody"
        );
        exchange(&node, office, &relay, &remote.public(), conversation).await;
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            after,
            "the next unchanged round must again be bodyless"
        );
    }
}

#[tokio::test]
async fn incremental_mailbox_invalidated_partial_upload_rebuilds_after_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let remote = DeviceIdentity::generate();
    let proof = crate::discovery::Announce::new_with_account(
        &remote,
        &Account::generate(),
        "Remote",
        47475,
    );
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let relay = DeviceIdentity::generate();
    let office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    for seq in 1..=128 {
        node.log
            .lock()
            .unwrap()
            .append_durable(Event::new(
                &node.identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![7; 32],
            ))
            .unwrap();
    }
    let keys = relay.secret_bytes();
    let endpoint = IndexedRelayEndpoint::new(office.clone(), None, None);
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let channel = crate::transport::SecureChannel::accept(server, &identity)
            .await
            .unwrap();
        endpoint
            .serve_connection_checked(channel, None)
            .await
            .unwrap();
    });
    let mut channel =
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let mut progress = OpaqueMailboxProgress::new(
        &relay.public(),
        &remote.public(),
        Some(conversation),
        mailbox,
    );
    // Negotiate, then admit one bounded preparation turn of a larger history.
    for _ in 0..2 {
        assert!(
            !node
                .advance_opaque_mailbox(&mut channel, &mut progress)
                .await
                .unwrap()
        );
    }
    assert!(
        office
            .mailbox_delta_page(&progress.mailbox.id(), None, 64)
            .unwrap()
            .ids
            .is_empty()
    );
    node.privacy
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    drop(progress);
    drop(channel);
    task.await.unwrap();
    exchange(
        &node,
        office.clone(),
        &relay,
        &remote.public(),
        conversation,
    )
    .await;
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let page = office.mailbox_delta_page(&mailbox.id(), None, 256).unwrap();
    assert_eq!(
        page.ids.len(),
        128,
        "a stale partial cache must not pin future rounds"
    );
    assert!(page.complete);
}

// Observe decoded requests at the actual Noise server boundary, before the
// production mailbox dispatcher handles them. No synthetic custody answers.
async fn counted_scope_exchange(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
    conversation: ConversationId,
    federated: bool,
    confirm: Option<EventId>,
) -> (usize, usize, Vec<(EventId, EventId)>) {
    let keys = relay.secret_bytes();
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let mut channel = crate::transport::SecureChannel::accept(server, &identity)
            .await
            .unwrap();
        let store = Mutex::new(office);
        let budget = crate::node::session::RemoteHaveBudget::default();
        let mut authorized = None;
        let mut frames = 0;
        let mut custody = 0;
        while let Ok(bytes) = channel.recv().await {
            frames += 1;
            custody +=
                usize::from(crate::node::session::durable_have_query_for_test(&bytes).is_some());
            crate::node::session::mailbox::serve(
                &mut channel,
                &store,
                &bytes,
                &budget,
                &mut authorized,
            )
            .await
            .unwrap();
        }
        (frames, custody)
    });
    let mut channel =
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
    let mailbox = if federated {
        RelayMailbox::federated(&node.identity, peer).unwrap()
    } else {
        RelayMailbox::new(&node.identity, peer, &relay.public()).unwrap()
    };
    let uploaded = node
        .opaque_mailbox_round_confirmed(&mut channel, peer, Some(conversation), mailbox, confirm, MailboxIntent::Duplex)
        .await
        .unwrap_or_else(|error| {
            panic!("counted scope exchange failed: federated={federated} confirm={confirm:?} conversation={conversation:?} error={error:?}")
        });
    if let Some(inner) = confirm {
        for (_, outer) in uploaded.iter().filter(|(id, _)| *id == inner) {
            assert!(
                crate::node::session::request_durable_have(
                    &mut channel,
                    if federated {
                        RelayMailbox::federated(&node.identity, peer).unwrap().id()
                    } else {
                        RelayMailbox::new(&node.identity, peer, &relay.public())
                            .unwrap()
                            .id()
                    },
                    *outer
                )
                .await
                .unwrap(),
                "exact target requires fresh server custody evidence"
            );
        }
    }
    drop(channel);
    let (frames, custody) = task.await.unwrap();
    (frames, custody, uploaded)
}

#[tokio::test]
async fn incremental_mailbox_six_scope_rotation_stays_metadata_and_network_quiet() {
    for count in [32, 128] {
        eprintln!("six-scope phase=prepare count={count}");
        let directory = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new_with_account(
            &remote,
            &Account::generate(),
            "Remote",
            47475,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.set_bridge_discovery_enabled(true);
        let conversation = super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &remote.public(),
        );
        let mut previous = None;
        for seq in 1..=count {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                previous.into_iter().collect(),
                seq,
                0,
                EventKind::Message,
                vec![7; 8192],
            );
            previous = Some(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        while
            !node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .prepare_kind_metadata_step(
                    crate::eventlog::sync::network::PreparationBudget::default(),
                )
                .unwrap()
                .ready
        {}
        let relay_directories: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let relays: Vec<_> = relay_directories
            .iter()
            .map(|root| {
                let identity = DeviceIdentity::generate();
                let office = IndexedPostOffice::open_with_lease(
                    ProfileStorageLease::acquire(root.path()).unwrap(),
                    &root.path().join("relay.log"),
                    "pw",
                    DeviceIdentity::generate(),
                )
                .unwrap();
                (identity, office)
            })
            .collect();
        // Three relays, two protocol mailbox scopes each: actual production
        // scope amplification exceeds the four heavyweight view slots.
        for (relay, office) in &relays {
            for federated in [false, true] {
                counted_scope_exchange(
                    &node,
                    office.clone(),
                    relay,
                    &remote.public(),
                    conversation,
                    federated,
                    None,
                )
                .await;
                let mailbox = if federated {
                    RelayMailbox::federated(&node.identity, &remote.public()).unwrap()
                } else {
                    RelayMailbox::new(&node.identity, &remote.public(), &relay.public()).unwrap()
                };
                use crate::eventlog::sync::network::NetworkSyncStore;
                let view = office.open_view(&mailbox.id()).unwrap();
                assert_eq!(
                    office.metadata(&view).unwrap().count,
                    count,
                    "every warmed scope must actually contain the entire historical prefix"
                );
            }
        }
        let bodies = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        for _ in 0..2 {
            incremental::HISTORY_METADATA_ROWS.with(|rows| rows.set(0));
            let mut frames = 0;
            let mut custody = 0;
            for (relay, office) in &relays {
                for federated in [false, true] {
                    let observed = counted_scope_exchange(
                        &node,
                        office.clone(),
                        relay,
                        &remote.public(),
                        conversation,
                        federated,
                        None,
                    )
                    .await;
                    frames += observed.0;
                    custody += observed.1;
                }
            }
            assert_eq!(
                custody, 0,
                "quiet scope rotation must not replay history custody probes"
            );
            assert_eq!(
                incremental::HISTORY_METADATA_ROWS.with(|rows| rows.get()),
                0,
                "quiet scope rotation must not prepare or enumerate historical selections"
            );
            assert!(
                frames <= 24,
                "quiet scopes require only bounded protocol probes"
            );
            assert_eq!(
                node.log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .body_read_counts(),
                bodies,
                "quiet scope rotation must stay bodyless"
            );
        }
        // A compact completion has no selected view. Exact old targets still
        // use the existing Principal/export owner, independently of recent64.
        let first = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .authored_header(
                &conversation,
                crate::eventlog::Author::from_ed25519(node.identity.public().ed25519_pub),
                1,
            )
            .unwrap()
            .unwrap()
            .id;
        let (relay, office) = &relays[0];
        eprintln!("six-scope phase=first-old-target-confirmation count={count}");
        let known = counted_scope_exchange(
            &node,
            office.clone(),
            relay,
            &remote.public(),
            conversation,
            false,
            Some(first),
        )
        .await;
        assert_eq!(known.1, 1, "old target needs one fresh exact custody query");
        assert!(known.2.iter().any(|(inner, _)| *inner == first));
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            bodies,
            "a signed target wrapper hint avoids reading its payload"
        );
        // The first target resolution may reconstruct its selected view. Once
        // qualified, the SAME old target must survive six-scope view rotation.
        for (scope_relay, (relay, office)) in relays.iter().enumerate() {
            for federated in [false, true] {
                eprintln!(
                    "six-scope phase=qualified-old-target-rotation count={count} relay={scope_relay} federated={federated}"
                );
                let observed = counted_scope_exchange(
                    &node,
                    office.clone(),
                    relay,
                    &remote.public(),
                    conversation,
                    federated,
                    Some(first),
                )
                .await;
                assert_eq!(observed.1, 1);
                assert!(observed.2.iter().any(|(inner, _)| *inner == first));
            }
        }
        for _ in 0..2 {
            incremental::HISTORY_METADATA_ROWS.with(|rows| rows.set(0));
            for (relay, office) in &relays {
                for federated in [false, true] {
                    let observed = counted_scope_exchange(
                        &node,
                        office.clone(),
                        relay,
                        &remote.public(),
                        conversation,
                        federated,
                        Some(first),
                    )
                    .await;
                    assert_eq!(
                        observed.1, 1,
                        "each qualified target still needs one fresh custody probe"
                    );
                    assert!(observed.2.iter().any(|(inner, _)| *inner == first));
                }
            }
            assert_eq!(
                incremental::HISTORY_METADATA_ROWS.with(|rows| rows.get()),
                0,
                "repeated qualified targets must not rebuild evicted historical selections"
            );
            assert_eq!(
                node.log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .body_read_counts(),
                bodies,
                "qualified target rotation must not read historical payloads"
            );
        }
        let mailbox = RelayMailbox::new(&node.identity, &remote.public(), &relay.public()).unwrap();
        // Keep completed upload evidence, but remove its optional target memo
        // and evict the live view to exercise only exact selected-body recovery.
        let checkpoint_key = bincode::serialize(&(
            relay.public(),
            remote.public(),
            mailbox.id(),
            Some(conversation),
        ))
        .unwrap();
        node.clear_mailbox_targets_for_test(&checkpoint_key);
        for (relay, office) in &relays {
            for federated in [false, true] {
                if relay.public() == relays[0].0.public() && !federated {
                    continue;
                }
                counted_scope_exchange(
                    &node,
                    office.clone(),
                    relay,
                    &remote.public(),
                    conversation,
                    federated,
                    None,
                )
                .await;
            }
        }
        let mut hint_key = b"wrapper-v1".to_vec();
        hint_key.extend_from_slice(mailbox.id().as_bytes());
        hint_key.extend_from_slice(first.as_bytes());
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .put_wrapper_hint(&hint_key, b"invalid-signature")
            .unwrap();
        let missing = counted_scope_exchange(
            &node,
            office.clone(),
            relay,
            &remote.public(),
            conversation,
            false,
            Some(first),
        )
        .await;
        assert_eq!(missing.1, 1);
        let after = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert_eq!(
            (after.0 - bodies.0, after.1 - bodies.1),
            (1, 1),
            "missing target hint must fetch exactly one selected body"
        );
        let absent = EventId::new([233; 32]);
        node.remember_wrapper_id(mailbox.id(), absent, first);
        let rejected = counted_scope_exchange(
            &node,
            office.clone(),
            relay,
            &remote.public(),
            conversation,
            false,
            Some(absent),
        )
        .await;
        assert!(
            !rejected.2.iter().any(|(inner, _)| *inner == absent),
            "a valid hint cannot authorize an unselected target"
        );
        assert_eq!(rejected.1, 0);
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            after
        );
        // A destructive remote fence invalidates the compact completion even
        // when all payloads happen to remain durable (no body work required).
        office.state.profile().database().transaction(|c| {
            c.execute("UPDATE event_conversations SET destructive_epoch=destructive_epoch+1 WHERE domain=?1 AND conversation=?2",
                rusqlite::params![crate::storage::profile::context::RELAY_EVENTS.0.as_slice(), mailbox.id().as_bytes().as_slice()])?;
            Ok(())
        }).unwrap();
        let reset = counted_scope_exchange(
            &node,
            office.clone(),
            relay,
            &remote.public(),
            conversation,
            false,
            None,
        )
        .await;
        assert!(
            reset.1 >= count as usize,
            "remote destructive reset must requalify history custody"
        );
        let quiet = counted_scope_exchange(
            &node,
            office.clone(),
            relay,
            &remote.public(),
            conversation,
            false,
            None,
        )
        .await;
        assert_eq!(quiet.1, 0);
        // Replacing the relay database under the same authenticated endpoint
        // must refill its mailbox rather than trusting the old store UUID.
        let replacement_root = tempfile::tempdir().unwrap();
        let replacement = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(replacement_root.path()).unwrap(),
            &replacement_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        counted_scope_exchange(
            &node,
            replacement.clone(),
            relay,
            &remote.public(),
            conversation,
            false,
            None,
        )
        .await;
        use crate::eventlog::sync::network::NetworkSyncStore;
        let view = replacement.open_view(&mailbox.id()).unwrap();
        assert_eq!(
            replacement.metadata(&view).unwrap().count,
            count,
            "replaced remote store must actually regain the complete prefix"
        );
        let quiet = counted_scope_exchange(
            &node,
            replacement,
            relay,
            &remote.public(),
            conversation,
            false,
            None,
        )
        .await;
        assert_eq!(quiet.1, 0);
        // Send-only clients must qualify remote custody without consuming the
        // incoming receive cursor or requiring the incoming finite drain.
        node.set_post_office_permissions(true, false);
        for (relay, office) in &relays {
            for federated in [false, true] {
                counted_scope_exchange(
                    &node,
                    office.clone(),
                    relay,
                    &remote.public(),
                    conversation,
                    federated,
                    None,
                )
                .await;
            }
        }
        for (relay, office) in &relays {
            for federated in [false, true] {
                let observed = counted_scope_exchange(
                    &node,
                    office.clone(),
                    relay,
                    &remote.public(),
                    conversation,
                    federated,
                    None,
                )
                .await;
                assert_eq!(observed.1, 0, "send-only completion survives view eviction");
            }
        }
    }
}
