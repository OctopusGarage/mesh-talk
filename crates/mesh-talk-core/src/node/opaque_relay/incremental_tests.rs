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
async fn incremental_pending_consent_without_publication_stays_quiet_until_decision_changes() {
    use crate::{
        discovery::Announce, eventlog::sync::network::NetworkSyncStore, file::FileManifest,
    };
    use std::sync::Arc;
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
    node.set_attachment_global_auto(false).unwrap();
    node.cached_peer_snapshot_async().await.unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&sender.public(), &node.identity.public());
    let mut offers = Vec::new();
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
        offers.push((manifest.file_conv, original.id));
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
    assert!(
        node.file_consent_events(&sender.public())
            .unwrap()
            .is_empty()
    );
    // Missing retained evidence is unqualified, and a failed canonical read
    // remains an error rather than becoming no-control scheduling knowledge.
    assert!(
        node.prepare_file_consent_controls(
            &sender.public(),
            &[(ConversationId::new([99; 32]), EventId::new([99; 32]))],
            false,
        )
        .unwrap()
        .is_empty()
    );
    node.log.lock().unwrap().fail_read_for(Some(offers[0].1));
    let failed = node.prepare_file_consent_controls(&sender.public(), &offers, false);
    node.log.lock().unwrap().fail_read_for(None);
    assert!(failed.is_err());
    assert_eq!(
        node.opaque_upload_cache
            .lock()
            .unwrap()
            .observed_control_hints(),
        0
    );
    let qualified = node
        .prepare_file_consent_controls(&sender.public(), &offers, false)
        .unwrap();
    assert_eq!(
        qualified.len(),
        offers.len(),
        "the same exact offers recover after read admission returns"
    );
    assert!(qualified.iter().all(|outcome| matches!(
        outcome,
        super::super::file_consent::ConsentControlOutcome::PendingNoPublishedControl(..)
    )));
    let relay = DeviceIdentity::generate();
    let office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    // Mutate real policy after the helper returns. It must not learn a
    // no-control result across even an off/on transition with the same value.
    let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
        let changed = changed.clone();
        let weak = Arc::downgrade(&node);
        move |_, phase| {
            if phase == "consent_controls_prepared"
                && !changed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let node = weak.upgrade().unwrap();
                node.set_attachment_global_auto(true).unwrap();
                node.set_attachment_global_auto(false).unwrap();
            }
        }
    }));
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
    *node.account_client_test_hook.lock().unwrap() = None;
    assert!(changed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        node.opaque_upload_cache
            .lock()
            .unwrap()
            .observed_control_hints(),
        0,
        "changed scope after a qualified empty result must prevent hint admission"
    );
    // Warm the real mailbox and any one-time discovery checkpoint. This is a
    // fixed setup allowance, not retry-until-success or a warmed policy mock.
    for _ in 0..2 {
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
    }
    for (_, manifest) in &offers {
        assert!(
            node.file_consent
                .lock()
                .unwrap()
                .published_control_with_decision(*manifest)
                .unwrap()
                .is_none()
        );
    }
    super::super::file_consent::offered_observations::reset();
    let (_, custody, _) = counted_scope_exchange(
        &node,
        office.clone(),
        &relay,
        &sender.public(),
        conversation,
        true,
        None,
    )
    .await;
    assert_eq!(
        super::super::file_consent::offered_observations::count(),
        0,
        "fully checked pending offers with no published control must not repeat preparation under unchanged inputs"
    );
    assert_eq!(
        custody, 0,
        "no-control scheduling evidence is not relay custody"
    );
    for (_, manifest) in &offers {
        assert!(
            node.file_consent
                .lock()
                .unwrap()
                .published_control_with_decision(*manifest)
                .unwrap()
                .is_none()
        );
    }

    // Direct requests explicitly include Pending and must bypass a quiet
    // background hint, performing the original current-offer checks.
    let (file, manifest) = offers[0];
    super::super::file_consent::offered_observations::reset();
    let pending = node
        .direct_file_consent_control(&sender.public(), file, manifest)
        .unwrap()
        .expect("direct include_pending must remain available");
    assert_eq!(
        super::super::file_consent::Decision::open(&pending)
            .unwrap()
            .decision,
        crate::node::ReceiveDecision::Pending,
    );
    assert_eq!(super::super::file_consent::offered_observations::count(), 1);
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
    let mailbox = RelayMailbox::federated(&node.identity, &sender.public()).unwrap();
    assert!(NetworkSyncStore::durable_have(
        &office, &mailbox.id(), &mailbox.wrap(&pending).unwrap().id,
    ).unwrap(), "a newly published Pending control invalidates the no-control hint");

    // A real user decision changes the same offer. Its exact replacement must
    // reach actual relay custody on the next ordinary round, without a timer.
    node.decide_attachment(file, false).unwrap();
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
    let (declined, decision) = node
        .file_consent
        .lock()
        .unwrap()
        .published_control_with_decision(manifest)
        .unwrap()
        .unwrap();
    assert_eq!(decision.decision, crate::node::ReceiveDecision::Declined);
    assert_ne!(declined.id, pending.id);
    assert!(
        NetworkSyncStore::durable_have(
            &office,
            &mailbox.id(),
            &mailbox.wrap(&declined).unwrap().id,
        )
        .unwrap(),
        "the current explicit refusal must not inherit a pending scheduling hint"
    );
}

#[tokio::test]
async fn incremental_mailbox_revalidates_only_the_selected_consent_offer() {
    Box::pin(assert_incremental_consent_offer_selection(false)).await;
}

#[tokio::test]
async fn unrelated_relay_mailbox_keeps_send_only_consent_quiet() {
    Box::pin(assert_incremental_consent_offer_selection(true)).await;
}

async fn assert_incremental_consent_offer_selection(send_only: bool) {
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
    if send_only {
        // Exercise the production send-only delta path: every fresh adapter
        // supplies prior=None, even when its outgoing completion is qualified.
        node.set_post_office_permissions(true, false);
    }
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
    let mut expected = node.file_consent_events(&sender.public()).unwrap();
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
    // The first completed exchange may publish its final receive checkpoint.
    // One unchanged round establishes stable local/remote control evidence.
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
    node.storage.participant_database().unwrap().transaction(|connection| {
            connection.execute_batch("CREATE TEMP TABLE quiet_consent_probe(value INTEGER); INSERT INTO quiet_consent_probe VALUES(1),(2),(3); UPDATE main.participant_profile SET singleton=singleton WHERE 0")?;
        Ok(())
    }).unwrap();
    // Another real sync operation may build and retire a disposable SQL
    // selection while retained history and authorization remain unchanged.
    // Its bookkeeping must not invalidate confirmed consent work evidence.
    {
        let store = node.sync_store(&sender.public());
        let guarded = store.lock().unwrap();
        let view = NetworkSyncStore::open_view(&*guarded, &conversation).unwrap();
        let mut ready = false;
        for _ in 0..128 {
            if NetworkSyncStore::prepare_view_step(
                &*guarded,
                &view,
                crate::eventlog::sync::network::PreparationBudget::default(),
            )
            .unwrap()
            .ready
            {
                ready = true;
                break;
            }
        }
        assert!(ready, "real independent selection must finish preparation");
        drop(view);
    }
    // An independent mailbox may durably advance its transport checkpoint.
    // That is not a consent policy/source mutation for this settled mailbox.
    assert!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .put_checkpoint(b"independent-mailbox-progress", b"transport-progress")
            .unwrap()
    );
    // Scheduling a separate channel and retiring its queue entry changes no
    // retained source or consent authority for this settled mailbox.
    let scheduling_only = ConversationId::new([0xe7; 32]);
    node.channel_checkpoints
        .queue_runtime(scheduling_only)
        .unwrap();
    node.channel_checkpoints
        .finish_runtime(scheduling_only, None)
        .unwrap();
    super::super::file_consent::offered_observations::reset();
    let (_, custody, _) = counted_scope_exchange(
        &node,
        office.clone(),
        &relay,
        &sender.public(),
        conversation,
        true,
        None,
    )
    .await;
    assert_eq!(
        super::super::file_consent::offered_observations::count(),
        0,
        "unchanged confirmed consent controls must not rehydrate historical offers on every mailbox poll"
    );
    assert_eq!(
        custody, 0,
        "unchanged control evidence must not repeat per-event HAVE requests"
    );
    if !send_only {
        // Age only local scheduling metadata after the real Noise exchange has
        // proved all eight signed controls are in durable relay custody.
        assert_eq!(
            node.age_quiet_consent_hints_for_test(std::time::Duration::from_secs(75)),
            expected.len()
        );
        super::super::file_consent::offered_observations::reset();
        let (_, custody, _) = counted_scope_exchange(
            &node,
            office.clone(),
            &relay,
            &sender.public(),
            conversation,
            true,
            None,
        )
        .await;
        assert_eq!(
            super::super::file_consent::offered_observations::count(),
            0,
            "unchanged confirmed consent controls must stay quiet at 75 seconds"
        );
        assert_eq!(
            custody, 0,
            "unchanged confirmed controls must not repeat HAVE before sparse repair"
        );
        // Continue from the original check time. A quiet read must not renew it:
        // 75 + 226 seconds crosses the finite 300-second repair deadline.
        assert_eq!(
            node.age_quiet_consent_hints_for_test(std::time::Duration::from_secs(226)),
            expected.len()
        );
        super::super::file_consent::offered_observations::reset();
        let (_, custody, _) = counted_scope_exchange(
            &node,
            office.clone(),
            &relay,
            &sender.public(),
            conversation,
            true,
            None,
        )
        .await;
        assert!(
            super::super::file_consent::offered_observations::count() >= expected.len(),
            "301-second sparse repair must recheck the actual offers"
        );
        assert!(
            custody >= expected.len(),
            "expired scheduling evidence must resume actual per-control HAVE"
        );
    }
    if send_only {
        assert!(!node.relay_receive_allowed(&relay.public()));
        assert!(node.relay_send_allowed(&relay.public()));
        let before = office
            .mailbox_delta_page(&mailbox.id(), None, 64)
            .unwrap()
            .cursor;
        let other_sender = DeviceIdentity::generate();
        let other_recipient = DeviceIdentity::generate();
        let other_mailbox =
            RelayMailbox::federated(&other_sender, &other_recipient.public()).unwrap();
        assert_ne!(other_mailbox.id(), mailbox.id());
        let inner = Event::new(
            &other_sender,
            ConversationId::new([0xc1; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            crate::dm::seal(
                &other_sender,
                &other_recipient.public().x25519_pub,
                b"unrelated mailbox fixture",
            )
            .unwrap(),
        );
        let wrapper = other_mailbox.wrap(&inner).unwrap();
        let mut relay_writer = office.clone();
        NetworkSyncStore::ingest(&mut relay_writer, wrapper.clone()).unwrap();
        assert!(NetworkSyncStore::durable_have(&office, &other_mailbox.id(), &wrapper.id).unwrap());
        let after = office
            .mailbox_delta_page(&mailbox.id(), None, 64)
            .unwrap()
            .cursor;
        assert_eq!(after.store_uuid, before.store_uuid);
        assert_eq!(after.conversation, before.conversation);
        assert_eq!(after.epoch, before.epoch);
        assert_eq!(after.first_seen, before.first_seen);
        assert_eq!(after.revision, before.revision);
        assert!(
            after.after > before.after,
            "only the relay's global upper must advance"
        );
        super::super::file_consent::offered_observations::reset();
        let writes = node.storage.database().successful_write_commits();
        let (_, custody, _) = counted_scope_exchange(
            &node,
            office.clone(),
            &relay,
            &sender.public(),
            conversation,
            true,
            None,
        )
        .await;
        assert_eq!(
            super::super::file_consent::offered_observations::count(),
            0,
            "an unrelated relay mailbox must not rehydrate the eight settled consent offers"
        );
        assert_eq!(
            custody, 0,
            "unrelated mailbox traffic cannot repay settled control HAVE"
        );
        assert_eq!(
            node.storage.database().successful_write_commits(),
            writes,
            "unrelated relay traffic must not rewrite local selection or checkpoint progress"
        );

        // Growth in A itself is a different input and must retain the fresh
        // per-offer checks, even though its existing controls are still present.
        let same_inner = Event::new(
            &sender,
            conversation,
            99,
            vec![],
            99,
            1,
            EventKind::Message,
            crate::dm::seal(
                &sender,
                &node.identity.public().x25519_pub,
                b"same mailbox growth",
            )
            .unwrap(),
        );
        NetworkSyncStore::ingest(&mut relay_writer, mailbox.wrap(&same_inner).unwrap()).unwrap();
        let same = office
            .mailbox_delta_page(&mailbox.id(), None, 64)
            .unwrap()
            .cursor;
        assert!(same.revision > after.revision);
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
        assert!(
            super::super::file_consent::offered_observations::count() > 0,
            "same-mailbox append must retain fresh consent preparation"
        );
    }
    let replacement_directory = tempfile::tempdir().unwrap();
    let replacement_office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(replacement_directory.path()).unwrap(),
        &replacement_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    super::super::file_consent::offered_observations::reset();
    counted_scope_exchange(
        &node,
        replacement_office.clone(),
        &relay,
        &sender.public(),
        conversation,
        true,
        None,
    )
    .await;
    assert!(
        super::super::file_consent::offered_observations::count() > 0,
        "a replacement store behind the same relay identity must not inherit control confirmation"
    );
    for control in &expected {
        assert!(
            NetworkSyncStore::durable_have(
                &replacement_office,
                &mailbox.id(),
                &mailbox.wrap(control).unwrap().id
            )
            .unwrap()
        );
    }
    node.set_attachment_global_auto(false).unwrap();
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
    assert!(
        super::super::file_consent::offered_observations::count() > 0,
        "a changed local consent policy must bypass quiet control evidence immediately"
    );
    for control in node.file_consent_events(&sender.public()).unwrap() {
        assert!(
            NetworkSyncStore::durable_have(
                &office,
                &mailbox.id(),
                &mailbox.wrap(&control).unwrap().id
            )
            .unwrap()
        );
    }
    node.set_attachment_global_auto(true).unwrap();
    expected = node.file_consent_events(&sender.public()).unwrap();
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
    counted_scope_exchange_with_intent(
        node,
        office,
        relay,
        peer,
        conversation,
        federated,
        confirm,
        MailboxIntent::Duplex,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn counted_scope_exchange_with_intent(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
    conversation: ConversationId,
    federated: bool,
    confirm: Option<EventId>,
    intent: MailboxIntent,
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
        .opaque_mailbox_round_confirmed(&mut channel, peer, Some(conversation), mailbox, confirm, intent)
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
        let late = Event::new(
            &node.identity,
            conversation,
            count + 1,
            vec![],
            count + 1,
            0,
            EventKind::Message,
            vec![9; 128],
        );
        let absent = late.id;
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
        // Missing target lookup is also a quiet query. Warm each exact mailbox
        // once, then rotate beyond the four heavy view owners without rescanning.
        for (relay, office) in &relays {
            for federated in [false, true] {
                let rejected = counted_scope_exchange(
                    &node,
                    office.clone(),
                    relay,
                    &remote.public(),
                    conversation,
                    federated,
                    Some(absent),
                )
                .await;
                assert_eq!(rejected.1, 0);
                assert!(!rejected.2.iter().any(|(inner, _)| *inner == absent));
            }
        }
        incremental::HISTORY_METADATA_ROWS.with(|rows| rows.set(0));
        let negative_sql = (
            node.storage
                .database()
                .read(|c| Ok(c.total_changes()))
                .unwrap(),
            node.storage.database().successful_write_commits(),
        );
        for _ in 0..2 {
            for (relay, office) in &relays {
                for federated in [false, true] {
                    let rejected = counted_scope_exchange(
                        &node,
                        office.clone(),
                        relay,
                        &remote.public(),
                        conversation,
                        federated,
                        Some(absent),
                    )
                    .await;
                    assert_eq!(rejected.1, 0);
                    assert!(!rejected.2.iter().any(|(inner, _)| *inner == absent));
                }
            }
        }
        assert_eq!(
            incremental::HISTORY_METADATA_ROWS.with(|rows| rows.get()),
            0,
            "unchanged negative membership lookup must not rebuild historical selections"
        );
        assert_eq!(
            (
                node.storage
                    .database()
                    .read(|c| Ok(c.total_changes()))
                    .unwrap(),
                node.storage.database().successful_write_commits()
            ),
            negative_sql,
            "qualified negative queries must not persist optional runtime evidence or rewrite selections"
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
            replacement.clone(),
            relay,
            &remote.public(),
            conversation,
            false,
            None,
        )
        .await;
        assert_eq!(quiet.1, 0);
        // Negative membership is version-qualified, never permanent. The exact
        // previously absent target becomes selectable when its source arrives.
        // The earlier negative test deliberately signed a false wrapper mapping.
        // Retire that fixture poison so this phase isolates source invalidation;
        // an invalid optional hint must use the normal exact-body fallback.
        let mut late_hint = b"wrapper-v1".to_vec();
        late_hint.extend_from_slice(mailbox.id().as_bytes());
        late_hint.extend_from_slice(absent.as_bytes());
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .put_wrapper_hint(&late_hint, b"invalid-signature")
            .unwrap();
        node.log.lock().unwrap().append_durable(late).unwrap();
        let recovered = counted_scope_exchange(
            &node,
            replacement,
            relay,
            &remote.public(),
            conversation,
            false,
            Some(absent),
        )
        .await;
        assert!(
            recovered.2.iter().any(|(inner, _)| *inner == absent),
            "new source must invalidate old negative membership evidence"
        );
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

#[tokio::test]
async fn cancelled_ready_history_confirmation_releases_unrelated_scope_capacity() {
    use crate::{discovery::Announce, eventlog::sync::network::NetworkSyncStore};
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().unwrap();
    let recipient = DeviceIdentity::generate();
    let peer = recipient.public();
    let proof = Announce::new_with_account(&recipient, &Account::generate(), "Recipient", 9);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.set_bridge_discovery_enabled(true);
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &peer);
    let event = Event::new(
        &node.identity,
        conversation,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![7; 32],
    );
    let inner = event.id;
    node.log.lock().unwrap().append_durable(event).unwrap();
    let relays: Vec<_> = (0..3).map(|_| DeviceIdentity::generate()).collect();
    let scopes = [(0, false), (0, true), (1, false), (1, true), (2, false)];
    // Open all durable stores before measuring the unfinished-capacity window.
    let roots: Vec<_> = (0..10).map(|_| tempfile::tempdir().unwrap()).collect();
    let offices: Vec<_> = roots
        .iter()
        .map(|root| {
            IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &root.path().join("relay.log"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap()
        })
        .collect();
    let mut outers = Vec::new();
    for (index, &(relay, federated)) in scopes.iter().enumerate() {
        let published = tokio::time::timeout(
            Duration::from_secs(5),
            counted_scope_exchange_with_intent(
                &node,
                offices[index].clone(),
                &relays[relay],
                &peer,
                conversation,
                federated,
                None,
                MailboxIntent::HistoryPublication,
            ),
        )
        .await
        .expect("initial publication deadline");
        outers.push(published.2.iter().find(|(id, _)| *id == inner).unwrap().1);
    }
    // All five compact completions survive this single cold-owner transition.
    let retired = std::mem::take(&mut *node.opaque_upload_cache.lock().unwrap());
    drop(retired);
    let started = Instant::now();
    for (index, &(relay, federated)) in scopes[..4].iter().enumerate() {
        let relay = &relays[relay];
        let mailbox = if federated {
            RelayMailbox::federated(&node.identity, &peer).unwrap()
        } else {
            RelayMailbox::new(&node.identity, &peer, &relay.public()).unwrap()
        };
        let mailbox_id = mailbox.id();
        let outer = outers[index];
        let office = offices[index + 5].clone();
        assert!(!office.durable_have(&mailbox_id, &outer).unwrap());
        let keys = relay.secret_bytes();
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (entered, admitted) = tokio::sync::oneshot::channel();
        let mut task = tokio::spawn(async move {
            let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
            let mut channel = crate::transport::SecureChannel::accept(server, &identity)
                .await
                .map_err(|error| format!("server handshake: {error:?}"))?;
            let store = Mutex::new(office);
            let budget = crate::node::session::RemoteHaveBudget::default();
            let mut authorized = None;
            let mut exact_queries = 0;
            while let Ok(bytes) = channel.recv().await {
                if crate::node::session::durable_have_query_for_test(&bytes)
                    == Some((mailbox_id, outer))
                {
                    exact_queries += 1;
                    if exact_queries == 2 {
                        // The first HAVE was answered by the actual empty store.
                        // This second HAVE is emitted by upload_history_event
                        // only after a ready selected export supplied its body.
                        entered.send(()).map_err(|_| "admission observer closed")?;
                        return match tokio::time::timeout(Duration::from_secs(5), channel.recv())
                            .await
                        {
                            Ok(Err(_)) => Ok(()),
                            Ok(Ok(_)) => Err("cancelled client sent another request".into()),
                            Err(_) => Err("cancelled client transport did not retire".into()),
                        };
                    }
                }
                crate::node::session::mailbox::serve(
                    &mut channel,
                    &store,
                    &bytes,
                    &budget,
                    &mut authorized,
                )
                .await
                .map_err(|error| format!("mailbox dispatcher: {error:?}"))?;
            }
            Err::<(), String>("client closed before the ready repair request".into())
        });
        let mut channel = tokio::time::timeout(
            Duration::from_secs(5),
            crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public())),
        )
        .await
        .expect("client handshake deadline")
        .unwrap();
        let mut round = Box::pin(node.opaque_mailbox_round_confirmed(
            &mut channel,
            &peer,
            Some(conversation),
            mailbox,
            Some(inner),
            MailboxIntent::HistoryPublication,
        ));
        let admission = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut round => Err(format!("repair returned before its hold: {result:?}")),
                result = admitted => result.map_err(|_| "repair admission closed".to_owned()),
            }
        })
        .await;
        // Drop the actual operation while its ready repair is on the wire.
        // Retire its transport and server before inspecting any fixture result.
        drop(round);
        drop(channel);
        let retirement = tokio::time::timeout(Duration::from_secs(5), &mut task).await;
        if retirement.is_err() {
            task.abort();
            let _ = task.await;
        }
        admission.expect("ready repair admission deadline").unwrap();
        retirement
            .expect("server retirement deadline")
            .unwrap()
            .unwrap();
    }
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "cancellations must leave time to test within the 30-second protection window"
    );
    // A fifth real scope must acquire capacity immediately; expiry is not the
    // recovery mechanism. Its selected target also proves the exchange finished.
    let fifth = tokio::time::timeout(
        Duration::from_secs(5),
        counted_scope_exchange_with_intent(
            &node,
            offices[9].clone(),
            &relays[2],
            &peer,
            conversation,
            false,
            Some(inner),
            MailboxIntent::HistoryPublication,
        ),
    )
    .await
    .expect("unrelated fifth scope must not wait for unfinished protection expiry");
    assert!(fifth.2.contains(&(inner, outers[4])));
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[tokio::test]
async fn history_publication_legacy_confirmation_keeps_ordinary_custody_failure() {
    use crate::{discovery::Announce, eventlog::sync::network::NetworkSyncStore};
    use futures_util::FutureExt;
    let directory = tempfile::tempdir().unwrap();
    let recipient = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&recipient, &Account::generate(), "Recipient", 9);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let conversation = super::super::conversation::dm_conversation_id(
        &node.identity.public(),
        &recipient.public(),
    );
    let event = Event::new(
        &node.identity,
        conversation,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![7; 32],
    );
    let inner = event.id;
    let relay = DeviceIdentity::generate();
    let mailbox = RelayMailbox::new(&node.identity, &recipient.public(), &relay.public()).unwrap();
    let mailbox_id = mailbox.id();
    let outer = mailbox.wrap(&event).unwrap().id;
    node.log.lock().unwrap().append_durable(event).unwrap();
    let original_root = tempfile::tempdir().unwrap();
    let original = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(original_root.path()).unwrap(),
        &original_root.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let replacement_root = tempfile::tempdir().unwrap();
    let replacement = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(replacement_root.path()).unwrap(),
        &replacement_root.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let keys = relay.secret_bytes();
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let mut channel = crate::transport::SecureChannel::accept(server, &identity)
            .await
            .unwrap();
        let store = Mutex::new(original);
        let budget = crate::node::session::RemoteHaveBudget::default();
        // The actual first probe reaches the ordinary protocol handler, which
        // negotiates unsupported without forcing the client's internal mode.
        assert!(matches!(
            crate::node::session::serve_one_budgeted(&mut channel, &store, &budget)
                .await
                .unwrap(),
            crate::node::session::Served::Handled(_)
        ));
        let mut replacement = Some(replacement);
        let mut retired = false;
        while let Ok(bytes) = channel.recv().await {
            if crate::node::session::durable_have_query_for_test(&bytes)
                == Some((mailbox_id, outer))
            {
                let mut store = store.lock().unwrap();
                assert!(store.durable_have(&mailbox_id, &outer).unwrap());
                *store = replacement.take().expect("one exact custody query");
                assert!(!store.durable_have(&mailbox_id, &outer).unwrap());
                retired = true;
            }
            crate::node::session::serve_wire_bytes_budgeted(&mut channel, &store, &bytes, &budget)
                .await
                .unwrap();
        }
        retired
    });
    let mut channel =
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
    // Catch a regression panic so this fixture still retires its actual server.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        std::panic::AssertUnwindSafe(node.opaque_mailbox_round_confirmed(
            &mut channel,
            &recipient.public(),
            Some(conversation),
            mailbox,
            Some(inner),
            MailboxIntent::HistoryPublication,
        ))
        .catch_unwind(),
    )
    .await;
    let held = if matches!(&result, Ok(Ok(Ok(_)))) {
        Some(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                crate::node::session::request_durable_have(&mut channel, mailbox_id, outer),
            )
            .await,
        )
    } else {
        None
    };
    drop(channel);
    let retired = task.await.unwrap();
    let uploaded = result
        .expect("legacy round deadline")
        .expect("legacy confirmation must not panic")
        .expect("ordinary legacy publication");
    assert!(uploaded.contains(&(inner, outer)));
    assert!(
        retired,
        "the real stored target must disappear at the exact HAVE"
    );
    assert!(
        !held.unwrap().unwrap().unwrap(),
        "legacy dispatch is not current custody"
    );
}

#[tokio::test]
async fn history_publication_confirmation_rejects_source_change_during_positive_have() {
    use crate::{discovery::Announce, eventlog::sync::network::NetworkSyncStore};
    for federated in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let recipient = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&recipient, &Account::generate(), "Recipient", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.set_bridge_discovery_enabled(true);
        let conversation = super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &recipient.public(),
        );
        let event = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![7; 32],
        );
        let inner = event.id;
        node.log.lock().unwrap().append_durable(event).unwrap();
        let relay = DeviceIdentity::generate();
        let relay_root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(relay_root.path()).unwrap(),
            &relay_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let first = counted_scope_exchange_with_intent(
            &node,
            office.clone(),
            &relay,
            &recipient.public(),
            conversation,
            federated,
            None,
            MailboxIntent::HistoryPublication,
        )
        .await;
        let outer = first.2.iter().find(|(id, _)| *id == inner).unwrap().1;
        let mailbox = if federated {
            RelayMailbox::federated(&node.identity, &recipient.public()).unwrap()
        } else {
            RelayMailbox::new(&node.identity, &recipient.public(), &relay.public()).unwrap()
        };
        let mailbox_id = mailbox.id();
        assert!(office.durable_have(&mailbox_id, &outer).unwrap());
        let before = node
            .sync_store(&recipient.public())
            .lock()
            .unwrap()
            .history_sync_stamp(&conversation)
            .unwrap()
            .expect("known source stamp");
        let generation = node.privacy.generation.load(Ordering::SeqCst);
        let keys = relay.secret_bytes();
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (entered, admitted) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
            let mut channel = crate::transport::SecureChannel::accept(server, &identity)
                .await
                .unwrap();
            let store = Mutex::new(office);
            let budget = crate::node::session::RemoteHaveBudget::default();
            let mut authorized = None;
            let mut hold = Some((entered, released));
            while let Ok(bytes) = channel.recv().await {
                if crate::node::session::durable_have_query_for_test(&bytes)
                    == Some((mailbox_id, outer))
                    && let Some((entered, released)) = hold.take()
                {
                    assert!(
                        store
                            .lock()
                            .unwrap()
                            .durable_have(&mailbox_id, &outer)
                            .unwrap()
                    );
                    entered.send(()).unwrap();
                    tokio::time::timeout(std::time::Duration::from_secs(5), released)
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(
                        store
                            .lock()
                            .unwrap()
                            .durable_have(&mailbox_id, &outer)
                            .unwrap()
                    );
                }
                // The production dispatcher returns the real positive HAVE;
                // neither the response nor durable custody is fabricated.
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
            hold.is_none()
        });
        let mut channel =
            crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
                .await
                .unwrap();
        let mutate = async {
            tokio::time::timeout(std::time::Duration::from_secs(5), admitted)
                .await
                .unwrap()
                .unwrap();
            let fresh = Event::new(
                &node.identity,
                conversation,
                2,
                vec![inner],
                2,
                0,
                EventKind::Message,
                vec![8; 32],
            );
            node.log.lock().unwrap().append_durable(fresh).unwrap();
            let after = node
                .sync_store(&recipient.public())
                .lock()
                .unwrap()
                .history_sync_stamp(&conversation)
                .unwrap()
                .expect("changed known source stamp");
            assert_ne!(before, after);
            assert_eq!(node.privacy.generation.load(Ordering::SeqCst), generation);
            release.send(()).unwrap();
        };
        let recipient_public = recipient.public();
        let round = node.opaque_mailbox_round_confirmed(
            &mut channel,
            &recipient_public,
            Some(conversation),
            mailbox,
            Some(inner),
            MailboxIntent::HistoryPublication,
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(round, mutate).0
        })
        .await;
        drop(channel);
        let held = task.await.unwrap();
        assert!(
            held,
            "must hold the exact real positive HAVE before changing source"
        );
        assert!(
            matches!(
                result.unwrap(),
                Err(crate::node::session::SessionError::Store(
                    LogError::StaleCursor
                ))
            ),
            "source replacement while HAVE is pending must reject the old selection"
        );
    }
}

#[tokio::test]
async fn settled_history_publication_does_not_refill_replaced_relay_without_a_target() {
    use crate::{discovery::Announce, eventlog::sync::network::NetworkSyncStore};
    for federated in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let recipient = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&recipient, &Account::generate(), "Recipient", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.set_bridge_discovery_enabled(true);
        let conversation = super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &recipient.public(),
        );
        let mut messages = Vec::new();
        for sequence in 1..=3 {
            let event = Event::new(
                &node.identity,
                conversation,
                sequence,
                messages.last().copied().into_iter().collect(),
                sequence,
                0,
                EventKind::Message,
                vec![7; 512],
            );
            messages.push(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        let relay = DeviceIdentity::generate();
        let original_root = tempfile::tempdir().unwrap();
        let original = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(original_root.path()).unwrap(),
            &original_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let first = counted_scope_exchange_with_intent(
            &node,
            original.clone(),
            &relay,
            &recipient.public(),
            conversation,
            federated,
            None,
            MailboxIntent::HistoryPublication,
        )
        .await;
        let mailbox = if federated {
            RelayMailbox::federated(&node.identity, &recipient.public()).unwrap()
        } else {
            RelayMailbox::new(&node.identity, &recipient.public(), &relay.public()).unwrap()
        };
        for id in &messages {
            let outer = first.2.iter().find(|(inner, _)| inner == id).unwrap().1;
            assert!(original.durable_have(&mailbox.id(), &outer).unwrap());
        }
        let retired = std::mem::take(&mut *node.opaque_upload_cache.lock().unwrap());
        drop(retired);
        let replacement_root = tempfile::tempdir().unwrap();
        let replacement = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(replacement_root.path()).unwrap(),
            &replacement_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        counted_scope_exchange_with_intent(
            &node,
            replacement.clone(),
            &relay,
            &recipient.public(),
            conversation,
            federated,
            None,
            MailboxIntent::HistoryPublication,
        )
        .await;
        for id in &messages {
            let outer = first.2.iter().find(|(inner, _)| inner == id).unwrap().1;
            assert!(
                !replacement.durable_have(&mailbox.id(), &outer).unwrap(),
                "relay replacement is not a request to refill acknowledged source history"
            );
        }
        // The server answers the exact target's real HAVE, then closes before
        // accepting a repair. Failure cannot become a successful confirmation.
        let missing_outer = first
            .2
            .iter()
            .find(|(inner, _)| *inner == messages[0])
            .unwrap()
            .1;
        let relay_keys = relay.secret_bytes();
        let failed_store = replacement.clone();
        let mailbox_id = mailbox.id();
        let (client, server) = tokio::io::duplex(256 * 1024);
        let failure = tokio::spawn(async move {
            let identity = DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1);
            let mut channel = crate::transport::SecureChannel::accept(server, &identity)
                .await
                .unwrap();
            let store = Mutex::new(failed_store);
            let budget = crate::node::session::RemoteHaveBudget::default();
            let mut authorized = None;
            while let Ok(bytes) = channel.recv().await {
                let target = crate::node::session::durable_have_query_for_test(&bytes)
                    == Some((mailbox_id, missing_outer));
                crate::node::session::mailbox::serve(
                    &mut channel,
                    &store,
                    &bytes,
                    &budget,
                    &mut authorized,
                )
                .await
                .unwrap();
                if target {
                    assert!(
                        !store
                            .lock()
                            .unwrap()
                            .durable_have(&mailbox_id, &missing_outer)
                            .unwrap()
                    );
                    return;
                }
            }
            panic!("the explicit target must require an actual fresh HAVE");
        });
        let mut channel =
            crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
                .await
                .unwrap();
        let failed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            node.opaque_mailbox_round_confirmed(
                &mut channel,
                &recipient.public(),
                Some(conversation),
                if federated {
                    RelayMailbox::federated(&node.identity, &recipient.public()).unwrap()
                } else {
                    RelayMailbox::new(&node.identity, &recipient.public(), &relay.public()).unwrap()
                },
                Some(messages[0]),
                MailboxIntent::HistoryPublication,
            ),
        )
        .await
        .unwrap();
        assert!(failed.is_err());
        drop(channel);
        failure.await.unwrap();
        assert!(
            !replacement
                .durable_have(&mailbox.id(), &missing_outer)
                .unwrap()
        );
        let requested = counted_scope_exchange_with_intent(
            &node,
            replacement.clone(),
            &relay,
            &recipient.public(),
            conversation,
            federated,
            Some(messages[0]),
            MailboxIntent::HistoryPublication,
        )
        .await;
        let outer = requested
            .2
            .iter()
            .find(|(inner, _)| *inner == messages[0])
            .unwrap()
            .1;
        assert!(replacement.durable_have(&mailbox.id(), &outer).unwrap());
        for id in &messages[1..] {
            let outer = first.2.iter().find(|(inner, _)| inner == id).unwrap().1;
            assert!(
                !replacement.durable_have(&mailbox.id(), &outer).unwrap(),
                "explicit confirmation must not restore unrelated historical messages"
            );
        }
        let absent = EventId::new([0xff; 32]);
        let rejected = counted_scope_exchange_with_intent(
            &node,
            replacement.clone(),
            &relay,
            &recipient.public(),
            conversation,
            federated,
            Some(absent),
            MailboxIntent::HistoryPublication,
        )
        .await;
        assert!(!rejected.2.iter().any(|(inner, _)| *inner == absent));
        // The replacement above changed remote incarnation after the local
        // upload entry was evicted. Repeat both exact negative membership and
        // selected-target lookup; only the selected target needs fresh custody.
        for target in [absent, messages[0]] {
            for repeat in 0..2 {
                incremental::HISTORY_METADATA_ROWS.with(|rows| rows.set(0));
                let repeated = counted_scope_exchange_with_intent(
                    &node,
                    replacement.clone(),
                    &relay,
                    &recipient.public(),
                    conversation,
                    federated,
                    Some(target),
                    MailboxIntent::HistoryPublication,
                )
                .await;
                if target == absent {
                    assert!(!repeated.2.iter().any(|(inner, _)| *inner == absent));
                    assert_eq!(
                        repeated.1, 0,
                        "unselected target must not invent wrapper custody"
                    );
                } else {
                    assert!(repeated.2.iter().any(|(inner, _)| *inner == target));
                    assert_eq!(
                        repeated.1, 2,
                        "selected target requires the production exact HAVE and independent fixture HAVE"
                    );
                }
                if repeat == 1 {
                    assert_eq!(
                        incremental::HISTORY_METADATA_ROWS.with(|rows| rows.get()),
                        0,
                        "same target after eviction and relay replacement must not repeat local preparation: federated={federated} target={target:?}"
                    );
                }
            }
        }
        // Privacy changes invalidate local membership qualification as well as
        // current network admission, even when the target and relay are stable.
        node.privacy
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        incremental::HISTORY_METADATA_ROWS.with(|rows| rows.set(0));
        let requalified = counted_scope_exchange_with_intent(
            &node,
            replacement.clone(),
            &relay,
            &recipient.public(),
            conversation,
            federated,
            Some(messages[0]),
            MailboxIntent::HistoryPublication,
        )
        .await;
        assert!(requalified.2.iter().any(|(inner, _)| *inner == messages[0]));
        assert!(
            incremental::HISTORY_METADATA_ROWS.with(|rows| rows.get()) > 0,
            "changed privacy generation must requalify the selected source"
        );
        let event = Event::new(
            &node.identity,
            conversation,
            4,
            vec![*messages.last().unwrap()],
            4,
            0,
            EventKind::Message,
            vec![8; 512],
        );
        let fresh = event.id;
        node.log.lock().unwrap().append_durable(event).unwrap();
        let changed = counted_scope_exchange_with_intent(
            &node,
            replacement.clone(),
            &relay,
            &recipient.public(),
            conversation,
            federated,
            Some(fresh),
            MailboxIntent::HistoryPublication,
        )
        .await;
        let outer = changed
            .2
            .iter()
            .find(|(inner, _)| *inner == fresh)
            .unwrap()
            .1;
        assert!(
            replacement.durable_have(&mailbox.id(), &outer).unwrap(),
            "a changed local selection must still publish new messages"
        );
    }
}

#[tokio::test]
async fn history_publication_relay_eviction_reuses_the_exact_ready_selection() {
    use crate::{discovery::Announce, eventlog::sync::network::NetworkSyncStore};
    let directory = tempfile::tempdir().unwrap();
    let recipient = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&recipient, &Account::generate(), "Recipient", 9);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.set_bridge_discovery_enabled(true);
    node.cached_peer_snapshot_async().await.unwrap();
    let conversation = node
        .create_channel("Relay history retirement", vec![recipient.public()])
        .await
        .unwrap();
    let mut messages = Vec::new();
    for _ in 0..32 {
        node.await_channel_metadata(conversation).await.unwrap();
        messages.push(
            node.enqueue_channel_message_reply_expiring_if(
                conversation,
                &[b'h'; 1024],
                None,
                None,
                |commit| commit(),
            )
            .unwrap(),
        );
    }
    node.await_channel_metadata(conversation).await.unwrap();
    let database = node.storage.database();
    let selection_ids = || {
        database
            .read(|connection| {
                let mut statement = connection
                    .prepare("SELECT id FROM participant_work.participant_sync_views WHERE retired=0 ORDER BY id")?;
                let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .unwrap()
    };
    let roots: Vec<_> = (0..5).map(|_| tempfile::tempdir().unwrap()).collect();
    let relays: Vec<_> = roots
        .iter()
        .map(|root| {
            let relay = DeviceIdentity::generate();
            let office = IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &root.path().join("relay.log"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap();
            (relay, office)
        })
        .collect();
    let mut before_eviction = Vec::new();
    for (index, (relay, office)) in relays.iter().enumerate() {
        if index == 4 {
            before_eviction = selection_ids();
            assert_eq!(
                before_eviction.len(),
                4,
                "actual hot history budget is four"
            );
        }
        let mailbox = RelayMailbox::federated(&node.identity, &recipient.public()).unwrap();
        assert!(
            office
                .mailbox_delta_page(&mailbox.id(), None, 64)
                .unwrap()
                .ids
                .is_empty()
        );
        // Each relay is new and has no completed upload proof. The fifth actual
        // admission must retire an old UploadEntry outside its cache guard.
        let (_, _, uploaded) = counted_scope_exchange_with_intent(
            &node,
            office.clone(),
            relay,
            &recipient.public(),
            conversation,
            true,
            None,
            MailboxIntent::HistoryPublication,
        )
        .await;
        for id in &messages {
            let outer = uploaded.iter().find(|(inner, _)| inner == id).unwrap().1;
            assert!(NetworkSyncStore::durable_have(office, &mailbox.id(), &outer).unwrap());
        }
    }
    assert_eq!(
        selection_ids(),
        before_eviction,
        "a fresh relay must reuse the retired exact-principal SQL identities instead of rebuilding them"
    );
}
