use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};
use crate::postoffice::{IndexedPostOffice, federation::IndexedRelayEndpoint};
use crate::storage::profile::ProfileStorageLease;

#[tokio::test]
async fn incremental_unfinished_upload_survives_fifth_scope_pressure() {
    use crate::transport::SecureChannel;
    async fn connect(
        node: &Node,
        relay: &DeviceIdentity,
        office: IndexedPostOffice,
    ) -> (
        SecureChannel<tokio::net::TcpStream>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let keys = relay.secret_bytes();
        let endpoint = IndexedRelayEndpoint::new(office, None, None);
        let task = tokio::spawn(async move {
            let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
            let (socket, _) = listener.accept().await.unwrap();
            let channel = SecureChannel::accept(socket, &identity).await.unwrap();
            endpoint
                .serve_connection_checked(channel, None)
                .await
                .unwrap();
        });
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let channel = SecureChannel::connect(socket, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
        (channel, task)
    }
    async fn join(task: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("relay owner did not close")
            .unwrap();
    }
    Box::pin(tokio::time::timeout(std::time::Duration::from_secs(90), async {
        let directory = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new_with_account(
            &remote, &Account::generate(), "Remote", 47475);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(), DeviceIdentity::generate(), Account::generate(), &proof);
        node.cached_peer_snapshot_async().await.unwrap();
        node.set_post_office_permissions(true, true);
        let conversation = super::super::conversation::dm_conversation_id(
            &node.identity.public(), &remote.public());
        let mut expected_inner = std::collections::BTreeSet::new();
        for seq in 1..=128 {
            let event = Event::new(
                &node.identity, conversation, seq, vec![], seq, 0,
                EventKind::Message, vec![7; 32],
            );
            expected_inner.insert(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        let roots: Vec<_> = (0..5).map(|_| tempfile::tempdir().unwrap()).collect();
        let relays: Vec<_> = roots.iter().map(|root| {
            let relay = DeviceIdentity::generate();
            let office = IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &root.path().join("relay.log"), "pw", DeviceIdentity::generate(),
            ).unwrap();
            (relay, office)
        }).collect();
        // End each real round at the same bounded preparation boundary. Source
        // and authority stay unchanged, unlike the invalidated-partial control.
        for (relay, office) in relays.iter().take(4) {
            let (mut channel, task) = connect(&node, relay, office.clone()).await;
            let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
            let mut progress = OpaqueMailboxProgress::new(
                &relay.public(), &remote.public(), Some(conversation), mailbox);
            for _ in 0..2 {
                assert!(!node.advance_opaque_mailbox(&mut channel, &mut progress).await.unwrap());
            }
            assert!(office.mailbox_delta_page(&progress.mailbox.id(), None, 128).unwrap().ids.is_empty());
            drop(progress);
            drop(channel);
            join(task).await;
        }
        let database = node.storage.database();
        let active_ids = || database.read(|c| {
            let mut q = c.prepare("SELECT id FROM participant_sync_views WHERE retired=0 ORDER BY id")?;
            Ok(q.query_map([], |r| r.get::<_, Vec<u8>>(0))?.collect::<Result<Vec<_>, _>>()?)
        }).unwrap();
        let unfinished = active_ids();
        assert_eq!(unfinished.len(), 4, "fixture must own four unfinished SQL selections");
        // Observe actual selection creation, even if later cleanup deletes the
        // original row. This connection-local fixture records no SQL payloads.
        database.read(|c| {
            c.execute_batch("CREATE TEMP TABLE observed_upload_creations(id BLOB PRIMARY KEY);
                CREATE TEMP TRIGGER observe_upload_creation AFTER INSERT ON main.participant_sync_views
                BEGIN INSERT INTO observed_upload_creations VALUES(NEW.id); END;")?;
            Ok(())
        }).unwrap();
        let creations = || database.read(|c| Ok(c.query_row(
            "SELECT count(*) FROM temp.observed_upload_creations", [], |r| r.get::<_, i64>(0))?)).unwrap();
        let changes = database.read(|c| Ok(c.total_changes())).unwrap();
        let commits = database.successful_write_commits();
        let (relay, office) = &relays[4];
        let (mut channel, task) = connect(&node, relay, office.clone()).await;
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut progress = OpaqueMailboxProgress::new(
            &relay.public(), &remote.public(), Some(conversation), mailbox);
        let admission = node.advance_opaque_mailbox(&mut channel, &mut progress).await;
        drop(progress);
        drop(channel);
        join(task).await;
        assert!(matches!(admission, Err(crate::node::session::SessionError::ResourceUnavailable)),
            "fifth scope must defer rather than evict unchanged unfinished preparation: {admission:?}");
        assert_eq!(active_ids(), unfinished, "pressure must preserve admitted SQL selection identities");
        assert_eq!(creations(), 0, "pressure must not create replacement selections");
        assert_eq!(database.read(|c| Ok(c.total_changes())).unwrap(), changes,
            "deferred admission must not write retirement or replacement rows");
        assert_eq!(database.successful_write_commits(), commits);

        // Receiving is independent of the four unfinished history owners.
        // A fresh authenticated consent echo exercises GET and real Decision
        // processing without mutating those captured history/authority inputs.
        let (relay, office) = &relays[4];
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let consent = super::super::file_consent::Decision {
            file: ConversationId::new([219; 32]),
            manifest: EventId::new([220; 32]),
            sender: remote.public(),
            recipient: node.identity.public(),
            sender_account: proof.account_id(),
            recipient_account: node.account_id(),
            decision: crate::node::ReceiveDecision::Accepted,
            completion: None,
        }.event(&node.identity, 1).unwrap();
        let outer = mailbox.wrap(&consent).unwrap();
        let outer_id = outer.id;
        crate::eventlog::sync::network::NetworkSyncStore::ingest(&mut office.clone(), outer).unwrap();
        let reads = office.state.body_read_counts();
        let receive_mailbox_id = mailbox.id();
        let (mut channel, task) = connect(&node, relay, office.clone()).await;
        let received = node.opaque_mailbox_round_confirmed(&mut channel, &remote.public(),
            None, mailbox, None).await;
        drop(channel);
        join(task).await;
        assert!(received.is_ok(), "protected uploads must not starve receive-only GET: {received:?}");
        assert_ne!(office.state.body_read_counts(), reads, "receive-only round must perform actual GET");
        assert!(node.consent_transport_seen(receive_mailbox_id, outer_id).unwrap(),
            "receive-only GET must authenticate and process its Decision");
        assert!(!node.log.lock().unwrap().has(&consent.id).unwrap(),
            "transport classification must not manufacture retained custody");
        assert_eq!(active_ids(), unfinished, "receive processing must preserve four history selection owners");
        assert_eq!(creations(), 0, "receive-only admission must not construct HistoryUpload selections");

        // Two receive owners may be active; a third must defer until cancellation
        // releases a lane. No unbounded ephemeral fallback is acceptable.
        let mut receiving = Vec::new();
        for (relay, office) in relays.iter().take(2) {
            let (mut channel, task) = connect(&node, relay, office.clone()).await;
            let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
            let mut progress = OpaqueMailboxProgress::new(&relay.public(), &remote.public(), None, mailbox);
            assert!(!node.advance_opaque_mailbox(&mut channel, &mut progress).await.unwrap());
            receiving.push((progress, channel, task));
        }
        let (relay, office) = &relays[2];
        let (mut channel, task) = connect(&node, relay, office.clone()).await;
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut progress = OpaqueMailboxProgress::new(&relay.public(), &remote.public(), None, mailbox);
        let saturated = node.advance_opaque_mailbox(&mut channel, &mut progress).await;
        drop(progress);
        drop(channel);
        join(task).await;
        assert!(matches!(saturated, Err(crate::node::session::SessionError::ResourceUnavailable)),
            "a third busy receive scope must respect the two-owner bound: {saturated:?}");
        let (cancelled, channel, task) = receiving.pop().unwrap();
        drop(cancelled);
        drop(channel);
        join(task).await;
        let (mut channel, task) = connect(&node, relay, office.clone()).await;
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut progress = OpaqueMailboxProgress::new(&relay.public(), &remote.public(), None, mailbox);
        assert!(!node.advance_opaque_mailbox(&mut channel, &mut progress).await.unwrap(),
            "cancelled receive work must release admission capacity");
        drop(progress);
        drop(channel);
        join(task).await;
        for (progress, channel, task) in receiving {
            drop(progress);
            drop(channel);
            join(task).await;
        }
        assert_eq!(active_ids(), unfinished);
        assert_eq!(creations(), 0);

        let (relay, office) = &relays[0];
        let (mut channel, task) = connect(&node, relay, office.clone()).await;
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut progress = OpaqueMailboxProgress::new(
            &relay.public(), &remote.public(), Some(conversation), mailbox);
        assert!(!node.advance_opaque_mailbox(&mut channel, &mut progress).await.unwrap());
        assert_eq!(active_ids(), unfinished, "reacquisition must resume the same SQL cursor owner");
        let mut complete = false;
        for _ in 0..4096 {
            if node.advance_opaque_mailbox(&mut channel, &mut progress).await.unwrap() {
                complete = true;
                break;
            }
        }
        assert!(complete, "admitted work must finish under bounded continuation");
        assert_eq!(creations(), 0, "resuming unchanged work must not restart preparation");
        let mailbox_id = progress.mailbox.id();
        drop(progress);
        drop(channel);
        join(task).await;
        assert_eq!(office.mailbox_delta_page(&mailbox_id, None, 256).unwrap().ids.len(), 128);

        // A completed owner is now reclaimable; no fifth resident slot is needed.
        let (relay, office) = &relays[4];
        let (mut channel, task) = connect(&node, relay, office.clone()).await;
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mailbox_id = mailbox.id();
        node.opaque_mailbox_round_confirmed(&mut channel, &remote.public(),
            Some(conversation), mailbox, None).await.unwrap();
        drop(channel);
        join(task).await;
        let page = office.mailbox_delta_page(&mailbox_id, None, 256).unwrap();
        assert_eq!(page.ids.len(), 129,
            "fifth relay must retain exactly 128 uploaded history wrappers plus its seeded consent wrapper");
        assert!(page.ids.contains(&outer_id), "receiving must not remove the relay's seeded consent wrapper");
        assert!(page.complete);
        let decoder = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut actual_inner = std::collections::BTreeSet::new();
        for ids in page.ids.chunks(32) {
            for outer in office.mailbox_delta_events(&mailbox_id, &page.cursor, ids,
                crate::transport::MAX_PLAINTEXT - 2048).unwrap()
            {
                actual_inner.insert(decoder.open(&outer).unwrap().id);
            }
        }
        expected_inner.insert(consent.id);
        assert_eq!(actual_inner, expected_inner,
            "fifth relay must contain every exact history event and the seeded consent, without substitutes");
        assert_eq!(creations(), 1, "only the newly admitted fifth scope needs a new SQL selection");
        assert!((4..=6).contains(&node.opaque_upload_cache.lock().unwrap().observed_len()),
            "four history owners and at most two receive owners must remain bounded");
    })).await.expect("bounded upload admission failed to make progress");
}

#[tokio::test]
async fn incremental_receive_exact_consent_mapping_survives_reopen() {
    assert_consent_mapping_survives_reopen(false).await;
}

#[tokio::test]
async fn incremental_receive_own_consent_mapping_survives_reopen() {
    assert_consent_mapping_survives_reopen(true).await;
}

async fn assert_consent_mapping_survives_reopen(own: bool) {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let remote_account = Account::generate();
    let proof =
        crate::discovery::Announce::new_with_account(&remote, &remote_account, "Remote", 47475);
    let (node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let payload = super::super::file_consent::Decision {
        file: ConversationId::new([181; 32]),
        manifest: EventId::new([182; 32]),
        sender: if own {
            remote.public()
        } else {
            node.identity.public()
        },
        recipient: if own {
            node.identity.public()
        } else {
            remote.public()
        },
        sender_account: Some(if own {
            remote_account.public().account_id()
        } else {
            node.account_id()
        }),
        recipient_account: if own {
            node.account_id()
        } else {
            remote_account.public().account_id()
        },
        decision: crate::node::ReceiveDecision::Accepted,
        completion: None,
    };
    let event = payload
        .event(if own { &node.identity } else { &remote }, 1)
        .unwrap();
    if !own {
        // The exact durable application fact exists independently of raw log retention.
        node.file_consent
            .lock()
            .unwrap()
            .receive_control(event.clone())
            .unwrap();
    }
    let outer = mailbox.wrap(&event).unwrap();
    let outer_id = outer.id;
    office.ingest(outer).unwrap();
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let before = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    let classified = office.state.body_read_counts();
    assert_ne!(
        classified, before,
        "first classification must perform authenticated GET"
    );
    assert!(!node.log.lock().unwrap().has(&event.id).unwrap());
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .remove_checkpoint(&key)
        .unwrap();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts(),
        classified,
        "fresh operation must reuse classification"
    );
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .remove_checkpoint(&key)
        .unwrap();
    drop(node);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts(),
        classified,
        "reopen must reuse current durable consent evidence"
    );
    assert!(!node.log.lock().unwrap().has(&event.id).unwrap());
    assert!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .retained_seen_selected(mailbox.id(), &[outer_id], &node.log.lock().unwrap(),)
            .unwrap()
            .is_empty(),
        "consent classification must never grant retained custody"
    );
    let database = node.storage.database();
    let changes = database.read(|c| Ok(c.total_changes())).unwrap();
    let commits = database.successful_write_commits();
    let reads = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    for _ in 0..8 {
        assert!(node.consent_transport_seen(mailbox.id(), outer_id).unwrap());
    }
    assert_eq!(database.read(|c| Ok(c.total_changes())).unwrap(), changes);
    assert_eq!(database.successful_write_commits(), commits);
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        reads
    );
    let mut hint_key = b"consent-v1".to_vec();
    hint_key.extend_from_slice(&node.account.public().ed25519_pub);
    hint_key.extend_from_slice(mailbox.id().as_bytes());
    hint_key.extend_from_slice(outer_id.as_bytes());
    let saved = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_wrapper_hint(&hint_key)
        .unwrap()
        .unwrap();
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .put_wrapper_hint(&hint_key, b"corrupt")
        .unwrap();
    assert!(!node.consent_transport_seen(mailbox.id(), outer_id).unwrap());
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .put_wrapper_hint(&hint_key, &saved)
        .unwrap();
    database
        .transaction(|c| {
            c.execute(
                "UPDATE participant_profile SET account=?1 WHERE singleton=1",
                [[199u8; 32].as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        node.consent_transport_seen(mailbox.id(), outer_id),
        Err(LogError::StaleCursor)
    ));
    database
        .transaction(|c| {
            c.execute(
                "UPDATE participant_profile SET account=?1 WHERE singleton=1",
                [node.account.public().ed25519_pub.as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    database
        .transaction(|c| {
            c.execute_batch(
                "ALTER TABLE file_consent_received RENAME TO unavailable_consent_received",
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        node.consent_transport_seen(mailbox.id(), outer_id).is_err(),
        "authoritative failure cannot acknowledge an own echo either"
    );
    database
        .transaction(|c| {
            c.execute_batch(
                "ALTER TABLE unavailable_consent_received RENAME TO file_consent_received",
            )?;
            Ok(())
        })
        .unwrap();
    if own {
        let mut malformed_identity = payload.clone();
        malformed_identity.sender = node.identity.public();
        malformed_identity.recipient.x25519_pub[0] ^= 1;
        let other = malformed_identity.event(&node.identity, 1).unwrap();
        let other_outer = EventId::new([183; 32]);
        node.remember_consent_transport(mailbox.id(), other_outer, &other)
            .unwrap();
        assert!(
            !node
                .consent_transport_seen(mailbox.id(), other_outer)
                .unwrap(),
            "same Ed key with a different recipient X key is not an own echo"
        );
    } else {
        node.file_consent
            .lock()
            .unwrap()
            .receive_control(payload.event(&remote, 2).unwrap())
            .unwrap();
        assert!(
            !node.consent_transport_seen(mailbox.id(), outer_id).unwrap(),
            "supersession is not exact evidence"
        );
        database
            .transaction(|c| {
                c.execute(
                    "DELETE FROM file_consent_received WHERE manifest=?1",
                    [payload.manifest.as_bytes().as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(!node.consent_transport_seen(mailbox.id(), outer_id).unwrap());
    }
    let ordinary = ConversationId::new([184; 32]);
    node.append_event(ordinary, EventKind::Message, vec![0; 64])
        .unwrap();
    node.log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .drop_conversation(&ordinary)
        .unwrap();
    assert!(
        !node.consent_transport_seen(mailbox.id(), outer_id).unwrap(),
        "real destruction invalidates own classifications too"
    );
}

#[tokio::test]
async fn incremental_receive_profile_compaction_does_not_resurrect_retired_leaves() {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let remote_account = Account::generate();
    let proof =
        crate::discovery::Announce::new_with_account(&remote, &remote_account, "Remote", 47475);
    let (node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let mut profiles = Vec::new();
    for revision in 1..=12 {
        let payload = super::super::profile::ProfilePayload::new(&remote_account, None, revision);
        let event = Event::new(
            &remote,
            conversation,
            revision,
            vec![],
            revision,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &remote,
                &node.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
        profiles.push(event.id);
    }
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        profiles
            .iter()
            .all(|id| node.log.lock().unwrap().has(id).unwrap())
    );
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let checkpoint = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_checkpoint(&key)
        .unwrap();
    assert!(
        checkpoint.is_some(),
        "first drain must have a durable receipt"
    );
    let warm_reads = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts(),
        warm_reads,
        "unchanged warm receipt must skip GET"
    );

    let binding = || {
        let log = node.log.lock().unwrap();
        let repository = log.repository().unwrap();
        (
            repository.destructive_policy_revision().unwrap(),
            repository.source_revision().unwrap(),
        )
    };
    let mut observations = Vec::new();
    for cycle in 0..2 {
        let before = binding();
        let dropped = node
            .log
            .lock()
            .unwrap()
            .compact_superseded_profiles(&conversation)
            .unwrap();
        if cycle == 0 {
            assert_eq!(
                dropped, 10,
                "fixture must compact at least eight obsolete valid profile leaves"
            );
        }
        let compacted = binding();
        let absent: Vec<_> = profiles
            .iter()
            .copied()
            .filter(|id| !node.log.lock().unwrap().has(id).unwrap())
            .collect();
        assert_eq!(absent.len(), 10);
        if dropped != 0 {
            assert!(
                compacted.0 > before.0,
                "real compaction must advance global destructive authority"
            );
        }
        let reads_before = office.state.body_read_counts();
        exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
        let reads_after = office.state.body_read_counts();
        let resurrected = absent
            .iter()
            .filter(|id| node.log.lock().unwrap().has(id).unwrap())
            .count();
        let retained = profiles
            .iter()
            .filter(|id| node.log.lock().unwrap().has(id).unwrap())
            .count();
        let after = binding();
        observations.push((
            cycle,
            dropped,
            before,
            compacted,
            after,
            reads_before,
            reads_after,
            retained,
            resurrected,
        ));
    }
    // Maintenance in another conversation must not invalidate this mailbox's
    // receive coverage through a participant-global destructive revision.
    let unrelated = ConversationId::new([93; 32]);
    let mut unrelated_ids = Vec::new();
    for revision in 1..=12 {
        let payload = super::super::profile::ProfilePayload::new(&remote_account, None, revision);
        let event = Event::new(
            &remote,
            unrelated,
            revision,
            vec![],
            revision,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &remote,
                &node.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        unrelated_ids.push(event.id);
        node.log.lock().unwrap().append(event).unwrap();
    }
    assert!(
        node.log
            .lock()
            .unwrap()
            .compact_superseded_profiles(&unrelated)
            .unwrap()
            >= 10
    );
    assert_eq!(
        unrelated_ids
            .iter()
            .filter(|id| !node.log.lock().unwrap().has(id).unwrap())
            .count(),
        10,
        "the unrelated scope must actually compact ten Profiles"
    );
    let unrelated_before = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    let unrelated_after = office.state.body_read_counts();
    drop(node);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let restart_before = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    let restart_after = office.state.body_read_counts();
    assert_eq!(
        restart_before, restart_after,
        "the unchanged fixed remote set must remain quiet after reopening"
    );
    assert_eq!(
        unrelated_before, unrelated_after,
        "Profile maintenance in an unrelated conversation must preserve mailbox coverage"
    );
    println!(
        "profile compaction replay observations (no payloads or identifiers): {observations:?}"
    );
    assert!(
        observations.iter().all(
            |(_, _, _, _, _, before, after, _, resurrected)| before == after && *resurrected == 0
        ),
        "routine compaction must not re-fetch and resurrect the same superseded profiles: {observations:?}"
    );
}

#[tokio::test]
async fn incremental_receive_late_child_recovers_compacted_profile_parent_after_restart() {
    assert_late_profile_parent_recovery(0).await;
}

#[tokio::test]
async fn incremental_receive_late_child_recovers_without_optional_parent_mapping() {
    assert_late_profile_parent_recovery(1).await;
}

#[tokio::test]
async fn incremental_receive_late_child_recovers_when_optional_hint_writes_fail() {
    assert_late_profile_parent_recovery(2).await;
}

#[tokio::test]
async fn incremental_receive_parent_repair_fences_send_only_generation_change() {
    assert_late_profile_parent_recovery(3).await;
}

#[tokio::test]
async fn incremental_receive_parent_repair_skips_unopenable_opaque_wrapper() {
    assert_late_profile_parent_recovery(4).await;
}

async fn assert_late_profile_parent_recovery(cache_mode: u8) {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let remote_account = Account::generate();
    let proof =
        crate::discovery::Announce::new_with_account(&remote, &remote_account, "Remote", 47475);
    let (node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let foreign = if cache_mode == 4 {
        let event = Event::new(
            &DeviceIdentity::generate(),
            mailbox.id(),
            0,
            vec![],
            0,
            0,
            EventKind::Message,
            vec![0; 540],
        );
        assert!(crate::postoffice::opaque::envelope_shape(&event));
        assert!(mailbox.open(&event).is_err());
        office.ingest(event.clone()).unwrap();
        Some(event.id)
    } else {
        None
    };
    let mut profiles = Vec::new();
    for revision in 1..=12 {
        let payload = super::super::profile::ProfilePayload::new(&remote_account, None, revision);
        let event = Event::new(
            &remote,
            conversation,
            revision,
            vec![],
            revision,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &remote,
                &node.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
        profiles.push(event.id);
    }
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        profiles
            .iter()
            .all(|id| node.log.lock().unwrap().has(id).unwrap())
    );
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let checkpoint = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_checkpoint(&key)
        .unwrap();
    assert!(
        checkpoint.is_some(),
        "first drain must have a durable receipt"
    );
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .compact_superseded_profiles(&conversation)
            .unwrap(),
        10
    );
    let parent = profiles
        .iter()
        .copied()
        .find(|id| !node.log.lock().unwrap().has(id).unwrap())
        .expect("fixture must delete the future child's parent");
    let mut wrapper_key = b"wrapper-v1".to_vec();
    wrapper_key.extend_from_slice(mailbox.id().as_bytes());
    wrapper_key.extend_from_slice(parent.as_bytes());
    assert!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .get_wrapper_hint(&wrapper_key)
            .unwrap()
            .is_some(),
        "the selected-GET recovery fixture must have a persisted wrapper mapping"
    );
    if cache_mode != 0 {
        assert!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_wrapper_hint(&wrapper_key, b"unavailable optional hint")
                .unwrap()
        );
    }
    let node = if cache_mode == 4 {
        // Normal receive classified the foreign wrapper only for this runtime.
        // Preserve that valid cursor so this control exercises fallback rather
        // than intentionally triggering a cold session-only replay.
        node
    } else {
        drop(node);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            Account::from_secret_bytes(account_key),
            &proof,
        );
        node
    };
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    if cache_mode == 2 {
        node.storage.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_optional_receipt_insert BEFORE INSERT ON opaque_checkpoints BEGIN SELECT RAISE(ABORT,'optional receipt unavailable'); END;
                CREATE TRIGGER fail_optional_receipt_update BEFORE UPDATE ON opaque_checkpoints BEGIN SELECT RAISE(ABORT,'optional receipt unavailable'); END;
                CREATE TRIGGER fail_optional_wrapper_insert BEFORE INSERT ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional wrapper hint unavailable'); END;
                CREATE TRIGGER fail_optional_wrapper_update BEFORE UPDATE ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional wrapper hint unavailable'); END;")?;
            Ok(())
        }).unwrap();
    }
    // Make compaction capable of deleting the recovered old parent in the
    // interval before its child is installed. The hook exercises that exact
    // competing owner without scheduler timing assumptions.
    for revision in 20..28 {
        let payload = super::super::profile::ProfilePayload::new(&remote_account, None, revision);
        let event = Event::new(
            &remote,
            conversation,
            revision,
            vec![],
            revision,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &remote,
                &node.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        node.log.lock().unwrap().append(event).unwrap();
    }
    let parent_recoveries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = parent_recoveries.clone();
    let weak = std::sync::Arc::downgrade(&node);
    *node.account_client_test_hook.lock().unwrap() = Some(std::sync::Arc::new(move |_, stage| {
        if stage == "mailbox_parent_recovered" {
            let node = weak.upgrade().unwrap();
            if cache_mode == 3 && counted.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                node.set_post_office_permissions(true, true);
            }
            assert_eq!(
                node.drain_profile_compaction().unwrap(),
                None,
                "active parent-to-child recovery must exclude the real compactor"
            );
            assert!(node.log.lock().unwrap().has(&parent).unwrap());
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }));
    let child = Event::new(
        &remote,
        conversation,
        13,
        vec![parent],
        13,
        0,
        EventKind::Message,
        vec![13; 128],
    );
    office.ingest(mailbox.wrap(&child).unwrap()).unwrap();
    if cache_mode == 3 {
        let stale = exchange_receive_result(&node, office.clone(), &relay, &remote.public()).await;
        assert!(
            matches!(
                stale,
                Err(super::super::session::SessionError::Store(
                    LogError::StaleCursor
                ))
            ),
            "changing only send authority must fence the old active repair State"
        );
        assert!(!node.log.lock().unwrap().has(&child.id).unwrap());
        node.set_post_office_permissions(false, true);
    }
    let result = exchange_receive_result(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        result.is_ok(),
        "an unrelated unopenable opaque wrapper must not abort the finite parent repair: {result:?}"
    );
    if let Some(foreign) = foreign {
        assert!(!node.log.lock().unwrap().has(&foreign).unwrap());
        let sealed = node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&key)
            .unwrap()
            .unwrap();
        let sealed: (Vec<u8>, Vec<u8>) = bincode::deserialize(&sealed).unwrap();
        let saved: (
            Option<crate::postoffice::MailboxCursor>,
            Vec<EventId>,
            i64,
            i64,
            [u8; 16],
            bool,
        ) = bincode::deserialize(&sealed.0).unwrap();
        assert!(
            saved.5,
            "an unopenable wrapper is session-only transport classification, never durable admission"
        );
    }

    assert!(
        node.log.lock().unwrap().has(&parent).unwrap(),
        "a newly observed causal dependency must recover its compacted parent"
    );
    assert!(
        node.log.lock().unwrap().has(&child.id).unwrap(),
        "preserving the receive cursor must not strand a child whose earlier parent was compacted"
    );
    assert!(
        parent_recoveries.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "the causal-repair compactor interleaving must actually run"
    );
    *node.account_client_test_hook.lock().unwrap() = None;
    let recovered_reads = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts(),
        recovered_reads,
        "a recovered dependency must not trigger another unchanged-mailbox replay"
    );
}

#[tokio::test]
async fn incremental_receive_cancelled_parent_repair_resumes_after_node_restart() {
    assert_cancelled_parent_repair_progress(false, false, false, 0).await;
}

#[tokio::test]
async fn incremental_receive_cancelled_parent_repair_resumes_when_checkpoint_writes_fail() {
    assert_cancelled_parent_repair_progress(true, false, false, 0).await;
}

#[tokio::test]
async fn incremental_receive_cancelled_wide_parent_repair_protects_partial_parents() {
    assert_cancelled_parent_repair_progress(true, true, false, 0).await;
}

#[tokio::test]
async fn incremental_receive_cancelled_repair_recovers_backward_ancestor_without_hint_writes() {
    assert_cancelled_parent_repair_progress(true, false, true, 0).await;
}

#[tokio::test]
async fn incremental_receive_repair_progress_budget_excludes_held_initial_metadata() {
    assert_cancelled_parent_repair_progress(true, false, true, 1).await;
}

#[tokio::test]
async fn incremental_receive_repair_progress_deadline_cancels_held_body() {
    assert_cancelled_parent_repair_progress(true, false, true, 2).await;
}

async fn assert_cancelled_parent_repair_progress(
    checkpoint_writes_fail: bool,
    wide: bool,
    nested: bool,
    control: u8,
) {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let remote_account = Account::generate();
    let proof =
        crate::discovery::Announce::new_with_account(&remote, &remote_account, "Remote", 47475);
    let (mut node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let mut profiles = Vec::new();
    for revision in 1..=12 {
        let payload = super::super::profile::ProfilePayload::new(&remote_account, None, revision);
        let event = Event::new(
            &remote,
            conversation,
            revision,
            if nested && revision == 2 {
                vec![profiles[0]]
            } else {
                vec![]
            },
            revision,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &remote,
                &node.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
        profiles.push(event.id);
    }
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        profiles
            .iter()
            .all(|id| node.log.lock().unwrap().has(id).unwrap())
    );
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .compact_superseded_profiles(&conversation)
            .unwrap(),
        if nested { 9 } else { 10 }
    );
    if nested {
        for revision in 13..=20 {
            let payload =
                super::super::profile::ProfilePayload::new(&remote_account, None, revision);
            let event = Event::new(
                &remote,
                conversation,
                revision,
                vec![],
                revision,
                0,
                EventKind::Profile,
                crate::dm::seal(
                    &remote,
                    &node.identity.public().x25519_pub,
                    &payload.encode(),
                )
                .unwrap(),
            );
            office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
        }
        exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .compact_superseded_profiles(&conversation)
                .unwrap(),
            9
        );
        assert!(
            !node.log.lock().unwrap().has(&profiles[0]).unwrap(),
            "the earlier Profile ancestor must have become compactable after its child was compacted"
        );
        let mut key = b"wrapper-v1".to_vec();
        key.extend_from_slice(mailbox.id().as_bytes());
        key.extend_from_slice(profiles[0].as_bytes());
        assert!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_wrapper_hint(&key, b"unavailable optional ancestor mapping")
                .unwrap()
        );
    }
    let parent = profiles[if nested { 1 } else { 9 }];
    assert!(
        !node.log.lock().unwrap().has(&parent).unwrap(),
        "the required Profile must be late in the finite relay prefix and compacted locally"
    );
    let mut wrapper_key = b"wrapper-v1".to_vec();
    wrapper_key.extend_from_slice(mailbox.id().as_bytes());
    wrapper_key.extend_from_slice(parent.as_bytes());
    assert!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .put_wrapper_hint(&wrapper_key, b"unavailable optional parent mapping")
            .unwrap()
    );
    if wide {
        for id in &profiles[..10] {
            let mut key = b"wrapper-v1".to_vec();
            key.extend_from_slice(mailbox.id().as_bytes());
            key.extend_from_slice(id.as_bytes());
            assert!(
                node.opaque_receive_cache
                    .lock()
                    .unwrap()
                    .put_wrapper_hint(&key, b"unavailable optional parent mapping")
                    .unwrap()
            );
        }
    }
    let child = Event::new(
        &remote,
        conversation,
        if nested { 21 } else { 13 },
        if wide {
            profiles[..10].to_vec()
        } else {
            vec![parent]
        },
        if nested { 21 } else { 13 },
        0,
        EventKind::Message,
        vec![13; 128],
    );
    office.ingest(mailbox.wrap(&child).unwrap()).unwrap();
    if checkpoint_writes_fail {
        node.storage.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_repair_checkpoint_insert BEFORE INSERT ON opaque_checkpoints BEGIN SELECT RAISE(ABORT,'optional receipt unavailable'); END;
                CREATE TRIGGER fail_repair_checkpoint_update BEFORE UPDATE ON opaque_checkpoints BEGIN SELECT RAISE(ABORT,'optional receipt unavailable'); END;")?;
            Ok(())
        }).unwrap();
    }
    if nested {
        node.storage.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_repair_wrapper_insert BEFORE INSERT ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional wrapper hint unavailable'); END;
                CREATE TRIGGER fail_repair_wrapper_update BEFORE UPDATE ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional wrapper hint unavailable'); END;")?;
            Ok(())
        }).unwrap();
    }
    let frontier = std::sync::Arc::new(Mutex::new(Vec::<Option<(i64, usize)>>::new()));
    let attach_frontier = |node: &Node| {
        let frontier = frontier.clone();
        *node.account_client_test_hook.lock().unwrap() =
            Some(std::sync::Arc::new(move |_, stage| {
                if stage == "mailbox_repair_rewind" {
                    frontier.lock().unwrap().push(None);
                } else if let Some(numbers) = stage.strip_prefix("mailbox_repair_frontier ") {
                    let mut numbers = numbers.split_whitespace();
                    let after = numbers.next().unwrap().parse().unwrap();
                    let offset = numbers.next().unwrap().parse().unwrap();
                    frontier.lock().unwrap().push(Some((after, offset)));
                }
            }));
    };
    attach_frontier(&node);
    let before = office.state.body_read_counts();
    let mut cancelled = 0;
    let mut delivered = false;
    let mut attempts = Vec::new();
    for attempt in 0..if nested { 32 } else { 16 } {
        let attempt_before = office.state.body_read_counts();
        let completed = exchange_receive_with_cancellation(
            &node,
            office.clone(),
            &relay,
            &remote.public(),
            std::time::Duration::from_millis(120),
            if attempt == 0 { control } else { 0 },
        )
        .await;
        attempts.push((completed, attempt_before, office.state.body_read_counts()));
        let completed = completed.0;
        if attempt == 0 && control != 0 {
            let observation = &attempts[0].0;
            assert!(
                observation.1,
                "controlled attempt must admit exact repair body work: {observation:?}"
            );
            if control == 2 {
                assert!(
                    !completed,
                    "held repair body must be cancelled by the actual deadline"
                );
                assert!(
                    observation.3,
                    "server must hold the admitted repair request"
                );
            }
        }
        if !completed {
            cancelled += 1;
        }
        delivered = node.log.lock().unwrap().has(&child.id).unwrap();
        if delivered {
            break;
        }
        if wide {
            assert_eq!(
                node.drain_profile_compaction().unwrap(),
                None,
                "the active runtime job must protect earlier parents between watchdog attempts"
            );
        }
        if !checkpoint_writes_fail {
            // Drop all operation/runtime caches: signed finite scan progress
            // must survive a real participant reopen in the same account.
            drop(node);
            (node, _, _) = super::super::test_support::node_with_files(
                directory.path(),
                DeviceIdentity::from_secret_bytes(keys.0, keys.1),
                Account::from_secret_bytes(account_key),
                &proof,
            );
            node.cached_peer_snapshot_async().await.unwrap();
            node.set_post_office_permissions(false, true);
            attach_frontier(&node);
        }
    }
    let mut previous = None;
    let mut advanced = 0;
    for step in frontier.lock().unwrap().iter().copied() {
        if let Some(step) = step {
            assert!(
                previous.is_none_or(|prior| step > prior),
                "saved repair frontier must strictly advance between justified rewinds: {step:?} after {previous:?}"
            );
            previous = Some(step);
            advanced += 1;
        } else {
            previous = None;
        }
    }
    assert!(
        advanced > 0,
        "watchdog recovery must produce observed saved frontier progress"
    );
    assert!(
        cancelled > 0,
        "the fixture must cancel an admitted real mailbox round"
    );
    assert!(
        office.state.body_read_counts().0 - before.0 >= 2,
        "the cancelled attempts must reach body admission rather than fail during setup"
    );
    assert!(
        delivered,
        "repeated deadline cancellation must advance the finite repair prefix; restarting from zero strands a recoverable late parent; attempts={attempts:?}, frontier_tail={:?}",
        frontier
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
    );
    assert!(node.log.lock().unwrap().has(&parent).unwrap());
}

// Exercise cancellation of admitted repair work with delayed real responses.
// Setup has its own watchdog; this phase fixture does not model the production
// five-second watchdog's complete operation budget.
async fn exchange_receive_with_cancellation(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
    deadline: std::time::Duration,
    control: u8,
) -> (bool, bool, usize, bool) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let admitted = std::sync::Arc::new(AtomicBool::new(false));
    let admission = std::sync::Arc::new(tokio::sync::Notify::new());
    let previous_hook = node.account_client_test_hook.lock().unwrap().clone();
    let saved_hook = previous_hook.clone();
    let body_admitted = admitted.clone();
    let body_admission = admission.clone();
    *node.account_client_test_hook.lock().unwrap() =
        Some(std::sync::Arc::new(move |peer, stage| {
            if stage == "mailbox_repair_body_admitted" {
                body_admitted.store(true, Ordering::SeqCst);
                body_admission.notify_one();
            }
            if let Some(hook) = &saved_hook {
                hook(peer, stage);
            }
        }));
    let metadata_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let held = std::sync::Arc::new(AtomicBool::new(false));
    let server_admitted = admitted.clone();
    let server_entered = metadata_entered.clone();
    let server_release = release.clone();
    let server_requests = requests.clone();
    let server_held = held.clone();
    let keys = relay.secret_bytes();
    let (client, server) = tokio::io::duplex(256 * 1024);
    let mut task = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let mut channel = crate::transport::SecureChannel::accept(server, &identity)
            .await
            .unwrap();
        let store = Mutex::new(office);
        let budget = super::super::session::RemoteHaveBudget::default();
        let mut authorized = None;
        while let Ok(bytes) = channel.recv().await {
            if bytes.starts_with(b"MTOM1") {
                let index = server_requests.fetch_add(1, Ordering::SeqCst);
                if control == 1 && index == 0 {
                    server_entered.notify_one();
                    server_release.notified().await;
                } else if control == 2 && server_admitted.load(Ordering::SeqCst) {
                    server_held.store(true, Ordering::SeqCst);
                    server_release.notified().await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            if super::super::session::mailbox::serve(
                &mut channel,
                &store,
                &bytes,
                &budget,
                &mut authorized,
            )
            .await
            .is_err()
            {
                break;
            }
        }
    });
    let connected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public())),
    )
    .await;
    let mut channel = match connected {
        Ok(Ok(channel)) => channel,
        failure => {
            task.abort();
            let _ = task.await;
            *node.account_client_test_hook.lock().unwrap() = previous_hook;
            panic!("repair fixture Noise setup failed: {:?}", failure.err());
        }
    };
    let mailbox = RelayMailbox::federated(&node.identity, peer).unwrap();
    // An explicit request-arrival signal controls the initial metadata hold.
    let metadata_release = release.clone();
    let controller = tokio::spawn(async move {
        if control == 1
            && tokio::time::timeout(
                std::time::Duration::from_secs(5),
                metadata_entered.notified(),
            )
            .await
            .is_ok()
        {
            tokio::time::sleep(std::time::Duration::from_millis(160)).await;
            metadata_release.notify_one();
        }
    });
    let mut setup_expired = false;
    let result = {
        let round = node.opaque_mailbox_round(&mut channel, peer, None, mailbox);
        tokio::pin!(round);
        let setup = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = &mut round => Some(result),
                _ = admission.notified() => None,
            }
        })
        .await;
        match setup {
            Ok(Some(result)) => Ok(result),
            Ok(None) => tokio::time::timeout(deadline, &mut round).await,
            Err(error) => {
                setup_expired = true;
                Err(error)
            }
        }
    };
    // The scoped pinned round is dropped before releasing or joining the server.
    controller.await.unwrap();
    release.notify_one();
    drop(channel);
    let joined = tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await;
    if joined.is_err() {
        task.abort();
        let _ = task.await;
    }
    *node.account_client_test_hook.lock().unwrap() = previous_hook;
    joined.expect("relay cleanup watchdog").unwrap();
    assert!(
        !setup_expired,
        "repair fixture setup watchdog expired before exact body admission"
    );
    let completed = match result {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => panic!("repair failed outside deadline cancellation: {error}"),
        Err(_) => false,
    };
    (
        completed,
        admitted.load(Ordering::SeqCst),
        requests.load(Ordering::SeqCst),
        held.load(Ordering::SeqCst),
    )
}

#[tokio::test]
async fn incremental_receive_processed_profile_transport_mapping_survives_fresh_operation_and_reopen()
 {
    assert_semantic_transport_mapping_reuse(false).await;
}

#[tokio::test]
async fn incremental_receive_own_profile_transport_mapping_survives_fresh_operation_and_reopen() {
    assert_semantic_transport_mapping_reuse(true).await;
}

async fn assert_semantic_transport_mapping_reuse(own: bool) {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let remote_account = Account::generate();
    let proof =
        crate::discovery::Announce::new_with_account(&remote, &remote_account, "Remote", 47475);
    let (node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let (author, author_account, recipient) = if own {
        (&node.identity, &node.account, remote.public())
    } else {
        (&remote, &remote_account, node.identity.public())
    };
    let payload = super::super::profile::ProfilePayload::new(author_account, None, 1);
    let event = Event::new(
        author,
        conversation,
        1,
        vec![],
        1,
        0,
        EventKind::Profile,
        crate::dm::seal(author, &recipient.x25519_pub, &payload.encode()).unwrap(),
    );
    let outer = mailbox.wrap(&event).unwrap();
    let outer_id = outer.id;
    office.ingest(outer).unwrap();
    if !own {
        // Exact durable presentation evidence may outlive its raw Profile.
        node.profile_receive_work
            .lock()
            .unwrap()
            .complete(event.id)
            .unwrap();
    }
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let first_reads = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    let classified_reads = office.state.body_read_counts();
    assert_ne!(
        classified_reads, first_reads,
        "first classification must authenticate a real GET"
    );
    assert!(!node.log.lock().unwrap().has(&event.id).unwrap());
    assert!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .retained_seen_selected(mailbox.id(), &[outer_id], &node.log.lock().unwrap(),)
            .unwrap()
            .is_empty(),
        "semantic classification must not advertise retained HAVE or custody"
    );
    // Exact semantic reads are quiet and fail closed on absent facts or a
    // corrupt optional mapping. Neither condition can become retained HAVE.
    let database = node.storage.database();
    let changes = database.read(|c| Ok(c.total_changes())).unwrap();
    let commits = database.successful_write_commits();
    for _ in 0..8 {
        assert!(node.semantic_profile_seen(mailbox.id(), outer_id).unwrap());
    }
    assert_eq!(database.read(|c| Ok(c.total_changes())).unwrap(), changes);
    assert_eq!(database.successful_write_commits(), commits);
    if !own {
        database
            .transaction(|c| {
                c.execute(
                    "DELETE FROM profile_receive_processed WHERE id=?1",
                    [event.id.as_bytes().as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(!node.semantic_profile_seen(mailbox.id(), outer_id).unwrap());
        node.profile_receive_work
            .lock()
            .unwrap()
            .complete(event.id)
            .unwrap();
    }
    let mut semantic_key = b"semantic-profile-v1".to_vec();
    semantic_key.extend_from_slice(&node.account.public().ed25519_pub);
    semantic_key.extend_from_slice(mailbox.id().as_bytes());
    semantic_key.extend_from_slice(outer_id.as_bytes());
    let saved = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_wrapper_hint(&semantic_key)
        .unwrap()
        .unwrap();
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .put_wrapper_hint(&semantic_key, b"corrupt semantic mapping")
        .unwrap();
    assert!(!node.semantic_profile_seen(mailbox.id(), outer_id).unwrap());
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .put_wrapper_hint(&semantic_key, &saved)
        .unwrap();
    let saved_account = node.account.public().ed25519_pub;
    database
        .transaction(|c| {
            c.execute(
                "UPDATE participant_profile SET account=?1 WHERE singleton=1",
                [[199u8; 32].as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        node.semantic_profile_seen(mailbox.id(), outer_id),
        Err(LogError::StaleCursor)
    ));
    database
        .transaction(|c| {
            c.execute(
                "UPDATE participant_profile SET account=?1 WHERE singleton=1",
                [saved_account.as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    database.transaction(|c| {
        c.execute_batch("CREATE TEMP TRIGGER fail_semantic_hint BEFORE INSERT ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional semantic hint unavailable'); END;")?;
        Ok(())
    }).unwrap();
    let uncached_outer = EventId::new([197; 32]);
    node.remember_semantic_profile(mailbox.id(), uncached_outer, event.id, own)
        .unwrap();
    assert!(
        !node
            .semantic_profile_seen(mailbox.id(), uncached_outer)
            .unwrap()
    );
    database
        .transaction(|c| {
            c.execute_batch("DROP TRIGGER fail_semantic_hint")?;
            Ok(())
        })
        .unwrap();
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .remove_checkpoint(&key)
        .unwrap();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts(),
        classified_reads,
        "a fresh operation must reuse exact durable semantic classification without GET"
    );
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .remove_checkpoint(&key)
        .unwrap();
    drop(node);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts(),
        classified_reads,
        "relogin must reuse successful semantic classification without GET"
    );
    assert!(!node.log.lock().unwrap().has(&event.id).unwrap());
    assert!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .retained_seen_selected(mailbox.id(), &[outer_id], &node.log.lock().unwrap(),)
            .unwrap()
            .is_empty(),
        "reopened semantic mapping must remain outside retained HAVE/custody"
    );

    // Real retained-history deletion must revoke the new semantic shortcut,
    // even when the exact presentation fact itself remains durable.
    let ordinary = ConversationId::new([196; 32]);
    node.log
        .lock()
        .unwrap()
        .append(Event::new(
            &node.identity,
            ordinary,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![1],
        ))
        .unwrap();
    let before_delete = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .receive_repair_revision()
        .unwrap();
    node.log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .drop_conversation(&ordinary)
        .unwrap();
    assert!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .receive_repair_revision()
            .unwrap()
            > before_delete
    );
    assert!(!node.semantic_profile_seen(mailbox.id(), outer_id).unwrap());
    let reads_before_repair = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_ne!(
        office.state.body_read_counts(),
        reads_before_repair,
        "real deletion must force GET before reclassifying semantic transport progress"
    );
    assert!(!node.log.lock().unwrap().has(&event.id).unwrap());

    // A new authenticated semantic event still finishes its real GET exchange
    // when optional mapping publication fails; reopen must retry that GET.
    let (author, author_account, recipient) = if own {
        (&node.identity, &node.account, remote.public())
    } else {
        (&remote, &remote_account, node.identity.public())
    };
    let payload = super::super::profile::ProfilePayload::new(author_account, None, 2);
    let next = Event::new(
        author,
        conversation,
        2,
        vec![],
        2,
        0,
        EventKind::Profile,
        crate::dm::seal(author, &recipient.x25519_pub, &payload.encode()).unwrap(),
    );
    let next_outer = mailbox.wrap(&next).unwrap();
    let next_outer_id = next_outer.id;
    office.ingest(next_outer).unwrap();
    if !own {
        node.profile_receive_work
            .lock()
            .unwrap()
            .complete(next.id)
            .unwrap();
    }
    node.storage.database().transaction(|c| {
        c.execute_batch("CREATE TEMP TRIGGER fail_semantic_transport_insert BEFORE INSERT ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional semantic transport insert unavailable'); END;")?;
        Ok(())
    }).unwrap();
    let reads_before_failure = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_ne!(office.state.body_read_counts(), reads_before_failure);
    assert!(
        !node
            .semantic_profile_seen(mailbox.id(), next_outer_id)
            .unwrap()
    );
    assert!(!node.log.lock().unwrap().has(&next.id).unwrap());
    node.storage
        .database()
        .transaction(|c| {
            c.execute_batch("DROP TRIGGER fail_semantic_transport_insert")?;
            Ok(())
        })
        .unwrap();
    node.opaque_receive_cache
        .lock()
        .unwrap()
        .remove_checkpoint(&key)
        .unwrap();
    drop(node);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let retry_reads = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_ne!(
        office.state.body_read_counts(),
        retry_reads,
        "missing optional mapping must fall back to a successful GET after reopen"
    );
    assert!(
        node.semantic_profile_seen(mailbox.id(), next_outer_id)
            .unwrap()
    );
    assert!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .retained_seen_selected(
                mailbox.id(),
                &[outer_id, next_outer_id],
                &node.log.lock().unwrap(),
            )
            .unwrap()
            .is_empty(),
        "repair and cache failure must never manufacture retained custody"
    );
}

#[tokio::test]
async fn incremental_receive_compacted_profiles_survive_missing_checkpoint() {
    assert_profile_semantic_replay(0).await;
}

#[tokio::test]
async fn incremental_receive_compacted_profiles_survive_corrupt_checkpoint() {
    assert_profile_semantic_replay(1).await;
}

#[tokio::test]
async fn incremental_receive_compacted_profiles_survive_relay_store_reset() {
    assert_profile_semantic_replay(2).await;
}

#[tokio::test]
async fn incremental_receive_unknown_profile_with_older_sequence_is_not_suppressed() {
    assert_profile_semantic_replay(3).await;
}

#[tokio::test]
async fn incremental_receive_compacted_own_profiles_survive_missing_checkpoint() {
    assert_profile_semantic_replay(4).await;
}

#[tokio::test]
async fn incremental_receive_compacted_undiscovered_profiles_survive_missing_checkpoint() {
    assert_profile_semantic_replay(5).await;
}

async fn assert_profile_semantic_replay(mode: u8) {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let remote_account = Account::generate();
    let proof =
        crate::discovery::Announce::new_with_account(&remote, &remote_account, "Remote", 47475);
    let (node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    if mode == 5 {
        // A direct synchronization batch can append more than one discovery
        // page before its single profile reception pass. Keep the profiles
        // behind one full page of unrelated committed events.
        let unrelated = ConversationId::new([94; 32]);
        for revision in 1..=128 {
            node.log
                .lock()
                .unwrap()
                .append(Event::new(
                    &remote,
                    unrelated,
                    revision,
                    vec![],
                    revision,
                    0,
                    EventKind::Message,
                    vec![1],
                ))
                .unwrap();
        }
    }
    let mut profiles = Vec::new();
    for revision in 1..=12 {
        let (author, author_account, recipient) = if mode == 4 {
            (&node.identity, &node.account, remote.public())
        } else {
            (&remote, &remote_account, node.identity.public())
        };
        // Preserve the ordinary maintenance byte threshold despite the backlog.
        let avatar = (mode == 5).then(|| vec![42; 1024]);
        let payload = super::super::profile::ProfilePayload::new(author_account, avatar, revision);
        let event = Event::new(
            author,
            conversation,
            revision,
            vec![],
            revision,
            0,
            EventKind::Profile,
            crate::dm::seal(author, &recipient.x25519_pub, &payload.encode()).unwrap(),
        );
        if mode == 4 || mode == 5 {
            // Model retained historical outgoing profiles or a direct-sync
            // batch, independently of the optional relay receipt cache.
            node.log.lock().unwrap().append(event.clone()).unwrap();
        }
        office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
        profiles.push(event);
    }
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    if mode == 4 || mode == 5 {
        node.process_profile_events(conversation);
    }
    assert!(
        profiles
            .iter()
            .all(|event| node.log.lock().unwrap().has(&event.id).unwrap())
    );
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let checkpoint = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_checkpoint(&key)
        .unwrap();
    assert!(
        checkpoint.is_some(),
        "first drain must have a durable receipt"
    );
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .compact_superseded_profiles(&conversation)
            .unwrap(),
        10
    );
    let retired: Vec<_> = profiles
        .iter()
        .map(|event| event.id)
        .filter(|id| !node.log.lock().unwrap().has(id).unwrap())
        .collect();
    assert_eq!(retired.len(), 10);
    let replacement_directory = tempfile::tempdir().unwrap();
    match mode {
        0 | 4 | 5 => node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .remove_checkpoint(&key)
            .unwrap(),
        1 => assert!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_checkpoint(&key, b"corrupt checkpoint")
                .unwrap()
        ),
        2 => {
            office = IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(replacement_directory.path()).unwrap(),
                &replacement_directory.path().join("relay.log"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap();
            for event in &profiles {
                office.ingest(mailbox.wrap(event).unwrap()).unwrap();
            }
        }
        3 => {}
        _ => unreachable!(),
    }
    // Real reopen retains the account-scoped semantic processing facts while
    // discarding runtime-only wrapper hints and operation state.
    drop(node);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    if mode == 3 {
        let payload = super::super::profile::ProfilePayload::new(&remote_account, None, 13);
        let unknown = Event::new(
            &remote,
            conversation,
            0,
            vec![],
            13,
            0,
            EventKind::Profile,
            crate::dm::seal(
                &remote,
                &node.identity.public().x25519_pub,
                &payload.encode(),
            )
            .unwrap(),
        );
        assert!(!profiles.iter().any(|event| event.id == unknown.id));
        office.ingest(mailbox.wrap(&unknown).unwrap()).unwrap();
        exchange_receive_only(&node, office, &relay, &remote.public()).await;
        assert!(
            node.log.lock().unwrap().has(&unknown.id).unwrap(),
            "an exact-ID processing fact must not suppress an unknown older-sequence Profile"
        );
        return;
    }
    let before = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .source_revision()
        .unwrap();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        retired
            .iter()
            .all(|id| !node.log.lock().unwrap().has(id).unwrap()),
        "loss of optional transport hints must not resurrect semantically handled Profile bodies"
    );
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .source_revision()
            .unwrap(),
        before,
        "semantic replay must not append raw events or request new compaction"
    );
    let warm_reads = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts(),
        warm_reads,
        "the new checkpoint must make the repaired fixed relay scope quiet"
    );
}

#[tokio::test]
async fn incremental_receive_revocation_preserves_unhandled_page_for_restore() {
    use crate::eventlog::sync::network::NetworkSyncStore;
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
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let mut inner_ids = Vec::new();
    for sequence in 1..=2 {
        let event = Event::new(
            &remote,
            conversation,
            sequence,
            vec![],
            sequence,
            0,
            EventKind::Message,
            vec![sequence as u8; 128],
        );
        inner_ids.push(event.id);
        office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
    }
    assert_eq!(
        office
            .mailbox_delta_page(&mailbox.id(), None, 64)
            .unwrap()
            .ids
            .len(),
        2
    );
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let endpoint = IndexedRelayEndpoint::new(office, None, None);
    let keys = relay.secret_bytes();
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
    let mut progress = OpaqueMailboxProgress::new(&relay.public(), &remote.public(), None, mailbox);
    assert!(
        !node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
    );
    assert!(
        !node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
    );
    let received = || {
        inner_ids
            .iter()
            .filter(|id| node.log.lock().unwrap().has(id).unwrap())
            .count()
    };
    assert_eq!(received(), 1, "fixture must pause inside a two-ID page");
    let before = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_checkpoint(&key)
        .unwrap();
    assert!(
        before.is_none(),
        "partial pages must not checkpoint their cursor"
    );
    node.set_post_office_permissions(false, false);
    let interrupted = node
        .advance_opaque_mailbox(&mut channel, &mut progress)
        .await;
    assert!(
        matches!(
            interrupted,
            Err(super::super::session::SessionError::Store(
                LogError::AdmissionRejected(_)
            ))
        ),
        "disabled receive must interrupt the page with an admission rejection"
    );
    assert_eq!(received(), 1);
    assert_eq!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&key)
            .unwrap(),
        before,
        "an unhandled ID cannot advance the saved cursor"
    );
    node.set_post_office_permissions(false, true);
    let mut done = false;
    for _ in 0..8 {
        if node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
        {
            done = true;
            break;
        }
    }
    assert!(done, "restored receive must finish its retained page");
    assert_eq!(
        received(),
        2,
        "the ID interrupted by revocation must be retried"
    );
    drop(channel);
    task.await.unwrap();
}

// Receive-only fixtures intentionally omit history upload: their signed DM
// bodies exercise storage/DAG admission rather than a live ratchet session.
async fn exchange_receive_only(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
) {
    exchange_receive_result(node, office, relay, peer)
        .await
        .unwrap();
}

async fn exchange_receive_result(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
) -> Result<Vec<(EventId, EventId)>, super::super::session::SessionError> {
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
    let result = node
        .opaque_mailbox_round(&mut channel, peer, None, mailbox)
        .await;
    drop(channel);
    task.await.unwrap();
    result
}

#[tokio::test]
async fn incremental_receive_restart_preserves_child_pending_and_repairs_missing_inner() {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let proof = crate::discovery::Announce::new_with_account(
        &remote,
        &Account::generate(),
        "Remote",
        47475,
    );
    let (node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let parent = Event::new(
        &remote,
        conversation,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![1; 128],
    );
    let child = Event::new(
        &remote,
        conversation,
        2,
        vec![parent.id],
        2,
        0,
        EventKind::Message,
        vec![2; 128],
    );
    let child_outer = mailbox.wrap(&child).unwrap();
    office.ingest(child_outer.clone()).unwrap();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        !node.log.lock().unwrap().has(&child.id).unwrap(),
        "child must remain pending while its parent is absent"
    );
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let receipt = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_checkpoint(&key)
        .unwrap()
        .expect("completed receive page must persist its pending IDs");
    let sealed: (Vec<u8>, Vec<u8>) = bincode::deserialize(&receipt).unwrap();
    let saved: (
        Option<crate::postoffice::MailboxCursor>,
        Vec<EventId>,
        i64,
        i64,
        [u8; 16],
        bool,
    ) = bincode::deserialize(&sealed.0).unwrap();
    assert!(saved.0.is_some());
    assert_eq!(
        saved.1,
        vec![child_outer.id],
        "restart recovery requires the exact deferred wrapper ID"
    );
    assert!(
        !saved.5,
        "a missing-parent deferral is durable rather than session-only progress"
    );
    for _ in 0..3 {
        let before = office.state.body_read_counts();
        exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
        let after = office.state.body_read_counts();
        assert_eq!(
            after.0 - before.0,
            1,
            "an unchanged unresolved witness may retry its child, but cannot rescan the mailbox"
        );
    }
    drop(node);
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let before = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        (1..=2).contains(&(office.state.body_read_counts().0 - before.0)),
        "cold restart may reacquire one finite repair, while preserving the main receive cursor"
    );
    let before = office.state.body_read_counts();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert_eq!(
        office.state.body_read_counts().0 - before.0,
        1,
        "the new hot owner must qualify each unchanged pending child without another scan"
    );
    office.ingest(mailbox.wrap(&parent).unwrap()).unwrap();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(node.log.lock().unwrap().has(&parent.id).unwrap());
    assert!(
        node.log.lock().unwrap().has(&child.id).unwrap(),
        "the saved cursor excludes the old child; its durable pending ID must retry it after the new parent arrives"
    );
    node.log
        .lock()
        .unwrap()
        .drop_conversation(&conversation)
        .unwrap();
    assert!(!node.log.lock().unwrap().has(&child.id).unwrap());
    exchange_receive_only(&node, office, &relay, &remote.public()).await;
    assert!(node.log.lock().unwrap().has(&parent.id).unwrap());
    assert!(
        node.log.lock().unwrap().has(&child.id).unwrap(),
        "retained wrapper hints cannot suppress recovery of genuinely missing inner events"
    );
}

#[tokio::test]
async fn incremental_receive_rejects_corrupt_or_unsigned_modified_checkpoint_after_restart() {
    use crate::eventlog::sync::network::NetworkSyncStore;
    for tampered_body in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let relay_directory = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let keys = identity.secret_bytes();
        let account = Account::generate();
        let account_key = account.secret_bytes();
        let remote = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new_with_account(
            &remote,
            &Account::generate(),
            "Remote",
            47475,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            identity,
            account,
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        node.set_post_office_permissions(false, true);
        let relay = DeviceIdentity::generate();
        let mut office = IndexedPostOffice::open_with_lease(
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
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let parent = Event::new(
            &remote,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![1; 128],
        );
        let child = Event::new(
            &remote,
            conversation,
            2,
            vec![parent.id],
            2,
            0,
            EventKind::Message,
            vec![2; 128],
        );
        let child_outer = mailbox.wrap(&child).unwrap();
        office.ingest(child_outer.clone()).unwrap();
        exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
        assert!(!node.log.lock().unwrap().has(&child.id).unwrap());
        let key = bincode::serialize(&(
            relay.public(),
            remote.public(),
            mailbox.id(),
            Option::<ConversationId>::None,
        ))
        .unwrap();
        let original = node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&key)
            .unwrap()
            .expect("fixture must persist a signed pending checkpoint");
        let invalid = if tampered_body {
            // Preserve a structurally valid completed cursor and all local owner
            // bindings, but erase its pending child without signing the edit.
            // Accepting this blob would fetch only the new parent and lose child.
            let mut sealed: (Vec<u8>, Vec<u8>) = bincode::deserialize(&original).unwrap();
            let mut receipt: (
                Option<crate::postoffice::MailboxCursor>,
                Vec<EventId>,
                i64,
                i64,
                [u8; 16],
                bool,
            ) = bincode::deserialize(&sealed.0).unwrap();
            assert!(receipt.0.is_some());
            assert_eq!(receipt.1, vec![child_outer.id]);
            assert!(!receipt.5);
            receipt.1.clear();
            sealed.0 = bincode::serialize(&receipt).unwrap();
            bincode::serialize(&sealed).unwrap()
        } else {
            b"corrupt optional checkpoint".to_vec()
        };
        assert_ne!(invalid, original);
        assert!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_checkpoint(&key, &invalid)
                .unwrap()
        );
        drop(node);
        office.ingest(mailbox.wrap(&parent).unwrap()).unwrap();
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            Account::from_secret_bytes(account_key),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        node.set_post_office_permissions(false, true);
        exchange_receive_only(&node, office, &relay, &remote.public()).await;
        assert!(node.log.lock().unwrap().has(&parent.id).unwrap());
        assert!(
            node.log.lock().unwrap().has(&child.id).unwrap(),
            "invalid optional checkpoint must fall back to the retained mailbox: tampered_body={tampered_body}"
        );
        assert_ne!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .get_checkpoint(&key)
                .unwrap()
                .unwrap(),
            invalid,
            "successful reconciliation must replace the rejected optional hint"
        );
    }
}
