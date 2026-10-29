use super::*;
use store::AccountDocumentStore;
fn key() -> AccountDocumentKey {
    AccountDocumentKey::Draft {
        conversation: DocumentConversation::Account("a".repeat(32)),
    }
}
#[test]
fn signed_documents_survive_restart_and_enforce_cas_tombstones_and_account_scope() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let writer = DeviceIdentity::generate().public().user_id();
    let mut store =
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
    let first = store
        .put(
            &account,
            writer.clone(),
            key(),
            b"private draft",
            None,
            false,
        )
        .unwrap();
    assert!(first.valid(&account.public().ed25519_pub));
    assert!(
        store
            .put(&account, writer.clone(), key(), b"stale", None, false)
            .is_err()
    );
    drop(store);
    let mut store =
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
    assert_eq!(
        store.get(&key()).unwrap().unwrap().payload(),
        b"private draft"
    );
    let removed = store
        .put(&account, writer, key(), &[], Some(&first.version()), true)
        .unwrap();
    assert!(removed.tombstone);
    assert!(store.get(&key()).unwrap().is_none());
    assert!(
        !format!(
            "{:?}",
            AccountDocument {
                meta: first,
                payload: b"private draft".to_vec()
            }
        )
        .contains("private draft")
    );
    let other = Account::generate();
    assert!(
        AccountDocumentStore::open(dir.path(), "pw", &other.public().ed25519_pub)
            .unwrap()
            .meta(&key())
            .is_none()
    );
}
fn node(path: &std::path::Path, account: &Account) -> std::sync::Arc<Node> {
    node_with_identity(path, account, DeviceIdentity::generate())
}
fn node_with_identity(
    path: &std::path::Path,
    account: &Account,
    identity: DeviceIdentity,
) -> std::sync::Arc<Node> {
    use crate::discovery::Roster;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    std::fs::create_dir_all(path).unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let (ctx, _) = mpsc::unbounded_channel();
    let (ftx, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        identity,
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        tx,
        ctx,
        ftx,
        &path.join("events.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap()
}

#[tokio::test]
async fn channel_custody_is_shared_with_certified_sibling_while_holder_is_offline() {
    channel_custody_sibling_scenario(true).await;
}

#[tokio::test]
async fn channel_custody_never_counts_sender_sibling_as_other_member() {
    channel_custody_sibling_scenario(false).await;
}

async fn channel_custody_sibling_scenario(external_online: bool) {
    use crate::{discovery::Announce, node::account_devices::AccountDevice};
    use std::{net::IpAddr, time::Duration};
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let other = Account::generate();
    let a = node(&dir.path().join("a"), &account);
    let sibling = node(&dir.path().join("sibling"), &account);
    let bob = node(&dir.path().join("bob"), &other);
    for (left, right) in [(&a, &sibling), (&sibling, &a)] {
        left.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: right.identity.public(),
                certificate: account.certify(&right.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let mut servers = Vec::new();
    for (current, owner) in [(&a, &account), (&sibling, &account), (&bob, &other)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        for observer in [&a, &sibling, &bob] {
            if observer.user_id() != current.user_id() {
                observer.roster.lock().unwrap().update(
                    &Announce::new_with_account(
                        &current.identity,
                        owner,
                        "fixture",
                        address.port(),
                    ),
                    IpAddr::from([127, 0, 0, 1]),
                    &observer.user_id(),
                );
            }
        }
        servers.push(tokio::spawn(current.clone().run_accept_loop(listener)));
    }
    let channel = a
        .create_channel(
            "custody fixture",
            vec![sibling.identity.public(), bob.identity.public()],
        )
        .await
        .unwrap();
    if !external_online {
        servers[2].abort();
        a.roster.lock().unwrap().forget(&bob.identity.public());
    }
    a.send_channel_message(channel, b"synthetic custody fixture")
        .await
        .unwrap();
    let event = a.channel_history(channel, 10).unwrap()[0].id;
    if external_online {
        assert!(
            bob.channel_history(channel, 10)
                .unwrap()
                .iter()
                .any(|row| row.id == event)
        );
        let key = AccountDocumentKey::ChannelCustody {
            channel: hex::encode(channel.as_bytes()),
            event: event.to_hex(),
            holder: bob.user_id(),
        };
        let before = a.account_document_meta(&key).unwrap();
        a.account_documents
            .lock()
            .unwrap()
            .remove_blob_for_test(before.content_hash);
        let reopened =
            AccountDocumentStore::open(&dir.path().join("a"), "pw", &account.public().ed25519_pub)
                .unwrap();
        *a.account_documents.lock().unwrap() = reopened;
        assert_eq!(
            a.channel_custody_count(channel, event).unwrap(),
            None,
            "unreadable positive evidence is unknown, never zero confirmed copies"
        );
        let peer = a.routing_peer(&bob.user_id()).unwrap();
        a.recheck_channel_custody_peer(channel, event, &peer).await;
        assert_eq!(
            a.account_document_meta(&key).unwrap(),
            before,
            "identical positive evidence repairs its blob without version churn"
        );
        assert!(a.account_document(&key).unwrap().is_some());
        assert_eq!(a.channel_custody_count(channel, event).unwrap(), Some(1));
        servers[2].abort();
        a.roster.lock().unwrap().forget(&bob.identity.public());
        sibling
            .roster
            .lock()
            .unwrap()
            .forget(&bob.identity.public());
    }
    let worker = tokio::spawn(sibling.clone().run_account_history_loop());
    tokio::time::timeout(Duration::from_secs(8), async {
        while !sibling
            .channel_history(channel, 10)
            .unwrap()
            .iter()
            .any(|row| row.id == event && row.from_me)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("sibling imports the exact outgoing message");
    let expected = usize::from(external_online);
    let mut final_expected = expected;
    let actual = tokio::time::timeout(Duration::from_secs(8), async {
        while sibling.channel_custody_count(channel, event).unwrap() != Some(expected) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if !external_online && actual.is_ok() {
        sibling
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        for observer in [&a, &sibling] {
            observer.roster.lock().unwrap().update(
                &Announce::new_with_account(&bob.identity, &other, "Bob", port),
                IpAddr::from([127, 0, 0, 1]),
                &observer.user_id(),
            );
        }
        let bob_server = tokio::spawn(bob.clone().run_accept_loop(listener));
        let members = a
            .channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .members()
            .to_vec();
        a.distribute_channel_message(channel, &members, event).await;
        assert_eq!(a.channel_custody_count(channel, event).unwrap(), Some(1));
        assert_eq!(
            bob.channel_custody_count(channel, event).unwrap(),
            None,
            "incoming foreign history is never own custody"
        );
        let source_peer = sibling.routing_peer(&a.user_id()).unwrap();
        let mut cursor =
            sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
        sibling
            .pull_account_documents_page(&source_peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            sibling.channel_custody_count(channel, event).unwrap(),
            Some(0),
            "history-off must not import a late confirmation"
        );
        sibling
            .set_device_sync_category(SyncCategory::History, true)
            .unwrap();
        let mut cursor =
            sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
        sibling
            .pull_account_documents_page(&source_peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            sibling.channel_custody_count(channel, event).unwrap(),
            Some(1),
            "confirmation arriving after history must update independently"
        );
        let held = a.channel_custody_observations(channel, event).into_iter().find(|meta| matches!(&meta.key, AccountDocumentKey::ChannelCustody { holder, .. } if holder == &bob.user_id())).unwrap();
        let old_payload = a
            .account_document(&held.key)
            .unwrap()
            .unwrap()
            .payload()
            .to_vec();
        // Model complete durable ciphertext loss on the synthetic holder. A real
        // authenticated negative probe uses the exact runtime recheck operation.
        bob_server.abort();
        let secret = bob.identity.secret_bytes();
        let lost = node_with_identity(
            &dir.path().join("bob-lost"),
            &other,
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        );
        lost.roster.lock().unwrap().update(
            &Announce::new_with_account(&a.identity, &account, "A1", source_peer.addr.port()),
            source_peer.addr.ip(),
            &lost.user_id(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        a.roster.lock().unwrap().update(
            &Announce::new_with_account(&lost.identity, &other, "Bob", port),
            IpAddr::from([127, 0, 0, 1]),
            &a.user_id(),
        );
        let lost_server = tokio::spawn(lost.clone().run_accept_loop(listener));
        let bob_peer = a.routing_peer(&bob.user_id()).unwrap();
        a.recheck_channel_custody_peer(channel, event, &bob_peer)
            .await;
        assert_eq!(a.channel_custody_count(channel, event).unwrap(), Some(0));
        let mut cursor =
            sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
        sibling
            .pull_account_documents_page(&source_peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            sibling.channel_custody_count(channel, event).unwrap(),
            Some(0),
            "negative evidence must propagate after the earlier positive"
        );
        assert!(
            !sibling
                .account_documents
                .lock()
                .unwrap()
                .merge(held, &old_payload)
                .unwrap(),
            "stale positive replay cannot undo the shared tombstone"
        );
        assert_eq!(
            sibling.channel_custody_count(channel, event).unwrap(),
            Some(0)
        );
        a.channel_custody
            .lock()
            .unwrap()
            .confirm(event, &bob.user_id())
            .unwrap();
        a.migrate_channel_custody(&mut None);
        assert_eq!(
            a.channel_custody_count(channel, event).unwrap(),
            Some(0),
            "an old journal positive cannot resurrect the newer shared negative"
        );
        let secret = sibling.identity.secret_bytes();
        let negative_reopened = node_with_identity(
            &dir.path().join("sibling"),
            &account,
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        );
        assert_eq!(
            negative_reopened
                .channel_custody_count(channel, event)
                .unwrap(),
            Some(0),
            "the negative survives a fresh encrypted-store open"
        );
        drop(negative_reopened);
        a.retry_one_channel_custody(
            0,
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .await;
        assert!(
            lost.log.lock().unwrap().get(&event).unwrap().is_some(),
            "a shared negative must allow retry despite an old local journal positive"
        );
        // Apply the receiver's retained controls before asking it to qualify
        // the repaired copy. The first exchange bootstrapped its lost state.
        lost.process_channel(channel);
        a.retry_one_channel_custody(
            0,
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .await;
        assert_eq!(
            a.channel_custody_count(channel, event).unwrap(),
            Some(1),
            "a fresh authenticated probe repairs the negative; receiver={:?}",
            lost.list_channels().unwrap()
        );
        let mut cursor =
            sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
        sibling
            .pull_account_documents_page(&source_peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            sibling.channel_custody_count(channel, event).unwrap(),
            Some(1)
        );
        final_expected = 1;
        lost_server.abort();
    }
    worker.abort();
    for server in servers {
        server.abort();
    }
    let secret = sibling.identity.secret_bytes();
    let reopened = node_with_identity(
        &dir.path().join("sibling"),
        &account,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    assert_eq!(
        reopened.channel_custody_count(channel, event).unwrap(),
        Some(final_expected),
        "a fresh node must retain synchronized observations and tombstones"
    );
    assert_eq!(
        a.channel_custody_count(channel, event).unwrap(),
        Some(final_expected),
        "sender siblings do not count as another member account; sibling status={:?}",
        sibling.channel_custody_count(channel, event).unwrap()
    );
    assert!(
        actual.is_ok(),
        "authenticated external custody must converge on sibling: {:?}",
        sibling.channel_custody_count(channel, event).unwrap()
    );
}
#[tokio::test]
async fn certified_siblings_transfer_large_typed_document_over_existing_account_session() {
    use crate::{discovery::roster::PeerRecord, node::account_devices::AccountDevice};
    use std::{sync::Arc, time::Instant};
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
        a.set_device_sync_category(SyncCategory::Appearance, true)
            .unwrap();
    }
    let key = AccountDocumentKey::CustomizationPack {
        pack_id: "test-pack".into(),
    };
    let payload = vec![7; 1024 * 1024 + 31];
    source
        .put_account_document_if(key.clone(), payload.clone(), None, |commit| commit())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = Arc::clone(&source);
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let channel = serving.privacy_accept(stream).await.unwrap();
        serving.serve_connection(channel).await;
    });
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: address,
        name: "source".into(),
        account_id: Some(account.account_id()),
        last_seen: Instant::now(),
        post_office: false,
        routes: Vec::new(),
    };
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::CustomizationPack);
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    assert_eq!(
        target.account_document(&key).unwrap().unwrap().payload(),
        payload
    );
    source
        .set_device_sync_category(SyncCategory::Appearance, false)
        .unwrap();
    let mut declined =
        sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::CustomizationPack);
    target
        .pull_account_documents_page(&peer, &mut declined)
        .await
        .unwrap();
    let status = declined.status(target.device_sync_policy());
    assert_eq!(status.0, super::super::SyncPhase::Disabled);
    assert_eq!(status.1.as_deref(), Some("source_sync_off"));
    assert!(!declined.up_to_date);
    // Pooled session stays valid until runtime-owned shutdown; abort only fixture listener task.
    task.abort();
    let _ = task.await;
}
#[test]
fn signatures_conflicts_receipts_and_payload_bounds_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let other = Account::generate();
    let writer = DeviceIdentity::generate().public().user_id();
    let mut store =
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
    let meta = store
        .put(&account, writer.clone(), key(), b"draft", None, false)
        .unwrap();
    for change in 0..7 {
        let mut bad = meta.clone();
        match change {
            0 => bad.signature[0] ^= 1,
            1 => bad.account_public = other.public().ed25519_pub,
            2 => {
                bad.key = AccountDocumentKey::CustomizationPack {
                    pack_id: "other".into(),
                }
            }
            3 => bad.revision += 1,
            4 => bad.content_hash[0] ^= 1,
            5 => bad.content_len += 1,
            _ => bad.tombstone = true,
        };
        assert!(store.merge(bad, b"draft").is_err());
    }
    let old = meta.clone();
    let next = store
        .put(
            &account,
            writer.clone(),
            key(),
            b"new",
            Some(&meta.version()),
            false,
        )
        .unwrap();
    assert!(!store.merge(old, b"draft").unwrap());
    assert_eq!(store.meta(&key()).unwrap(), next);
    assert!(
        store
            .put(
                &account,
                writer.clone(),
                AccountDocumentKey::Draft {
                    conversation: DocumentConversation::Account("b".repeat(32))
                },
                &vec![0; 65537],
                None,
                false
            )
            .is_err()
    );
    let seen = AccountDocumentKey::ReadReceipt {
        conversation: DocumentConversation::Channel("c".repeat(64)),
        message_id: "exact-message-before-history".into(),
    };
    let receipt = store
        .put(&account, writer.clone(), seen.clone(), &[], None, false)
        .unwrap();
    assert!(
        store
            .put(
                &account,
                writer,
                seen.clone(),
                &[],
                Some(&receipt.version()),
                true
            )
            .is_err()
    );
    drop(store);
    let store =
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
    assert!(store.get(&seen).unwrap().is_some());
}
#[test]
fn missing_blob_is_not_converged_and_equal_signed_record_repairs_it() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let writer = DeviceIdentity::generate().public().user_id();
    let mut store =
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
    let meta = store
        .put(&account, writer, key(), b"draft", None, false)
        .unwrap();
    let before = store.digest(AccountDocumentDomain::Draft);
    store.remove_blob_for_test(meta.content_hash);
    drop(store);
    let mut store =
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
    assert!(store.get(&key()).is_err());
    assert_eq!(store.digest(AccountDocumentDomain::Draft), [0; 32]);
    assert!(store.merge(meta, b"draft").unwrap());
    assert_eq!(store.digest(AccountDocumentDomain::Draft), before);
    assert_eq!(store.get(&key()).unwrap().unwrap().payload(), b"draft");
}
#[test]
fn failed_index_commit_keeps_previous_visible_document_and_reclaims_orphan() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let writer = DeviceIdentity::generate().public().user_id();
    let mut store =
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
    let first = store
        .put(&account, writer.clone(), key(), b"first", None, false)
        .unwrap();
    store.fail_index_for_test(true);
    assert!(
        store
            .put(
                &account,
                writer,
                key(),
                b"second",
                Some(&first.version()),
                false
            )
            .is_err()
    );
    assert_eq!(store.get(&key()).unwrap().unwrap().payload(), b"first");
    store.fail_index_for_test(false);
    drop(store);
    assert_eq!(
        AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub)
            .unwrap()
            .get(&key())
            .unwrap()
            .unwrap()
            .payload(),
        b"first"
    );
}
#[test]
fn document_extension_preserves_old_tags_and_rejects_legacy_codec() {
    use super::super::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion};
    let request = wire::DocumentRequest {
        version: 1,
        account_id: "a".repeat(32),
        domain: AccountDocumentDomain::Draft,
        after: None,
        known_digest: [0; 32],
    };
    let frame = AccountHistoryFrame::DocumentsRequest {
        body: request.encode(),
    };
    assert!(frame.encode_version(AccountSyncVersion::V1).is_none());
    let bytes = frame.encode_version(AccountSyncVersion::V2).unwrap();
    assert_eq!(&bytes[6..10], &21u32.to_le_bytes());
    assert!(AccountHistoryFrame::decode_versioned(&bytes).is_some());
    assert!(
        AccountHistoryFrame::DocumentChunk {
            bytes: vec![0; 16385]
        }
        .encode_version(AccountSyncVersion::V2)
        .is_none()
    );
    assert!(wire::DocumentRequest::decode(&vec![0; 8193]).is_none());
    let mut invalid = request;
    invalid.after = Some(AccountDocumentKey::ReadReceipt {
        conversation: DocumentConversation::Account("a".repeat(32)),
        message_id: "x".into(),
    });
    assert!(wire::DocumentRequest::decode(&invalid.encode()).is_none());
}
#[tokio::test]
async fn unrelated_noise_peer_cannot_fetch_account_documents() {
    use crate::{
        node::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion},
        transport::SecureChannel,
    };
    use tokio::net::{TcpListener, TcpStream};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    source
        .put_account_document_if(key(), b"private draft".to_vec(), None, |commit| commit())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let channel = serving.privacy_accept(stream).await.unwrap();
        serving.serve_connection(channel).await;
    });
    let stranger = DeviceIdentity::generate();
    let mut channel = SecureChannel::connect(
        TcpStream::connect(address).await.unwrap(),
        &stranger,
        Some(&source.identity.public()),
    )
    .await
    .unwrap();
    let request = wire::DocumentRequest {
        version: 1,
        account_id: account.account_id(),
        domain: AccountDocumentDomain::Draft,
        after: None,
        known_digest: [0; 32],
    };
    channel
        .send(
            &AccountHistoryFrame::DocumentsRequest {
                body: request.encode(),
            }
            .encode_version(AccountSyncVersion::V2)
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), channel.recv())
            .await
            .unwrap()
            .is_err()
    );
    task.await.unwrap();
}
#[tokio::test]
async fn interrupted_document_does_not_replace_previous_visible_version() {
    use crate::{
        discovery::roster::PeerRecord,
        node::{
            account_devices::AccountDevice,
            account_sync_wire::{AccountHistoryFrame, AccountSyncVersion},
        },
    };
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let old = source
        .put_account_document_if(key(), b"old".to_vec(), None, |commit| commit())
        .unwrap();
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(old.clone(), b"old")
        .unwrap();
    let next = source
        .put_account_document_if(
            key(),
            b"new incomplete draft".to_vec(),
            Some(old.version()),
            |commit| commit(),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = source.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = server.privacy_accept(stream).await.unwrap();
        let bytes = channel.recv().await.unwrap();
        let Some((_, AccountHistoryFrame::SessionRequest { request_id, .. })) =
            AccountHistoryFrame::decode_versioned(&bytes)
        else {
            panic!("expected pooled request")
        };
        for frame in [
            AccountHistoryFrame::SessionReady { request_id },
            AccountHistoryFrame::DocumentHeader {
                body: serde_json::to_vec(&next).unwrap(),
            },
            AccountHistoryFrame::DocumentChunk {
                bytes: b"new incomplete".to_vec(),
            },
        ] {
            channel
                .send(&frame.encode_version(AccountSyncVersion::V2).unwrap())
                .await
                .unwrap();
        }
    });
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: address,
        name: "source".into(),
        post_office: false,
        routes: Vec::new(),
        account_id: Some(account.account_id()),
        last_seen: std::time::Instant::now(),
    };
    let mut cursor = sync::DocumentSyncCursor::default();
    assert!(
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .is_err()
    );
    assert_eq!(
        target.account_document(&key()).unwrap().unwrap().payload(),
        b"old"
    );
    assert!(!cursor.up_to_date);
    task.await.unwrap();
}
#[tokio::test]
async fn existing_bounded_account_worker_converges_documents_without_a_new_daemon() {
    use crate::{discovery::Announce, node::account_devices::AccountDevice};
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    source
        .put_account_document_if(
            key(),
            b"autonomous private draft".to_vec(),
            None,
            |commit| commit(),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    target.roster.lock().unwrap().update(
        &Announce::new_with_account(&source.identity, &account, "source", address.port()),
        address.ip(),
        &target.user_id(),
    );
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    let worker = tokio::spawn(target.clone().run_account_history_loop());
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            if target.account_document(&key()).unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("existing worker imports document");
    assert_eq!(
        target.account_document(&key()).unwrap().unwrap().payload(),
        b"autonomous private draft"
    );
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn old_peer_document_probe_has_negative_cooldown_and_history_still_works() {
    use crate::{
        discovery::roster::PeerRecord,
        node::{
            account_devices::AccountDevice,
            account_sync_wire::{AccountHistoryFrame, AccountSyncVersion},
        },
    };
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = source.clone();
    let task = tokio::spawn(async move {
        let mut probes = 0;
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = server.privacy_accept(stream).await.unwrap();
            let bytes = channel.recv().await.unwrap();
            let (_, frame) = AccountHistoryFrame::decode_versioned(&bytes).unwrap();
            match frame {
                AccountHistoryFrame::SessionRequest { .. }
                | AccountHistoryFrame::DocumentsRequest { .. } => {
                    probes += 1;
                }
                AccountHistoryFrame::Request {
                    account_id,
                    cursor,
                    known_digest,
                    wanted,
                    ..
                } => {
                    assert_eq!(probes, 2);
                    server
                        .serve_account_history(
                            &mut channel,
                            AccountSyncVersion::V2,
                            &account_id,
                            cursor,
                            known_digest,
                            wanted,
                        )
                        .await;
                    return;
                }
                _ => panic!("unexpected old-client frame"),
            }
        }
    });
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: address,
        name: "source".into(),
        post_office: false,
        routes: Vec::new(),
        account_id: Some(account.account_id()),
        last_seen: std::time::Instant::now(),
    };
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::Draft);
    assert!(
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .is_err()
    );
    assert!(
        cursor
            .retry_at
            .unwrap()
            .duration_since(std::time::Instant::now())
            .as_secs()
            > 250
    );
    assert!(cursor.error.as_ref().unwrap().contains("upgrade"));
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    let mut exchange = target
        .begin_account_exchange(
            &peer,
            AccountSyncVersion::V2,
            AccountHistoryFrame::Request {
                version: 2,
                account_id: account.account_id(),
                cursor: None,
                known_digest: [0; 32],
                wanted: target.device_sync_policy(),
            },
        )
        .await
        .unwrap();
    let mut completed = false;
    for _ in 0..10 {
        let bytes = exchange.recv().await.unwrap();
        if matches!(
            AccountHistoryFrame::decode_versioned(&bytes),
            Some((_, AccountHistoryFrame::Complete { .. }))
        ) {
            completed = true;
            break;
        }
    }
    assert!(completed);
    task.await.unwrap();
}
#[test]
fn absent_delete_and_concurrent_create_converge_without_stale_resurrection() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let mut left =
        AccountDocumentStore::open(a.path(), "pw", &account.public().ed25519_pub).unwrap();
    let mut right =
        AccountDocumentStore::open(b.path(), "pw", &account.public().ed25519_pub).unwrap();
    let created = left
        .put(&account, "1".repeat(32), key(), b"concurrent", None, false)
        .unwrap();
    let removed = right
        .put(&account, "f".repeat(32), key(), &[], None, true)
        .unwrap();
    left.merge(removed.clone(), &[]).unwrap();
    right.merge(created.clone(), b"concurrent").unwrap();
    assert_eq!(left.meta(&key()), right.meta(&key()));
    assert!(
        left.put(
            &account,
            "1".repeat(32),
            key(),
            b"stale",
            Some(&created.version()),
            false
        )
        .is_err()
    );
    assert!(left.get(&key()).unwrap().is_none());
    assert_eq!(
        left.digest(AccountDocumentDomain::Draft),
        right.digest(AccountDocumentDomain::Draft)
    );
}
#[tokio::test]
async fn policy_off_on_while_payload_is_inflight_denies_document_commit() {
    use crate::{
        discovery::roster::PeerRecord,
        node::{
            account_devices::AccountDevice,
            account_sync_wire::{AccountHistoryFrame, AccountSyncVersion},
        },
    };
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let meta = source
        .put_account_document_if(key(), b"inflight".to_vec(), None, |commit| commit())
        .unwrap();
    let digest = source
        .account_documents
        .lock()
        .unwrap()
        .digest(AccountDocumentDomain::Draft);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = source.clone();
    let (sent, received) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = server.privacy_accept(stream).await.unwrap();
        let bytes = channel.recv().await.unwrap();
        let Some((_, AccountHistoryFrame::SessionRequest { request_id, .. })) =
            AccountHistoryFrame::decode_versioned(&bytes)
        else {
            panic!("expected session request")
        };
        for frame in [
            AccountHistoryFrame::SessionReady { request_id },
            AccountHistoryFrame::DocumentHeader {
                body: serde_json::to_vec(&meta).unwrap(),
            },
        ] {
            channel
                .send(&frame.encode_version(AccountSyncVersion::V2).unwrap())
                .await
                .unwrap();
        }
        sent.send(()).unwrap();
        released.await.unwrap();
        for frame in [
            AccountHistoryFrame::DocumentChunk {
                bytes: b"inflight".to_vec(),
            },
            AccountHistoryFrame::DocumentsComplete {
                body: serde_json::to_vec(&wire::DocumentComplete {
                    enabled: true,
                    domain: AccountDocumentDomain::Draft,
                    next: None,
                    digest,
                })
                .unwrap(),
            },
        ] {
            let _ = channel
                .send(&frame.encode_version(AccountSyncVersion::V2).unwrap())
                .await;
        }
    });
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: address,
        name: "source".into(),
        post_office: false,
        routes: Vec::new(),
        account_id: Some(account.account_id()),
        last_seen: std::time::Instant::now(),
    };
    let pulling = target.clone();
    let client = tokio::spawn(async move {
        pulling
            .pull_account_documents_page(&peer, &mut sync::DocumentSyncCursor::default())
            .await
    });
    received.await.unwrap();
    target
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    target
        .set_device_sync_category(SyncCategory::History, true)
        .unwrap();
    release.send(()).unwrap();
    assert!(client.await.unwrap().is_err());
    assert!(target.account_document(&key()).unwrap().is_none());
    task.await.unwrap();
}
#[test]
fn equal_revision_and_writer_use_content_hash_as_the_only_value_tie_break() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let mut left =
        AccountDocumentStore::open(a.path(), "pw", &account.public().ed25519_pub).unwrap();
    let mut right =
        AccountDocumentStore::open(b.path(), "pw", &account.public().ed25519_pub).unwrap();
    let first = AccountDocumentMeta::new(&account, "a".repeat(32), key(), 77, b"one", false);
    let second = AccountDocumentMeta::new(&account, "a".repeat(32), key(), 77, b"two", false);
    left.merge(first.clone(), b"one").unwrap();
    left.merge(second.clone(), b"two").unwrap();
    right.merge(second.clone(), b"two").unwrap();
    right.merge(first.clone(), b"one").unwrap();
    let winner = if first.content_hash > second.content_hash {
        first
    } else {
        second
    };
    assert_eq!(left.meta(&key()).unwrap().version(), winner.version());
    assert_eq!(left.meta(&key()), right.meta(&key()));
    assert_eq!(
        left.digest(AccountDocumentDomain::Draft),
        right.digest(AccountDocumentDomain::Draft)
    );
}

#[tokio::test]
async fn private_dm_reaction_catches_up_through_a_third_certified_device() {
    use crate::{discovery::roster::PeerRecord, node::account_devices::AccountDevice};
    use std::{sync::Arc, time::Instant};
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("reaction-source"), &account);
    let relay = node(&dir.path().join("reaction-relay"), &account);
    let fresh = node(&dir.path().join("reaction-fresh"), &account);
    for a in [&source, &relay, &fresh] {
        for b in [&source, &relay, &fresh] {
            a.account_devices
                .lock()
                .unwrap()
                .insert(AccountDevice {
                    public: b.identity.public(),
                    certificate: account.certify(&b.identity.public().ed25519_pub),
                })
                .unwrap();
        }
    }
    let peer_account = Account::generate().account_id();
    let target = crate::eventlog::EventId::new([42; 32]);
    source
        .react_to_account(&peer_account, target, "👍", false)
        .await
        .unwrap();
    let expected = source.account_reactions(&peer_account).unwrap();
    assert_eq!(expected.len(), 1);
    for (from, to) in [(&source, &relay), (&relay, &fresh)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = Arc::clone(from);
        let task = tokio::spawn(async move { Box::pin(serving.run_accept_loop(listener)).await });
        let peer = PeerRecord {
            public: from.identity.public(),
            addr: address,
            name: "source".into(),
            account_id: Some(account.account_id()),
            last_seen: Instant::now(),
            post_office: false,
            routes: Vec::new(),
        };
        let mut cursor = sync::DocumentSyncCursor::default();
        for _ in 0..8 {
            to.pull_account_documents_page(&peer, &mut cursor)
                .await
                .unwrap();
        }
        task.abort();
        let _ = task.await;
    }
    assert_eq!(
        fresh.account_reactions(&peer_account).unwrap(),
        expected,
        "a newly linked device must recover old reactions from any certified sibling"
    );
    assert!(
        fresh.account_history_revision() > 0,
        "a committed remote control must invalidate already-open history and reaction views"
    );
    let revision = fresh.account_history_revision();
    pull_control_documents(&source, &fresh, &account).await;
    assert_eq!(
        fresh.account_history_revision(),
        revision,
        "duplicate witnessed controls must not trigger reload loops"
    );
}

async fn pull_control_documents(
    from: &std::sync::Arc<Node>,
    to: &std::sync::Arc<Node>,
    account: &Account,
) {
    use crate::{discovery::roster::PeerRecord, node::account_devices::AccountDevice};
    use std::{sync::Arc, time::Instant};
    use tokio::net::TcpListener;
    for (a, b) in [(from, to), (to, from)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = Arc::clone(from);
    let task = tokio::spawn(async move { Box::pin(serving.run_accept_loop(listener)).await });
    let peer = PeerRecord {
        public: from.identity.public(),
        addr: address,
        name: "source".into(),
        account_id: Some(account.account_id()),
        last_seen: Instant::now(),
        post_office: false,
        routes: Vec::new(),
    };
    let mut cursor = sync::DocumentSyncCursor::default();
    for _ in 0..8 {
        to.pull_account_documents_page(&peer, &mut cursor)
            .await
            .unwrap();
    }
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn private_dm_recall_catches_up_before_a_late_original_message() {
    use crate::node::{
        BackfillRecord, DmEnvelope, MessageBody, conversation::account_conversation_id,
        node::now_millis,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("recall-source"), &account);
    let fresh = node(&dir.path().join("recall-fresh"), &account);
    let peer = Account::generate().account_id();
    let target = [43; 32];
    let wall = now_millis();
    let conv = account_conversation_id(&account.account_id(), &peer);
    let envelope = DmEnvelope::new(
        account.account_id(),
        peer.clone(),
        target,
        MessageBody::new(b"recalled private body".to_vec(), None).encode(),
    )
    .encode();
    source
        .sentlog
        .lock()
        .unwrap()
        .record(conv, 1, wall, &envelope)
        .unwrap();
    source
        .recall_account(&peer, crate::eventlog::EventId::new(target))
        .await
        .unwrap();
    pull_control_documents(&source, &fresh, &account).await;
    fresh
        .import_account_backfill(&[BackfillRecord {
            conv: *conv.as_bytes(),
            from: account.account_id(),
            wall_clock: wall,
            event_id: target,
            plaintext: envelope,
        }])
        .unwrap();
    assert!(
        fresh
            .account_history(&peer, 10)
            .unwrap()
            .iter()
            .all(|m| m.id != crate::eventlog::EventId::new(target) || m.recalled),
        "catch-up must never show the late recalled plaintext"
    );
}

#[tokio::test]
async fn private_peer_reaction_witness_reaches_an_offline_linked_device() {
    use crate::{
        discovery::Announce,
        eventlog::{Event, EventId, EventKind},
        node::{ReactionEnvelope, conversation::dm_conversation_id, test_support::node_with_files},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let actor_account = Account::generate();
    let actor = DeviceIdentity::generate();
    let observer_identity = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&actor, &actor_account, "peer", 9);
    let path = dir.path().join("observer");
    std::fs::create_dir_all(&path).unwrap();
    let (observer, _, _) = node_with_files(
        &path,
        observer_identity,
        Account::from_secret_bytes(account.secret_bytes()),
        &proof,
    );
    let fresh = node(&dir.path().join("offline-fresh"), &account);
    let conv = dm_conversation_id(&observer.identity.public(), &actor.public());
    let target = EventId::new([44; 32]);
    let env = ReactionEnvelope::new(
        actor_account.account_id(),
        account.account_id(),
        *target.as_bytes(),
        "👍".into(),
        false,
    );
    let event = Event::new(
        &actor,
        conv,
        1,
        vec![],
        1,
        10,
        EventKind::React,
        crate::dm::seal(
            &actor,
            &observer.identity.public().x25519_pub,
            &env.encode(),
        )
        .unwrap(),
    );
    observer.log.lock().unwrap().append(event).unwrap();
    observer
        .account_documents
        .lock()
        .unwrap()
        .fail_index_for_test(true);
    assert!(observer.project_private_dm_controls(conv).is_err());
    assert!(
        observer
            .account_documents(AccountDocumentDomain::DmReaction, None, 64)
            .unwrap()
            .items
            .is_empty()
    );
    observer
        .account_documents
        .lock()
        .unwrap()
        .fail_index_for_test(false);
    observer.emit_new_messages(conv);
    pull_control_documents(&observer, &fresh, &account).await;
    let got = fresh
        .account_reactions(&actor_account.account_id())
        .unwrap();
    assert_eq!(
        got.len(),
        1,
        "verified peer controls must reach a different device key through private account catch-up"
    );
    assert_eq!(got[0].who, vec![actor_account.account_id()]);
}

#[test]
fn private_control_original_order_removal_and_generic_write_guards() {
    use crate::{eventlog::EventId, node::reaction::ReactionPayload};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(dir.path(), &account);
    let peer = Account::generate().account_id();
    let target = EventId::new([45; 32]);
    let action = |remove| ReactionPayload {
        target,
        emoji: "👍".into(),
        remove,
    };
    source
        .publish_own_account_reaction(&peer, 20, &action(true))
        .unwrap();
    source
        .publish_own_account_reaction(&peer, 10, &action(false))
        .unwrap();
    source
        .publish_own_account_reaction(&peer, 20, &action(false))
        .unwrap();
    assert!(
        source.account_reactions(&peer).unwrap().is_empty(),
        "late/tied adds cannot resurrect a removal"
    );
    let key = AccountDocumentKey::DmReaction {
        peer_account: peer.clone(),
        target: target.to_hex(),
        actor_account: account.account_id(),
        emoji: "👍".into(),
    };
    let version = source.account_document_meta(&key).unwrap().version();
    let history_revision = source.account_history_revision();
    assert!(
        source
            .put_account_document_if(key.clone(), vec![], Some(version.clone()), |c| c())
            .is_err()
    );
    assert!(
        source
            .remove_account_document_if(key, Some(version), |c| c())
            .is_err()
    );
    let key = AccountDocumentKey::DmRecall {
        peer_account: peer,
        target: target.to_hex(),
        actor_account: account.account_id(),
    };
    assert!(
        source
            .put_account_document_if(key.clone(), vec![], None, |c| c())
            .is_err()
    );
    assert!(
        source
            .remove_account_document_if(key, None, |c| c())
            .is_err()
    );
    assert_eq!(
        source.account_history_revision(),
        history_revision,
        "rejected generic controls must not invalidate history"
    );
}

#[tokio::test]
async fn accepted_own_recall_retries_a_failed_document_commit_after_the_window() {
    use crate::node::{
        DmEnvelope, MessageBody, conversation::account_conversation_id, node::now_millis,
        recall::RECALL_WINDOW_MS,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(dir.path(), &account);
    let peer = Account::generate().account_id();
    let target = crate::eventlog::EventId::new([46; 32]);
    let wall = now_millis() - RECALL_WINDOW_MS - 1;
    let conv = account_conversation_id(&account.account_id(), &peer);
    let envelope = DmEnvelope::new(
        account.account_id(),
        peer.clone(),
        *target.as_bytes(),
        MessageBody::new(b"previously accepted recall".to_vec(), None).encode(),
    )
    .encode();
    source
        .sentlog
        .lock()
        .unwrap()
        .record(conv, 1, wall, &envelope)
        .unwrap();
    assert!(
        source.recall_account(&peer, target).await.is_err(),
        "an unaccepted late recall stays forbidden"
    );
    source
        .recalls
        .lock()
        .unwrap()
        .record(conv, target, wall)
        .unwrap();
    source
        .account_documents
        .lock()
        .unwrap()
        .fail_index_for_test(true);
    assert!(source.recall_account(&peer, target).await.is_err());
    source
        .account_documents
        .lock()
        .unwrap()
        .fail_index_for_test(false);
    source.recall_account(&peer, target).await.expect(
        "previously accepted durable recall must repair its private document even after the window",
    );
    assert_eq!(
        source.private_dm_recall_claims(&peer),
        vec![(target, account.account_id())]
    );
}

#[tokio::test]
async fn private_controls_obey_history_policy_survive_restart_and_reject_another_root() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("policy-source"), &account);
    let path = dir.path().join("policy-fresh");
    let fresh = node(&path, &account);
    let peer = Account::generate().account_id();
    let target = crate::eventlog::EventId::new([47; 32]);
    source
        .react_to_account(&peer, target, "👍", false)
        .await
        .unwrap();
    source
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    pull_control_documents(&source, &fresh, &account).await;
    assert!(fresh.account_reactions(&peer).unwrap().is_empty());
    source
        .set_device_sync_category(SyncCategory::History, true)
        .unwrap();
    pull_control_documents(&source, &fresh, &account).await;
    let expected = source.account_reactions(&peer).unwrap();
    assert_eq!(fresh.account_reactions(&peer).unwrap(), expected);
    let key = AccountDocumentKey::DmReaction {
        peer_account: peer.clone(),
        target: target.to_hex(),
        actor_account: account.account_id(),
        emoji: "👍".into(),
    };
    let doc = source.account_document(&key).unwrap().unwrap();
    let foreign = node(&dir.path().join("foreign-root"), &Account::generate());
    assert!(
        foreign
            .account_documents
            .lock()
            .unwrap()
            .merge(doc.meta.clone(), doc.payload())
            .is_err()
    );
    let identity = fresh.identity.secret_bytes();
    drop(fresh);
    let reopened = node_with_identity(
        &path,
        &account,
        DeviceIdentity::from_secret_bytes(identity.0, identity.1),
    );
    assert_eq!(reopened.account_reactions(&peer).unwrap(), expected);
    source
        .react_to_account(&peer, target, "👍", true)
        .await
        .unwrap();
    pull_control_documents(&source, &reopened, &account).await;
    assert!(reopened.account_reactions(&peer).unwrap().is_empty());
    assert!(
        !reopened
            .account_documents
            .lock()
            .unwrap()
            .merge(doc.meta.clone(), doc.payload())
            .unwrap(),
        "an old witnessed add cannot overtake the later removal"
    );
    assert!(reopened.account_reactions(&peer).unwrap().is_empty());
}

#[test]
fn private_legacy_control_import_is_explicit_and_bound_to_the_kept_root() {
    use crate::node::{
        DmEnvelope, MessageBody, conversation::account_conversation_id, reaction::ReactionPayload,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(dir.path(), &account);
    let peer_account = Account::generate();
    let peer = peer_account.account_id();
    let target = crate::eventlog::EventId::new([48; 32]);
    let conv = account_conversation_id(&account.account_id(), &peer);
    let env = DmEnvelope::new(
        account.account_id(),
        peer.clone(),
        *target.as_bytes(),
        MessageBody::new(b"kept source".to_vec(), None).encode(),
    )
    .encode();
    source
        .sentlog
        .lock()
        .unwrap()
        .record(conv, 1, 10, &env)
        .unwrap();
    source
        .my_dm_reactions
        .lock()
        .unwrap()
        .record(
            conv,
            11,
            ReactionPayload {
                target,
                emoji: "👍".into(),
                remove: false,
            },
        )
        .unwrap();
    source
        .recalls
        .lock()
        .unwrap()
        .record(conv, target, 10)
        .unwrap();
    assert!(
        source
            .account_documents(AccountDocumentDomain::DmReaction, None, 64)
            .unwrap()
            .items
            .is_empty()
    );
    source.publish_legacy_account_controls().unwrap();
    assert_eq!(
        source
            .account_documents(AccountDocumentDomain::DmReaction, None, 64)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        source.private_dm_recall_claims(&peer),
        vec![(target, account.account_id())]
    );
    drop(source);
    let adopted = node(&dir.path().join("adopted"), &peer_account);
    // This account was the old sender's peer: the conversation hash is symmetric.
    // Keep the old unscoped cache and old send route without relabelling either.
    adopted
        .sentlog
        .lock()
        .unwrap()
        .record(conv, 1, 10, &env)
        .unwrap();
    adopted
        .recalls
        .lock()
        .unwrap()
        .record(conv, target, 10)
        .unwrap();
    assert!(
        adopted
            .account_documents(AccountDocumentDomain::DmReaction, None, 64)
            .unwrap()
            .items
            .is_empty()
    );
    adopted.publish_legacy_account_controls().unwrap();
    assert!(
        adopted
            .account_documents(AccountDocumentDomain::DmRecall, None, 64)
            .unwrap()
            .items
            .is_empty(),
        "old unscoped RecallLog cannot acquire another root through a mismatched retained route"
    );
}

#[test]
fn private_peer_control_ingress_rejects_wrong_actor_and_unrelated_route() {
    use crate::{
        discovery::Announce,
        eventlog::{Event, EventKind},
        node::{ReactionEnvelope, conversation::dm_conversation_id, test_support::node_with_files},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let actor_account = Account::generate();
    let actor = DeviceIdentity::generate();
    let identity = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&actor, &actor_account, "peer", 9);
    let (observer, _, _) = node_with_files(
        dir.path(),
        identity,
        Account::from_secret_bytes(account.secret_bytes()),
        &proof,
    );
    let conv = dm_conversation_id(&observer.identity.public(), &actor.public());
    let target = [49; 32];
    for (index, (sender, recipient)) in [
        (Account::generate().account_id(), account.account_id()),
        (actor_account.account_id(), Account::generate().account_id()),
    ]
    .into_iter()
    .enumerate()
    {
        let env = ReactionEnvelope::new(sender, recipient, target, "👍".into(), false);
        let event = Event::new(
            &actor,
            conv,
            index as u64 + 1,
            vec![],
            index as u64 + 1,
            10,
            EventKind::React,
            crate::dm::seal(
                &actor,
                &observer.identity.public().x25519_pub,
                &env.encode(),
            )
            .unwrap(),
        );
        observer.log.lock().unwrap().append(event).unwrap();
    }
    observer.emit_new_messages(conv);
    assert!(
        observer
            .account_documents(AccountDocumentDomain::DmReaction, None, 64)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        observer
            .account_reactions(&actor_account.account_id())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn private_control_request_versions_keep_legacy_domains_unchanged() {
    use wire::DocumentRequest;
    for (domain, version, accepted) in [
        (AccountDocumentDomain::Draft, 1, true),
        (AccountDocumentDomain::AvatarOverride, 1, true),
        (AccountDocumentDomain::DmReaction, 1, false),
        (AccountDocumentDomain::DmReaction, 2, true),
        (AccountDocumentDomain::DmRecall, 2, true),
        (AccountDocumentDomain::ChannelCustody, 1, false),
        (AccountDocumentDomain::ChannelCustody, 2, false),
        (AccountDocumentDomain::ChannelCustody, 3, true),
        (AccountDocumentDomain::GroupInventory, 1, false),
        (AccountDocumentDomain::GroupInventory, 2, false),
        (AccountDocumentDomain::GroupInventory, 3, false),
        (AccountDocumentDomain::GroupInventory, 4, true),
        (AccountDocumentDomain::Draft, 4, false),
    ] {
        let request = DocumentRequest {
            version,
            domain,
            account_id: "a".repeat(32),
            after: None,
            known_digest: [0; 32],
        };
        assert_eq!(
            DocumentRequest::decode(&request.encode()).is_some(),
            accepted
        );
    }
}

#[tokio::test]
async fn unsupported_control_domains_do_not_pause_legacy_draft_updates() {
    use crate::{
        discovery::roster::PeerRecord,
        node::{
            account_devices::AccountDevice,
            account_sync_wire::{AccountHistoryFrame, AccountSessionRequest, AccountSyncVersion},
        },
    };
    use std::time::Instant;
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("old-source"), &account);
    let target = node(&dir.path().join("new-target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let old = source
        .put_account_document_if(key(), b"before probe".to_vec(), None, |c| c())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let server = serving.clone();
            tokio::spawn(async move {
                let mut channel = server.privacy_accept(stream).await.unwrap();
                while let Ok(bytes) = channel.recv().await {
                    let Some((
                        _,
                        AccountHistoryFrame::SessionRequest {
                            request_id,
                            request: AccountSessionRequest::Documents { body },
                        },
                    )) = AccountHistoryFrame::decode_versioned(&bytes)
                    else {
                        break;
                    };
                    let request: wire::DocumentRequest = serde_json::from_slice(&body).unwrap();
                    if request.version != 1 {
                        break;
                    }
                    if channel
                        .send(
                            &AccountHistoryFrame::SessionReady { request_id }
                                .encode_version(AccountSyncVersion::V2)
                                .unwrap(),
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if !server.serve_account_documents(&mut channel, request).await {
                        break;
                    }
                }
            });
        }
    });
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: address,
        name: "old source".into(),
        account_id: Some(account.account_id()),
        last_seen: Instant::now(),
        post_office: false,
        routes: vec![],
    };
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::DmReaction);
    assert!(
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .is_err()
    );
    assert!(
        cursor.requires_upgrade,
        "the optional new-domain close needs an upgrade hint"
    );
    source
        .put_account_document_if(key(), b"after probe".to_vec(), Some(old.version()), |c| c())
        .unwrap();
    for _ in 0..2 {
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .unwrap();
    }
    assert_eq!(
        target.account_document(&key()).unwrap().unwrap().payload(),
        b"after probe",
        "legacy domains must keep converging during optional control cooldown"
    );
    assert!(cursor.requires_upgrade);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn unsupported_channel_custody_cooldown_does_not_hide_legacy_document_failure() {
    use crate::{discovery::roster::PeerRecord, node::account_devices::AccountDevice};
    use std::time::Instant;
    use tokio::net::TcpListener;
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("legacy"), &account);
    let target = node(&directory.path().join("target"), &account);
    for (owner, sibling) in [(&source, &target), (&target, &source)] {
        owner
            .account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: sibling.identity.public(),
                certificate: account.certify(&sibling.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let owner = serving.clone();
            tokio::spawn(async move {
                let mut connection = owner.privacy_accept(stream).await.unwrap();
                // A history-only legacy client closes every document request,
                // including capability negotiation and the legacy fallback.
                let _ = connection.recv().await;
            });
        }
    });
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: address,
        name: "history-only legacy sibling".into(),
        account_id: Some(account.account_id()),
        last_seen: Instant::now(),
        post_office: false,
        routes: vec![],
    };
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
    assert!(
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .is_err()
    );
    assert!(
        cursor.pending,
        "optional rejection must still try older domains"
    );
    assert_eq!(cursor.domain, AccountDocumentDomain::Draft as usize);
    assert!(
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .is_err()
    );
    assert!(
        !cursor.pending,
        "an unrelated optional cooldown must not keep failed legacy work pending"
    );
    assert!(cursor.status(target.device_sync_policy()).2);
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    assert!(
        !cursor.pending,
        "the retry cooldown must allow history to settle"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn unsupported_channel_custody_does_not_pause_v2_controls_or_legacy_drafts() {
    use crate::{
        discovery::roster::PeerRecord,
        node::{
            account_devices::AccountDevice,
            account_sync_wire::{AccountHistoryFrame, AccountSessionRequest, AccountSyncVersion},
        },
    };
    use std::time::Instant;
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("old-source"), &account);
    let target = node(&dir.path().join("new-target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let old = source
        .put_account_document_if(key(), b"before probe".to_vec(), None, |c| c())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let server = serving.clone();
            tokio::spawn(async move {
                let mut channel = server.privacy_accept(stream).await.unwrap();
                while let Ok(bytes) = channel.recv().await {
                    let Some((
                        _,
                        AccountHistoryFrame::SessionRequest {
                            request_id,
                            request: AccountSessionRequest::Documents { body },
                        },
                    )) = AccountHistoryFrame::decode_versioned(&bytes)
                    else {
                        break;
                    };
                    let request: wire::DocumentRequest = serde_json::from_slice(&body).unwrap();
                    if request.version > 2 {
                        break;
                    }
                    if channel
                        .send(
                            &AccountHistoryFrame::SessionReady { request_id }
                                .encode_version(AccountSyncVersion::V2)
                                .unwrap(),
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if !server.serve_account_documents(&mut channel, request).await {
                        break;
                    }
                }
            });
        }
    });
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: address,
        name: "old source".into(),
        account_id: Some(account.account_id()),
        last_seen: Instant::now(),
        post_office: false,
        routes: vec![],
    };
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
    assert!(
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .is_err()
    );
    assert!(
        cursor.status(target.device_sync_policy()).2,
        "unsupported custody must retain its history upgrade hint"
    );
    source
        .put_account_document_if(key(), b"after probe".to_vec(), Some(old.version()), |c| c())
        .unwrap();
    source
        .set_device_sync_category(SyncCategory::Appearance, true)
        .unwrap();
    target
        .set_device_sync_category(SyncCategory::Appearance, true)
        .unwrap();
    for _ in 0..6 {
        target
            .pull_account_documents_page(&peer, &mut cursor)
            .await
            .unwrap();
    }
    assert_eq!(
        target.account_document(&key()).unwrap().unwrap().payload(),
        b"after probe",
        "legacy domains must keep converging during optional control cooldown"
    );
    assert!(
        cursor.status(target.device_sync_policy()).2,
        "V2 success must not clear the custody upgrade hint"
    );
    assert!(!cursor.up_to_date);
    assert_eq!(
        cursor.domain, 6,
        "both V2 control domains still exchange during custody cooldown"
    );
    task.abort();
    let _ = task.await;
}

#[test]
fn private_recall_witness_cannot_recall_another_accounts_target() {
    use crate::{
        discovery::Announce,
        eventlog::{Event, EventId, EventKind},
        node::{
            DmEnvelope, MessageBody, RecallEnvelope,
            conversation::{account_conversation_id, dm_conversation_id},
            test_support::node_with_files,
        },
    };
    let dir = tempfile::tempdir().unwrap();
    let own = Account::generate();
    let peer = Account::generate();
    let actor = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&actor, &peer, "peer", 9);
    let (observer, _, _) = node_with_files(
        dir.path(),
        DeviceIdentity::generate(),
        Account::from_secret_bytes(own.secret_bytes()),
        &proof,
    );
    let target = EventId::new([50; 32]);
    let acct_conv = account_conversation_id(&own.account_id(), &peer.account_id());
    let body = DmEnvelope::new(
        own.account_id(),
        peer.account_id(),
        *target.as_bytes(),
        MessageBody::new(b"authored by us".to_vec(), None).encode(),
    )
    .encode();
    observer
        .sentlog
        .lock()
        .unwrap()
        .record(acct_conv, 1, 1, &body)
        .unwrap();
    let conv = dm_conversation_id(&observer.identity.public(), &actor.public());
    let recall = RecallEnvelope::new(peer.account_id(), own.account_id(), *target.as_bytes());
    let event = Event::new(
        &actor,
        conv,
        1,
        vec![],
        1,
        2,
        EventKind::Delete,
        crate::dm::seal(
            &actor,
            &observer.identity.public().x25519_pub,
            &recall.encode(),
        )
        .unwrap(),
    );
    observer.log.lock().unwrap().append(event).unwrap();
    observer.emit_new_messages(conv);
    assert!(
        !observer
            .recalled_targets_account(&peer.account_id())
            .unwrap()
            .contains(&target)
    );
    assert!(!observer.private_dm_message_recalled(&peer.account_id(), target, &own.account_id()));
    assert!(
        observer
            .account_history(&peer.account_id(), 10)
            .unwrap()
            .iter()
            .all(|message| !message.recalled)
    );
}

#[tokio::test]
async fn channel_custody_refreshes_holder_certificates_and_deduplicates_explicit_legacy_members() {
    use crate::{discovery::Announce, eventlog::EventId, node::account_devices::AccountDevice};
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let bob_account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let sibling = node(&dir.path().join("sibling"), &account);
    let bob = node(&dir.path().join("bob"), &bob_account);
    let bob2 = node(&dir.path().join("bob2"), &bob_account);
    for (left, right) in [(&source, &sibling), (&sibling, &source)] {
        left.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: right.identity.public(),
                certificate: account.certify(&right.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let mut servers = Vec::new();
    let mut addresses = Vec::new();
    for current in [&source, &sibling, &bob, &bob2] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        addresses.push(address);
        // Bare signed announcements model a legacy member before its verified
        // account certificate becomes available through discovery.
        for observer in [&source, &sibling, &bob, &bob2] {
            if observer.user_id() != current.user_id() {
                let announce = if current.user_id() == source.user_id()
                    || current.user_id() == sibling.user_id()
                {
                    Announce::new_with_account(
                        &current.identity,
                        &account,
                        "fixture",
                        address.port(),
                    )
                } else {
                    Announce::new(&current.identity, "fixture", address.port())
                };
                observer.roster.lock().unwrap().update(
                    &announce,
                    address.ip(),
                    &observer.user_id(),
                );
            }
        }
        servers.push(tokio::spawn(current.clone().run_accept_loop(listener)));
    }
    let channel = source
        .create_channel(
            "legacy custody certificate refresh",
            vec![
                bob.identity.public(),
                bob2.identity.public(),
                sibling.identity.public(),
            ],
        )
        .await
        .unwrap();
    source
        .send_channel_message(channel, b"synthetic certificate refresh")
        .await
        .unwrap();
    let event = source.channel_history(channel, 10).unwrap()[0].id;
    assert_eq!(
        source.channel_custody_count(channel, event).unwrap(),
        Some(2),
        "unbound explicit legacy members have separate device principals"
    );
    let key = AccountDocumentKey::ChannelCustody {
        channel: hex::encode(channel.as_bytes()),
        event: event.to_hex(),
        holder: bob.user_id(),
    };
    let before = source.account_document_meta(&key).unwrap();
    let source_peer = sibling.routing_peer(&source.user_id()).unwrap();
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
    sibling
        .pull_account_documents_page(&source_peer, &mut cursor)
        .await
        .unwrap();
    assert_eq!(
        sibling.channel_custody_count(channel, event).unwrap(),
        Some(2)
    );
    for (holder, address) in [(&bob, addresses[2]), (&bob2, addresses[3])] {
        source.roster.lock().unwrap().update(
            &Announce::new_with_account(&holder.identity, &bob_account, "Bob", address.port()),
            address.ip(),
            &source.user_id(),
        );
        let peer = source.routing_peer(&holder.user_id()).unwrap();
        source
            .recheck_channel_custody_peer(channel, event, &peer)
            .await;
    }
    let upgraded = source.account_document_meta(&key).unwrap();
    assert!(upgraded.revision > before.revision);
    assert_ne!(upgraded.content_hash, before.content_hash);
    let peer = source.routing_peer(&bob.user_id()).unwrap();
    source
        .recheck_channel_custody_peer(channel, event, &peer)
        .await;
    assert_eq!(
        source.account_document_meta(&key).unwrap(),
        upgraded,
        "unchanged positive rechecks do not churn revisions"
    );
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
    sibling
        .pull_account_documents_page(&source_peer, &mut cursor)
        .await
        .unwrap();
    assert_eq!(
        sibling.channel_custody_count(channel, event).unwrap(),
        Some(1),
        "positive runtime rechecks upgrade the sibling's certified principal projection"
    );
    assert!(
        source
            .put_account_document_if(
                key.clone(),
                b"forged".to_vec(),
                Some(upgraded.version()),
                |commit| commit()
            )
            .is_err()
    );
    assert!(
        source
            .remove_account_document_if(key, Some(upgraded.version()), |commit| commit())
            .is_err()
    );
    assert_eq!(
        source
            .channel_custody_count(channel, EventId::new([0; 32]))
            .unwrap(),
        None
    );
    for server in servers {
        server.abort();
    }
    let secret = sibling.identity.secret_bytes();
    let reopened = node_with_identity(
        &dir.path().join("sibling"),
        &account,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    assert_eq!(
        reopened.channel_custody_count(channel, event).unwrap(),
        Some(1),
        "certified explicit members deduplicate after discovery is forgotten"
    );
}

#[tokio::test]
async fn group_inventory_worker_shares_two_legacy_groups_without_owner_and_survives_restart() {
    group_inventory_worker_scenario(false).await;
}
#[tokio::test]
async fn group_inventory_worker_keeps_directory_when_all_optional_categories_are_off() {
    group_inventory_worker_scenario(true).await;
}
async fn group_inventory_worker_scenario(optional_off: bool) {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        discovery::Announce,
        eventlog::{Event, EventKind},
        node::account_devices::AccountDevice,
    };
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let source_path = dir.path().join("source");
    let source = node(&source_path, &Account::generate());
    let target_path = dir.path().join("target");
    let target = node(&target_path, &account);
    let mut ids = Vec::new();
    for name in ["Labali", "labali"] {
        let id = new_channel_id();
        let event = Event::new(
            &owner.identity,
            id,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: name.into(),
                members: vec![owner.identity.public(), source.identity.public()],
                epoch: 0,
                owner: owner.user_id(),
            }
            .encode(),
        );
        for node in [&owner, &source] {
            node.log.lock().unwrap().append(event.clone()).unwrap();
            node.process_channel(id);
        }
        assert!(source.channel_owner_account_id(id).is_none());
        ids.push(id);
    }
    // The exact legacy device grant remains valid after this device adopts the
    // current root; reconstruct its metadata rather than copying old-root docs.
    source.prepare_account_transition(&account).unwrap();
    let secret = source.identity.secret_bytes();
    drop(source);
    let source = node_with_identity(
        &source_path,
        &account,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    let owner_announce =
        Announce::new_with_account(&owner.identity, &owner.account, "Owner canonical", 4242);
    source.roster.lock().unwrap().update(
        &owner_announce,
        "127.0.0.1".parse().unwrap(),
        &source.user_id(),
    );
    drop(owner); // No third-party owner participates in inventory synchronization.
    if optional_off {
        source
            .put_account_document_if(key(), b"must not transfer".to_vec(), None, |commit| {
                commit()
            })
            .unwrap();
        for node in [&source, &target] {
            for category in [
                SyncCategory::History,
                SyncCategory::Attachments,
                SyncCategory::Appearance,
                SyncCategory::Contacts,
                SyncCategory::Profile,
            ] {
                node.set_device_sync_category(category, false).unwrap();
            }
        }
    }
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    for (node, path, name, port) in [
        (&source, &source_path, "source", address.port()),
        (&target, &target_path, "target", 4243),
    ] {
        node.configure_privacy(
            path,
            "pw",
            &Announce::new_with_account(&node.identity, &account, name, port),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::default()),
        )
        .unwrap();
    }
    source
        .remember_peer(
            &owner_announce.public(),
            Some(&owner_announce),
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
    target.roster.lock().unwrap().update(
        &Announce::new_with_account(&source.identity, &account, "source", address.port()),
        address.ip(),
        &target.user_id(),
    );
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    let source_worker = tokio::spawn(source.clone().run_account_history_loop());
    let worker = tokio::spawn(target.clone().run_account_history_loop());
    let result = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while !ids.iter().all(|id| {
            target
                .list_channels()
                .unwrap()
                .iter()
                .any(|row| row.id == *id)
        }) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    worker.abort();
    source_worker.abort();
    server.abort();
    let _ = worker.await;
    let _ = source_worker.await;
    let _ = server.await;
    assert!(
        result.is_ok(),
        "same-account descriptive inventory must include both distinct legacy groups with their owner offline: expected channels={}; actual channels={}",
        ids.len(),
        target.list_channels().unwrap().len()
    );
    if optional_off {
        assert!(
            target.account_document(&key()).unwrap().is_none(),
            "mandatory directory exchange must not enable optional history documents"
        );
    }
    for id in &ids {
        let row = target
            .list_channels()
            .unwrap()
            .into_iter()
            .find(|row| row.id == *id)
            .unwrap();
        assert!(row.archived);
        let reference = target.channel_shared_reference(*id).unwrap();
        assert!(
            reference
                .members
                .iter()
                .any(|member| member.public.user_id() == reference.owner
                    && member.name.as_deref() == Some("Owner canonical")),
            "offline owner display names must come from the certified source snapshot"
        );
        assert!(!target.channel_can_manage(*id).unwrap());
        assert!(
            target.channel_members(*id).is_empty(),
            "display references must never become membership"
        );
    }
    let secret = target.identity.secret_bytes();
    let identity = DeviceIdentity::from_secret_bytes(secret.0, secret.1);
    drop(target);
    drop(source);
    let reopened = node_with_identity(&target_path, &account, identity);
    for id in ids {
        assert!(
            reopened
                .list_channels()
                .unwrap()
                .iter()
                .any(|row| row.id == id)
        );
    }
}

fn group_inventory_legacy_fixture(
    owner: &Node,
    source: &Node,
    name: &str,
) -> crate::eventlog::ConversationId {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventKind},
    };
    let id = new_channel_id();
    let event = Event::new(
        &owner.identity,
        id,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: name.into(),
            members: vec![owner.identity.public(), source.identity.public()],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    source.log.lock().unwrap().append(event).unwrap();
    source.process_channel(id);
    id
}
fn group_inventory_source_document(
    source: &Node,
    id: crate::eventlog::ConversationId,
) -> AccountDocument {
    source.publish_group_inventory(&mut None);
    source
        .account_document(&AccountDocumentKey::GroupInventory {
            channel: hex::encode(id.as_bytes()),
            source_device: source.user_id(),
        })
        .unwrap()
        .unwrap()
}
#[test]
fn group_inventory_preserves_unchanged_revision_heals_blobs_and_rejects_forged_controls() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let id = group_inventory_legacy_fixture(&owner, &source, "Real group");
    let first = group_inventory_source_document(&source, id);
    let second = group_inventory_source_document(&source, id);
    assert_eq!(
        first.meta, second.meta,
        "unchanged local snapshots must not churn revisions"
    );
    source
        .account_documents
        .lock()
        .unwrap()
        .remove_blob_for_test(first.meta.content_hash);
    assert!(source.account_document(&first.meta.key).is_err());
    let healed = group_inventory_source_document(&source, id);
    assert_eq!(healed.meta, first.meta);
    assert_eq!(healed.payload(), first.payload());
    assert!(
        source
            .put_account_document_if(
                first.meta.key.clone(),
                first.payload().to_vec(),
                Some(first.meta.version()),
                |commit| commit()
            )
            .is_err()
    );
    assert!(
        source
            .remove_account_document_if(
                first.meta.key.clone(),
                Some(first.meta.version()),
                |commit| commit()
            )
            .is_err()
    );
    let target = node(&dir.path().join("target"), &account);
    let mut value: serde_json::Value = serde_json::from_slice(first.payload()).unwrap();
    value["source_device"] = serde_json::json!("a".repeat(32));
    let forged = serde_json::to_vec(&value).unwrap();
    let forged_meta = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        first.meta.key.clone(),
        first.meta.revision + 1,
        &forged,
        false,
    );
    assert!(
        target
            .account_documents
            .lock()
            .unwrap()
            .merge(forged_meta, &forged)
            .is_err()
    );
    let foreign = Account::generate();
    let foreign_meta = AccountDocumentMeta::new(
        &foreign,
        source.user_id(),
        first.meta.key.clone(),
        first.meta.revision + 1,
        first.payload(),
        false,
    );
    assert!(
        target
            .account_documents
            .lock()
            .unwrap()
            .merge(foreign_meta, first.payload())
            .is_err()
    );
    value = serde_json::from_slice(first.payload()).unwrap();
    value["observer"]["certificate"] =
        serde_json::to_value(foreign.certify(&source.identity.public().ed25519_pub)).unwrap();
    let payload = serde_json::to_vec(&value).unwrap();
    let meta = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        first.meta.key.clone(),
        first.meta.revision + 1,
        &payload,
        false,
    );
    assert!(
        target
            .account_documents
            .lock()
            .unwrap()
            .merge(meta, &payload)
            .is_err()
    );
}

#[test]
fn group_inventory_sources_retract_independently_and_restore_absence_preserves_reference() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let a = node(&dir.path().join("a"), &account);
    let b = node(&dir.path().join("b"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let target = node(&dir.path().join("target"), &account);
    let id = group_inventory_legacy_fixture(&owner, &a, "Before");
    let first = group_inventory_source_document(&a, id);
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(first.meta.clone(), first.payload())
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(first.payload()).unwrap();
    value["source_device"] = serde_json::json!(b.user_id());
    value["observer"] = serde_json::to_value(crate::node::account_devices::AccountDevice {
        public: b.identity.public(),
        certificate: account.certify(&b.identity.public().ed25519_pub),
    })
    .unwrap();
    value["members"].as_array_mut().unwrap().push(serde_json::json!({"public":b.identity.public(),"account_id":account.account_id(),"pending":false,"post_office":false,"name":"Sibling"}));
    value["name"] = serde_json::json!("Newer");
    // Both accepted devices project to the same account member.
    let payload = serde_json::to_vec(&value).unwrap();
    let second_key = AccountDocumentKey::GroupInventory {
        channel: hex::encode(id.as_bytes()),
        source_device: b.user_id(),
    };
    let second = AccountDocumentMeta::new(
        &account,
        b.user_id(),
        second_key.clone(),
        first.meta.revision + 1,
        &payload,
        false,
    );
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(second.clone(), &payload)
        .unwrap();
    assert_eq!(target.channel_shared_reference(id).unwrap().name, "Newer");
    let retraction = AccountDocumentMeta::new(
        &account,
        b.user_id(),
        second_key,
        second.revision + 1,
        &[],
        true,
    );
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(retraction, &[])
        .unwrap();
    assert!(
        !target
            .account_documents
            .lock()
            .unwrap()
            .merge(second, &payload)
            .unwrap()
    );
    assert_eq!(
        target.channel_shared_reference(id).unwrap().name,
        "Before",
        "one source retraction cannot erase another source"
    );
    target.publish_group_inventory(&mut None);
    assert_eq!(
        target.channel_shared_reference(id).unwrap().name,
        "Before",
        "borrowed reference cannot be re-exported under target source"
    );
    assert!(
        target
            .account_document_meta(&AccountDocumentKey::GroupInventory {
                channel: hex::encode(id.as_bytes()),
                source_device: target.user_id()
            })
            .is_none()
    );
    // A partial restore of this source's book does not assert a group deletion.
    *a.channels.lock().unwrap() = crate::node::channel::ChannelBook::new();
    a.publish_group_inventory(&mut None);
    assert_eq!(
        a.account_document_meta(&first.meta.key).unwrap(),
        first.meta
    );
    assert!(a.channel_shared_reference(id).is_some());
}

#[tokio::test]
async fn group_inventory_adoption_reconstructs_live_legacy_grants_but_excludes_old_modern_membership()
 {
    let dir = tempfile::tempdir().unwrap();
    let previous = Account::generate();
    let current = Account::generate();
    let source_path = dir.path().join("source");
    let source = node(&source_path, &previous);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let legacy = group_inventory_legacy_fixture(&owner, &source, "Still authorized legacy grant");
    let modern = source
        .create_channel("Old root modern", vec![])
        .await
        .unwrap();
    let mut maintenance =
        crate::node::channel_account_maintenance::ChannelAccountMaintenance::default();
    for _ in 0..source.list_channels().unwrap().len() {
        source.maintain_account_channels(&mut maintenance).await;
    }
    assert!(source.channel_owner_account_id(modern).is_some());
    let old_document = group_inventory_source_document(&source, legacy);
    source.prepare_account_transition(&current).unwrap();
    source
        .persist_account_adoption(&current.public(), || Ok(()), || Ok(source.account.public()))
        .unwrap();
    let secret = source.identity.secret_bytes();
    drop(source);
    let adopted = node_with_identity(
        &source_path,
        &current,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    assert!(
        adopted
            .account_document_meta(&old_document.meta.key)
            .is_none(),
        "old root documents must not be copied on adoption"
    );
    adopted.publish_group_inventory(&mut None);
    let new_document = adopted
        .account_document(&old_document.meta.key)
        .unwrap()
        .unwrap();
    assert_ne!(
        new_document.meta.account_public,
        old_document.meta.account_public
    );
    assert!(
        adopted
            .account_document_meta(&AccountDocumentKey::GroupInventory {
                channel: hex::encode(modern.as_bytes()),
                source_device: adopted.user_id()
            })
            .is_none()
    );
    let sibling = node(&dir.path().join("sibling"), &current);
    sibling
        .account_documents
        .lock()
        .unwrap()
        .merge(new_document.meta.clone(), new_document.payload())
        .unwrap();
    assert!(sibling.channel_shared_reference(legacy).is_some());
    assert!(!sibling.channel_can_send(legacy).unwrap());
    assert!(!sibling.channel_can_manage(legacy).unwrap());
    assert!(sibling.channel_members(legacy).is_empty());
}

#[tokio::test]
async fn group_inventory_v4_cooldown_keeps_v1_v2_v3_and_never_probes_disabled_domains() {
    use crate::{
        discovery::roster::PeerRecord,
        node::{
            account_devices::AccountDevice,
            account_sync_wire::{AccountHistoryFrame, AccountSessionRequest, AccountSyncVersion},
        },
    };
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };
    use tokio::net::TcpListener;
    for optional_off in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = node(&dir.path().join("old-source"), &account);
        let target = node(&dir.path().join("target"), &account);
        for (a, b) in [(&source, &target), (&target, &source)] {
            a.account_devices
                .lock()
                .unwrap()
                .insert(AccountDevice {
                    public: b.identity.public(),
                    certificate: account.certify(&b.identity.public().ed25519_pub),
                })
                .unwrap();
            a.set_device_sync_category(SyncCategory::Appearance, !optional_off)
                .unwrap();
            a.set_device_sync_category(SyncCategory::History, !optional_off)
                .unwrap();
        }
        source
            .put_account_document_if(key(), b"optional draft".to_vec(), None, |commit| commit())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let versions = Arc::new(Mutex::new(Vec::new()));
        let observed = versions.clone();
        let serving = source.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let server = serving.clone();
                let observed = observed.clone();
                tokio::spawn(async move {
                    let mut channel = server.privacy_accept(stream).await.unwrap();
                    while let Ok(bytes) = channel.recv().await {
                        let Some((
                            _,
                            AccountHistoryFrame::SessionRequest {
                                request_id,
                                request: AccountSessionRequest::Documents { body },
                            },
                        )) = AccountHistoryFrame::decode_versioned(&bytes)
                        else {
                            break;
                        };
                        let request: wire::DocumentRequest = serde_json::from_slice(&body).unwrap();
                        observed.lock().unwrap().push(request.version);
                        if request.version >= 4 {
                            break;
                        }
                        if channel
                            .send(
                                &AccountHistoryFrame::SessionReady { request_id }
                                    .encode_version(AccountSyncVersion::V2)
                                    .unwrap(),
                            )
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if !server.serve_account_documents(&mut channel, request).await {
                            break;
                        }
                    }
                });
            }
        });
        let peer = PeerRecord {
            public: source.identity.public(),
            addr: address,
            name: "older sibling".into(),
            account_id: Some(account.account_id()),
            last_seen: Instant::now(),
            post_office: false,
            routes: vec![],
        };
        let mut cursor = sync::DocumentSyncCursor::default();
        assert!(
            target
                .pull_account_documents_page(&peer, &mut cursor)
                .await
                .is_err()
        );
        assert!(cursor.status(target.device_sync_policy()).2);
        let mut hostname_rejections = 0;
        for _ in 0..16 {
            if target
                .pull_account_documents_page(&peer, &mut cursor)
                .await
                .is_err()
            {
                hostname_rejections += 1;
            }
        }
        assert_eq!(
            hostname_rejections, 1,
            "unsupported hostname metadata has its own cooldown"
        );
        let versions = versions.lock().unwrap().clone();
        assert_eq!(
            versions.iter().filter(|version| **version == 4).count(),
            1,
            "optional rejection has its own negative cooldown"
        );
        assert_eq!(
            versions.iter().filter(|version| **version == 5).count(),
            if optional_off { 1 } else { 2 },
            "a closed pooled session permits one fresh negotiation before the domain cooldown"
        );
        if optional_off {
            assert_eq!(
                versions,
                vec![4, 5],
                "disabled optional domains cannot be attempted after selector wraps"
            );
            assert!(
                !cursor.pending,
                "no eligible work must not create an immediate retry spin"
            );
            assert!(target.account_document(&key()).unwrap().is_none());
        } else {
            for version in [1, 2, 3] {
                assert!(
                    versions.contains(&version),
                    "v4 rejection must preserve v{version}"
                );
            }
            assert_eq!(
                target.account_document(&key()).unwrap().unwrap().payload(),
                b"optional draft"
            );
            assert_ne!(
                cursor.digests[6], [0; 32],
                "v3 custody still settles independently"
            );
        }
        assert!(
            cursor.status(target.device_sync_policy()).2,
            "other versions cannot clear the mandatory inventory upgrade hint"
        );
        task.abort();
        let _ = task.await;
    }
}

#[test]
fn group_inventory_archive_projection_is_coherent_and_real_local_state_wins() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let id = group_inventory_legacy_fixture(&owner, &source, "Latest descriptive snapshot");
    let document = group_inventory_source_document(&source, id);
    let mut target = node(&dir.path().join("target"), &account);
    std::sync::Arc::get_mut(&mut target)
        .unwrap()
        .archived_channels
        .push(crate::node::ChannelSummary {
            id,
            name: "Old archived name".into(),
            member_count: 99,
            owner: "a".repeat(32),
            archived: true,
            dissolved: false,
            left: false,
            pending_invite: false,
        });
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(document.meta.clone(), document.payload())
        .unwrap();
    let rows = target.list_channels().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let reference = target.channel_shared_reference(id).unwrap();
    assert_eq!(row.name, reference.name);
    assert_eq!(row.member_count, reference.member_count);
    assert_eq!(row.owner, reference.owner);
    assert!(row.archived);
    // A real local book remains the authority for the native/UI projection.
    let event = source.log.lock().unwrap().events(&id).unwrap()[0].clone();
    target.log.lock().unwrap().append(event).unwrap();
    // The target is absent from this legacy event, so merely retaining that event
    // must not install membership or make the reference writable.
    target.process_channel(id);
    assert!(!target.channel_can_send(id).unwrap());
    assert!(target.channel_members(id).is_empty());
    assert!(source.channel_shared_reference(id).is_none());
    assert!(!source.list_channels().unwrap()[0].archived);
}

#[test]
fn group_inventory_partial_roster_is_explicit_bounded_and_retains_owner_observer() {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventKind},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let id = new_channel_id();
    let mut members = vec![owner.identity.public(), source.identity.public()];
    members.extend((0..255).map(|_| DeviceIdentity::generate().public()));
    let event = Event::new(
        &owner.identity,
        id,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Large group".into(),
            members,
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    source.log.lock().unwrap().append(event).unwrap();
    source.process_channel(id);
    let document = group_inventory_source_document(&source, id);
    let reference: GroupInventoryReference = serde_json::from_slice(document.payload()).unwrap();
    assert!(!reference.members_complete);
    assert_eq!(reference.members.len(), 256);
    assert_eq!(reference.member_count, 257);
    assert!(
        reference
            .members
            .iter()
            .any(|member| member.public == source.identity.public())
    );
    assert!(
        reference
            .members
            .iter()
            .any(|member| member.public == owner.identity.public())
    );
    let target = node(&dir.path().join("target"), &account);
    let mut forged: serde_json::Value = serde_json::from_slice(document.payload()).unwrap();
    forged["members_complete"] = serde_json::json!(true);
    let payload = serde_json::to_vec(&forged).unwrap();
    let meta = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        document.meta.key.clone(),
        document.meta.revision + 1,
        &payload,
        false,
    );
    assert!(
        target
            .account_documents
            .lock()
            .unwrap()
            .merge(meta, &payload)
            .is_err(),
        "truncated facts cannot claim a complete roster"
    );
}

#[test]
fn group_inventory_publishes_validated_departure_and_dissolution_without_granting_membership() {
    use crate::{
        channel::ChannelMeta,
        eventlog::{Event, EventKind},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let target = node(&dir.path().join("target"), &account);
    let id = group_inventory_legacy_fixture(&owner, &source, "Status fixture");
    let initial = group_inventory_source_document(&source, id);
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(initial.meta.clone(), initial.payload())
        .unwrap();
    let (parents, lamport) = source.log.lock().unwrap().prepare(&id).unwrap();
    let departure = Event::new(
        &owner.identity,
        id,
        2,
        parents,
        lamport,
        2,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Status fixture".into(),
            members: vec![owner.identity.public()],
            epoch: 1,
            owner: owner.user_id(),
        }
        .encode(),
    );
    source.log.lock().unwrap().append(departure).unwrap();
    source.process_channel(id);
    assert!(!source.channel_can_send(id).unwrap());
    let left = group_inventory_source_document(&source, id);
    assert!(left.meta.revision > initial.meta.revision);
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(left.meta.clone(), left.payload())
        .unwrap();
    let reference = target.channel_shared_reference(id).unwrap();
    assert!(reference.left);
    assert_eq!(reference.member_count, 1);
    assert!(!reference.dissolved);
    let (parents, lamport) = source.log.lock().unwrap().prepare(&id).unwrap();
    source
        .log
        .lock()
        .unwrap()
        .append(Event::new(
            &owner.identity,
            id,
            3,
            parents,
            lamport,
            3,
            EventKind::ChannelDissolve,
            vec![],
        ))
        .unwrap();
    source.process_channel(id);
    let dissolved = group_inventory_source_document(&source, id);
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(dissolved.meta.clone(), dissolved.payload())
        .unwrap();
    assert!(target.channel_shared_reference(id).unwrap().dissolved);
    assert!(
        target.list_channels().unwrap()[0].archived
            && target.list_channels().unwrap()[0].left
            && target.list_channels().unwrap()[0].dissolved
    );
    assert!(target.channel_members(id).is_empty());
    assert!(!target.channel_can_send(id).unwrap());
    assert!(!target.channel_can_manage(id).unwrap());
}

#[test]
fn group_inventory_post_office_display_fact_keeps_existing_member_count_consistent() {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        discovery::Announce,
        eventlog::{Event, EventKind},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let relay = DeviceIdentity::generate();
    source.roster.lock().unwrap().update(
        &Announce::new_post_office(&relay, "Relay", 4242),
        "127.0.0.1".parse().unwrap(),
        &source.user_id(),
    );
    assert!(source.device_is_post_office(&relay.public()));
    let id = new_channel_id();
    source
        .log
        .lock()
        .unwrap()
        .append(Event::new(
            &owner.identity,
            id,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Legacy group with explicit relay".into(),
                members: vec![
                    owner.identity.public(),
                    source.identity.public(),
                    relay.public(),
                ],
                epoch: 0,
                owner: owner.user_id(),
            }
            .encode(),
        ))
        .unwrap();
    source.process_channel(id);
    assert_eq!(source.list_channels().unwrap()[0].member_count, 2);
    source.publish_group_inventory(&mut None);
    let key = AccountDocumentKey::GroupInventory {
        channel: hex::encode(id.as_bytes()),
        source_device: source.user_id(),
    };
    let document = source
        .account_document(&key)
        .unwrap()
        .expect("verified infrastructure facts cannot prevent inventory publication");
    let reference: GroupInventoryReference = serde_json::from_slice(document.payload()).unwrap();
    assert_eq!(reference.member_count, 2);
    assert_eq!(reference.members.len(), 3);
    assert!(reference.members_complete);
    let value: serde_json::Value = serde_json::from_slice(document.payload()).unwrap();
    let relay_fact = value["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["public"] == serde_json::to_value(relay.public()).unwrap())
        .unwrap();
    assert_eq!(relay_fact["post_office"], true);
    let target = node(&dir.path().join("target"), &account);
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(document.meta.clone(), document.payload())
        .unwrap();
    assert_eq!(target.list_channels().unwrap()[0].member_count, 2);
    assert!(
        target.channel_members(id).is_empty(),
        "infrastructure display facts cannot become memberships"
    );
}

#[test]
fn group_inventory_long_canonical_names_fit_payload_budget_and_preserve_required_members() {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        discovery::Announce,
        eventlog::{Event, EventKind},
    };
    for owner_is_observer in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = node(&dir.path().join("source"), &account);
        let external_owner = DeviceIdentity::generate();
        let owner = if owner_is_observer {
            &source.identity
        } else {
            &external_owner
        };
        let name = "😀".repeat(50);
        assert_eq!(name.len(), 200);
        source.set_profile_display_name(&name).unwrap();
        let mut members = vec![source.identity.public()];
        if !owner_is_observer {
            members.push(owner.public());
            source.roster.lock().unwrap().update(
                &Announce::new(owner, &name, 4242),
                "127.0.0.1".parse().unwrap(),
                &source.user_id(),
            );
        }
        while members.len() < 256 {
            let peer = DeviceIdentity::generate();
            source.roster.lock().unwrap().update(
                &Announce::new_with_account(&peer, &account, &name, 4242),
                "127.0.0.1".parse().unwrap(),
                &source.user_id(),
            );
            members.push(peer.public());
        }
        let id = new_channel_id();
        source
            .log
            .lock()
            .unwrap()
            .append(Event::new(
                owner,
                id,
                1,
                vec![],
                1,
                1,
                EventKind::MembershipChange,
                ChannelMeta {
                    name: "Long canonical member names".into(),
                    members,
                    epoch: 0,
                    owner: owner.public().user_id(),
                }
                .encode(),
            ))
            .unwrap();
        source.process_channel(id);
        let count = source.list_channels().unwrap()[0].member_count;
        source.publish_group_inventory(&mut None);
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(id.as_bytes()),
            source_device: source.user_id(),
        };
        let document = source
            .account_document(&key)
            .unwrap()
            .expect("valid long canonical member names must not suppress group inventory");
        assert!(document.payload().len() <= key.limit());
        let reference: GroupInventoryReference =
            serde_json::from_slice(document.payload()).unwrap();
        assert_eq!(reference.member_count, count);
        assert!(
            !reference.members_complete,
            "byte-budget truncation must be explicit"
        );
        assert!(reference.members.len() < 256);
        assert!(
            reference
                .members
                .iter()
                .all(|member| member.name.as_deref() == Some(&name))
        );
        assert!(
            reference
                .members
                .iter()
                .any(|member| member.public == owner.public())
        );
        assert!(
            reference
                .members
                .iter()
                .any(|member| member.public == source.identity.public())
        );
        let repeated = group_inventory_source_document(&source, id);
        assert_eq!(
            repeated.meta, document.meta,
            "deterministic budgeting must not churn revisions"
        );
        let target = node(&dir.path().join("target"), &account);
        target
            .account_documents
            .lock()
            .unwrap()
            .merge(document.meta.clone(), document.payload())
            .unwrap();
        assert_eq!(target.list_channels().unwrap()[0].member_count, count);
        assert!(
            !target
                .channel_shared_reference(id)
                .unwrap()
                .members_complete
        );
        assert!(!target.channel_can_send(id).unwrap());
    }
}

#[tokio::test]
async fn group_inventory_display_labels_cover_real_create_and_rename_without_changing_book_names() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let long = "界".repeat(342);
    let boundary = format!("{}x", "界".repeat(341));
    assert_eq!(boundary.len(), 1024);
    let cases = [
        long.as_str(),
        "Team\nChat",
        "\n\t\r",
        "",
        "  Ordinary name  ",
        boundary.as_str(),
    ];
    for name in cases {
        let id = source.create_channel(name, vec![]).await.unwrap();
        source.publish_group_inventory(&mut None);
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(id.as_bytes()),
            source_device: source.user_id(),
        };
        let document = source
            .account_document(&key)
            .unwrap()
            .expect("a valid real group name must always have an inventory display label");
        let reference: GroupInventoryReference =
            serde_json::from_slice(document.payload()).unwrap();
        assert!(reference.name.len() <= 1024 && !reference.name.chars().any(char::is_control));
        assert_eq!(
            source.channels.lock().unwrap().state(&id).unwrap().name(),
            name
        );
        assert_eq!(reference.channel_id, hex::encode(id.as_bytes()));
        if name == long {
            assert!(reference.name.ends_with('…'));
            assert!(long.starts_with(reference.name.trim_end_matches('…')));
        } else if name == "Team\nChat" {
            assert_eq!(reference.name, "Team Chat");
        } else if name.trim().is_empty() {
            assert!(reference.name.contains(&hex::encode(id.as_bytes())[..12]));
        } else {
            assert_eq!(
                reference.name, name,
                "ordinary names and exact UTF-8 byte boundaries must remain intact"
            );
        }
        assert_eq!(
            group_inventory_source_document(&source, id).meta,
            document.meta
        );
    }
    let id = source
        .create_channel("Before rename", vec![])
        .await
        .unwrap();
    let initial = group_inventory_source_document(&source, id);
    source.rename_channel(id, &long).await.unwrap();
    let renamed = group_inventory_source_document(&source, id);
    let reference: GroupInventoryReference = serde_json::from_slice(renamed.payload()).unwrap();
    assert!(reference.name.ends_with('…') && reference.name.len() <= 1024);
    assert!(
        renamed.meta.revision > initial.meta.revision,
        "a long valid rename must not leave the previous inventory label stale"
    );
    assert_eq!(
        source.channels.lock().unwrap().state(&id).unwrap().name(),
        long
    );
    source.rename_channel(id, "Team\nChat").await.unwrap();
    let controlled = group_inventory_source_document(&source, id);
    let reference: GroupInventoryReference = serde_json::from_slice(controlled.payload()).unwrap();
    assert_eq!(reference.name, "Team Chat");
    assert_eq!(
        source.channels.lock().unwrap().state(&id).unwrap().name(),
        "Team\nChat"
    );
    assert_eq!(
        group_inventory_source_document(&source, id).meta,
        controlled.meta
    );
}

#[test]
fn device_hostnames_are_account_private_bounded_persistent_and_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    let outsider = node(&dir.path().join("outsider"), &Account::generate());
    source.set_device_hostname(Some("Kingson-MacBook")).unwrap();
    let key = AccountDocumentKey::DeviceHostname {
        device_id: source.user_id(),
    };
    let first = source.account_document(&key).unwrap().unwrap();
    source.set_device_hostname(Some("Kingson-MacBook")).unwrap();
    assert_eq!(source.account_document_meta(&key), Some(first.meta.clone()));
    assert!(
        target
            .account_documents
            .lock()
            .unwrap()
            .merge(first.meta.clone(), first.payload())
            .unwrap()
    );
    assert!(
        outsider
            .account_documents
            .lock()
            .unwrap()
            .merge(first.meta.clone(), first.payload())
            .is_err()
    );
    // Unknown device labels never enter the roster or its presentation projection.
    assert!(
        !target
            .device_hostnames()
            .unwrap()
            .contains_key(&source.user_id())
    );
    target
        .account_devices
        .lock()
        .unwrap()
        .insert(crate::node::account_devices::AccountDevice {
            public: source.identity.public(),
            certificate: account.certify(&source.identity.public().ed25519_pub),
        })
        .unwrap();
    assert_eq!(
        target.device_hostnames().unwrap()[&source.user_id()],
        "Kingson-MacBook"
    );
    assert!(source.set_device_hostname(Some("bad\nhost")).is_err());
    assert!(source.set_device_hostname(Some(&"a".repeat(256))).is_err());
    assert!(
        source
            .put_account_document_if(
                key.clone(),
                b"spoof".to_vec(),
                Some(first.meta.version()),
                |commit| commit()
            )
            .is_err()
    );
    let forged = AccountDocumentMeta::new(
        &account,
        target.user_id(),
        key.clone(),
        first.meta.revision + 1,
        b"spoof",
        false,
    );
    assert!(!forged.valid(&account.public().ed25519_pub));
    let reopened = AccountDocumentStore::open(
        &dir.path().join("target"),
        "pw",
        &account.public().ed25519_pub,
    )
    .unwrap();
    assert_eq!(
        reopened.get(&key).unwrap().unwrap().payload(),
        b"Kingson-MacBook"
    );
    source.set_device_hostname(None).unwrap();
    assert!(
        !source
            .device_hostnames()
            .unwrap()
            .contains_key(&source.user_id())
    );
}

#[tokio::test]
async fn unrelated_noise_peer_cannot_fetch_device_hostnames() {
    use crate::{
        node::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion},
        transport::SecureChannel,
    };
    use tokio::net::{TcpListener, TcpStream};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    source
        .set_device_hostname(Some("Private-Office-PC"))
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let channel = serving.privacy_accept(stream).await.unwrap();
        serving.serve_connection(channel).await;
    });
    let stranger = DeviceIdentity::generate();
    let mut channel = SecureChannel::connect(
        TcpStream::connect(address).await.unwrap(),
        &stranger,
        Some(&source.identity.public()),
    )
    .await
    .unwrap();
    let request = wire::DocumentRequest {
        version: 5,
        account_id: account.account_id(),
        domain: AccountDocumentDomain::DeviceHostname,
        after: None,
        known_digest: [0; 32],
    };
    channel
        .send(
            &AccountHistoryFrame::DocumentsRequest {
                body: request.encode(),
            }
            .encode_version(AccountSyncVersion::V2)
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), channel.recv())
            .await
            .unwrap()
            .is_err()
    );
    task.await.unwrap();
}

#[tokio::test]
async fn device_hostnames_use_encrypted_sibling_sync_with_optional_categories_off() {
    use crate::{
        discovery::{Announce, roster::PeerRecord},
        node::account_devices::AccountDevice,
    };
    use std::time::Instant;
    use tokio::net::TcpListener;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let target = node(&dir.path().join("target"), &account);
    for (a, b) in [(&source, &target), (&target, &source)] {
        a.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: b.identity.public(),
                certificate: account.certify(&b.identity.public().ed25519_pub),
            })
            .unwrap();
        for category in [
            SyncCategory::History,
            SyncCategory::Attachments,
            SyncCategory::Profile,
            SyncCategory::Appearance,
            SyncCategory::Contacts,
        ] {
            a.set_device_sync_category(category, false).unwrap();
        }
    }
    source
        .set_device_hostname(Some("Private-Office-PC"))
        .unwrap();
    let announce = Announce::new_with_account(&source.identity, &account, "Public nickname", 4242);
    assert!(!format!("{announce:?}").contains("Private-Office-PC"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = PeerRecord {
        public: source.identity.public(),
        addr: listener.local_addr().unwrap(),
        name: "Public nickname".into(),
        account_id: Some(account.account_id()),
        last_seen: Instant::now(),
        post_office: false,
        routes: vec![],
    };
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    let mut cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::DeviceHostname);
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    assert_eq!(
        target.device_hostnames().unwrap()[&source.user_id()],
        "Private-Office-PC"
    );
    let bytes = std::fs::read(
        dir.path()
            .join("target")
            .join(format!("account-documents-{}.log", account.account_id())),
    )
    .unwrap();
    assert!(
        !bytes
            .windows(b"Private-Office-PC".len())
            .any(|w| w == b"Private-Office-PC")
    );
    source
        .set_device_hostname(Some("Renamed-Office-PC"))
        .unwrap();
    cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::DeviceHostname);
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    assert_eq!(
        target.device_hostnames().unwrap()[&source.user_id()],
        "Renamed-Office-PC"
    );
    source.set_device_hostname(None).unwrap();
    cursor = sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::DeviceHostname);
    target
        .pull_account_documents_page(&peer, &mut cursor)
        .await
        .unwrap();
    assert!(
        !target
            .device_hostnames()
            .unwrap()
            .contains_key(&source.user_id())
    );
    server.abort();
}
