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
    let event = a.channel_history(channel, 10)[0].id;
    if external_online {
        assert!(
            bob.channel_history(channel, 10)
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
            a.channel_custody_count(channel, event),
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
        assert_eq!(a.channel_custody_count(channel, event), Some(1));
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
        while sibling.channel_custody_count(channel, event) != Some(expected) {
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
        assert_eq!(a.channel_custody_count(channel, event), Some(1));
        assert_eq!(
            bob.channel_custody_count(channel, event),
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
            sibling.channel_custody_count(channel, event),
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
            sibling.channel_custody_count(channel, event),
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
        assert_eq!(a.channel_custody_count(channel, event), Some(0));
        let mut cursor =
            sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
        sibling
            .pull_account_documents_page(&source_peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(
            sibling.channel_custody_count(channel, event),
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
        assert_eq!(sibling.channel_custody_count(channel, event), Some(0));
        a.channel_custody
            .lock()
            .unwrap()
            .confirm(event, &bob.user_id())
            .unwrap();
        a.migrate_channel_custody(&mut None);
        assert_eq!(
            a.channel_custody_count(channel, event),
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
            negative_reopened.channel_custody_count(channel, event),
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
            lost.log.lock().unwrap().get(&event).is_some(),
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
            a.channel_custody_count(channel, event),
            Some(1),
            "a fresh authenticated probe repairs the negative; receiver={:?}",
            lost.list_channels()
        );
        let mut cursor =
            sync::DocumentSyncCursor::for_domain(AccountDocumentDomain::ChannelCustody);
        sibling
            .pull_account_documents_page(&source_peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(sibling.channel_custody_count(channel, event), Some(1));
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
        reopened.channel_custody_count(channel, event),
        Some(final_expected),
        "a fresh node must retain synchronized observations and tombstones"
    );
    assert_eq!(
        a.channel_custody_count(channel, event),
        Some(final_expected),
        "sender siblings do not count as another member account; sibling status={:?}",
        sibling.channel_custody_count(channel, event)
    );
    assert!(
        actual.is_ok(),
        "authenticated external custody must converge on sibling: {:?}",
        sibling.channel_custody_count(channel, event)
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
    let expected = source.account_reactions(&peer_account);
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
        fresh.account_reactions(&peer_account),
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
    let got = fresh.account_reactions(&actor_account.account_id());
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
        source.account_reactions(&peer).is_empty(),
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
    assert!(fresh.account_reactions(&peer).is_empty());
    source
        .set_device_sync_category(SyncCategory::History, true)
        .unwrap();
    pull_control_documents(&source, &fresh, &account).await;
    let expected = source.account_reactions(&peer);
    assert_eq!(fresh.account_reactions(&peer), expected);
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
    assert_eq!(reopened.account_reactions(&peer), expected);
    source
        .react_to_account(&peer, target, "👍", true)
        .await
        .unwrap();
    pull_control_documents(&source, &reopened, &account).await;
    assert!(reopened.account_reactions(&peer).is_empty());
    assert!(
        !reopened
            .account_documents
            .lock()
            .unwrap()
            .merge(doc.meta.clone(), doc.payload())
            .unwrap(),
        "an old witnessed add cannot overtake the later removal"
    );
    assert!(reopened.account_reactions(&peer).is_empty());
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
    for _ in 0..4 {
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
    let mut cursor = sync::DocumentSyncCursor::default();
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
            .contains(&target)
    );
    assert!(!observer.private_dm_message_recalled(&peer.account_id(), target, &own.account_id()));
    assert!(
        observer
            .account_history(&peer.account_id(), 10)
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
    let event = source.channel_history(channel, 10)[0].id;
    assert_eq!(
        source.channel_custody_count(channel, event),
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
    assert_eq!(sibling.channel_custody_count(channel, event), Some(2));
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
        sibling.channel_custody_count(channel, event),
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
        source.channel_custody_count(channel, EventId::new([0; 32])),
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
        reopened.channel_custody_count(channel, event),
        Some(1),
        "certified explicit members deduplicate after discovery is forgotten"
    );
}
