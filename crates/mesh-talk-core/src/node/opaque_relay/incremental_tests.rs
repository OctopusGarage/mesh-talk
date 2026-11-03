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
        .opaque_mailbox_round_confirmed(&mut channel, peer, Some(conversation), mailbox, confirm)
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
        .opaque_mailbox_round_confirmed(&mut channel, peer, Some(conversation), mailbox, confirm)
        .await
        .unwrap();
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
