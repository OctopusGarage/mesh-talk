use super::*;
use store::AccountDocumentStore;
#[path = "inventory_causality_tests.rs"]
mod inventory_causality;
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
pub(super) fn node(path: &std::path::Path, account: &Account) -> std::sync::Arc<Node> {
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
    // Match the history worker's optional scope: disabling History must not
    // redirect this custody check into mandatory inventory/control exchanges.
    let custody_cursor = || {
        let mut cursor = sync::DocumentSyncCursor::optional();
        cursor.domain = AccountDocumentDomain::ChannelCustody as usize;
        cursor
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let other = Account::generate();
    let a = node(&dir.path().join("a"), &account);
    let mut sibling = node(&dir.path().join("sibling"), &account);
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
        servers.push(Some(tokio::spawn(
            current.clone().run_accept_loop(listener),
        )));
    }
    let channel = a
        .create_channel(
            "custody fixture",
            vec![sibling.identity.public(), bob.identity.public()],
        )
        .await
        .unwrap();
    if !external_online {
        let server = servers[2].take().unwrap();
        server.abort();
        let _ = server.await;
        bob.runtime_work.drain().await;
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
        {
            let mut documents = a.account_documents.lock().unwrap();
            documents.remove_blob_for_test(before.content_hash);
            *documents = AccountDocumentStore::open(
                &dir.path().join("a"),
                "pw",
                &account.public().ed25519_pub,
            )
            .unwrap();
        }
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
        let server = servers[2].take().unwrap();
        server.abort();
        let _ = server.await;
        bob.runtime_work.drain().await;
        a.roster.lock().unwrap().forget(&bob.identity.public());
        sibling
            .roster
            .lock()
            .unwrap()
            .forget(&bob.identity.public());
    }
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
    assert!(
        actual.is_ok(),
        "authenticated external custody must converge before restart: count={:?}, channels={:?}, own_history={}, observations={:?}, sync={:?}",
        sibling.channel_custody_count(channel, event),
        sibling.list_channels(),
        sibling
            .channel_history(channel, 10)
            .iter()
            .any(|row| row.id == event && row.from_me),
        sibling
            .channel_custody_observations(channel, event)
            .iter()
            .map(|meta| (
                meta,
                sibling
                    .account_document(&meta.key)
                    .map(|document| document.map(|document| document.payload().len())),
            ))
            .collect::<Vec<_>>(),
        sibling.device_sync_status(),
    );
    if !external_online {
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
        let mut cursor = custody_cursor();
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
        let mut cursor = custody_cursor();
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
        let _ = bob_server.await;
        bob.runtime_work.close();
        bob.runtime_work.drain().await;
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
        let mut cursor = custody_cursor();
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
        // Join producers and admitted blocking writes before reopening this profile.
        let server = servers[1].take().unwrap();
        sibling.runtime_work.close();
        server.abort();
        let _ = server.await;
        sibling.runtime_work.drain().await;
        let secret = sibling.identity.secret_bytes();
        sibling = node_with_identity(
            &dir.path().join("sibling"),
            &account,
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        );
        assert_eq!(
            sibling.channel_custody_count(channel, event),
            Some(0),
            "the negative survives a fresh encrypted-store open"
        );
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
        let mut cursor = custody_cursor();
        sibling
            .pull_account_documents_page(&source_peer, &mut cursor)
            .await
            .unwrap();
        assert_eq!(sibling.channel_custody_count(channel, event), Some(1));
        final_expected = 1;
        lost.runtime_work.close();
        lost_server.abort();
        let _ = lost_server.await;
        lost.runtime_work.drain().await;
    }
    for current in [&a, &sibling, &bob] {
        current.runtime_work.close();
    }
    for server in servers.iter().flatten() {
        server.abort();
    }
    for server in servers.into_iter().flatten() {
        let _ = server.await;
    }
    for current in [&a, &sibling, &bob] {
        current.runtime_work.drain().await;
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
        operation: None,
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
        operation: None,
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
                    operation: None,
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
        (AccountDocumentDomain::GroupInventory, 1, false),
        (AccountDocumentDomain::GroupInventory, 2, false),
        (AccountDocumentDomain::GroupInventory, 3, false),
        (AccountDocumentDomain::GroupInventory, 4, true),
        (AccountDocumentDomain::GroupInventory, 5, false),
        (AccountDocumentDomain::GroupControl, 1, false),
        (AccountDocumentDomain::GroupControl, 2, false),
        (AccountDocumentDomain::GroupControl, 3, false),
        (AccountDocumentDomain::GroupControl, 4, false),
        (AccountDocumentDomain::GroupControl, 5, true),
        (AccountDocumentDomain::Draft, 4, false),
    ] {
        let request = DocumentRequest {
            version,
            domain,
            account_id: "a".repeat(32),
            after: None,
            known_digest: [0; 32],
            operation: None,
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
        while !ids
            .iter()
            .all(|id| target.list_channels().iter().any(|row| row.id == *id))
        {
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
        target.list_channels().len()
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
        assert!(!target.channel_can_manage(*id));
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
        assert!(reopened.list_channels().iter().any(|row| row.id == id));
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

fn copy_public_group(source: &Node, target: &Node, group: crate::eventlog::ConversationId) {
    let events: Vec<_> = source
        .log
        .lock()
        .unwrap()
        .events(&group)
        .into_iter()
        .cloned()
        .collect();
    for event in events {
        target.log.lock().unwrap().append(event).unwrap();
    }
    target.process_channel(group);
}

fn group_control_source_document(
    source: &Node,
    group: crate::eventlog::ConversationId,
) -> AccountDocument {
    source.publish_account_group_access(&mut None);
    source
        .account_document(&AccountDocumentKey::GroupControl {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        })
        .unwrap()
        .unwrap()
}

#[test]
fn group_control_signed_heads_and_metadata_reject_forged_or_malformed_records() {
    use crate::node::account_group::wire::{GroupControlHead, MAX_HEAD_BYTES};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &account);
    let group = group_inventory_legacy_fixture(&owner, &member, "Bound signed source");
    copy_public_group(&member, &owner, group);
    let document = group_control_source_document(&owner, group);
    let head = GroupControlHead::decode(document.payload(), &account.public().ed25519_pub).unwrap();
    let mut store = AccountDocumentStore::open(
        &dir.path().join("store"),
        "pw",
        &account.public().ed25519_pub,
    )
    .unwrap();
    assert!(
        store
            .merge(document.meta.clone(), document.payload())
            .unwrap()
    );
    let original = serde_json::from_slice::<serde_json::Value>(document.payload()).unwrap();
    for field in ["signature", "role", "source", "revision", "proof"] {
        let mut forged = original.clone();
        match field {
            "signature" => {
                forged["signature"][0] =
                    serde_json::json!(forged["signature"][0].as_u64().unwrap() ^ 1)
            }
            "role" => forged["access"]["source"]["role"] = serde_json::json!("member"),
            "source" => {
                forged["access"]["source"]["device"]["x25519_pub"][0] = serde_json::json!(
                    forged["access"]["source"]["device"]["x25519_pub"][0]
                        .as_u64()
                        .unwrap()
                        ^ 1
                )
            }
            "revision" => forged["revision"] = serde_json::json!(head.revision + 1),
            "proof" => {
                forged["proof"]["x25519_pub"][0] =
                    serde_json::json!(forged["proof"]["x25519_pub"][0].as_u64().unwrap() ^ 1)
            }
            _ => unreachable!(),
        }
        let payload = serde_json::to_vec(&forged).unwrap();
        let meta = AccountDocumentMeta::new(
            &account,
            owner.user_id(),
            document.meta.key.clone(),
            head.revision,
            &payload,
            false,
        );
        assert!(
            store.merge(meta, &payload).is_err(),
            "forged {field} must be rejected before installation"
        );
    }
    let wrong_signer = GroupControlHead::new(
        &member.identity,
        group,
        head.revision,
        head.proof.clone(),
        head.access.clone(),
    )
    .encode();
    let wrong_signer_meta = AccountDocumentMeta::new(
        &account,
        owner.user_id(),
        document.meta.key.clone(),
        head.revision,
        &wrong_signer,
        false,
    );
    assert!(store.merge(wrong_signer_meta, &wrong_signer).is_err());
    let wrong_keys = [
        AccountDocumentKey::GroupControl {
            channel: "f".repeat(64),
            source_device: owner.user_id(),
        },
        AccountDocumentKey::GroupControl {
            channel: hex::encode(group.as_bytes()),
            source_device: member.user_id(),
        },
    ];
    for key in wrong_keys {
        let AccountDocumentKey::GroupControl { source_device, .. } = &key else {
            unreachable!()
        };
        let meta = AccountDocumentMeta::new(
            &account,
            source_device.clone(),
            key,
            head.revision,
            document.payload(),
            false,
        );
        assert!(store.merge(meta, document.payload()).is_err());
    }
    let wrong_revision = AccountDocumentMeta::new(
        &account,
        owner.user_id(),
        document.meta.key.clone(),
        head.revision + 1,
        document.payload(),
        false,
    );
    assert!(store.merge(wrong_revision, document.payload()).is_err());
    let wrong_account = AccountDocumentMeta::new(
        &Account::generate(),
        owner.user_id(),
        document.meta.key.clone(),
        head.revision,
        document.payload(),
        false,
    );
    assert!(store.merge(wrong_account, document.payload()).is_err());
    let tombstone = AccountDocumentMeta::new(
        &account,
        owner.user_id(),
        document.meta.key.clone(),
        head.revision + 1,
        &[],
        true,
    );
    assert!(store.merge(tombstone, &[]).is_err());
    for payload in [vec![b'{'], vec![0; MAX_HEAD_BYTES + 1]] {
        let meta = AccountDocumentMeta::new(
            &account,
            owner.user_id(),
            document.meta.key.clone(),
            head.revision + 1,
            &payload,
            false,
        );
        assert!(store.merge(meta, &payload).is_err());
    }
    assert_eq!(
        store.get(&document.meta.key).unwrap().unwrap().payload(),
        document.payload()
    );
    assert!(
        member
            .put_account_document_if(
                document.meta.key.clone(),
                document.payload().to_vec(),
                None,
                |commit| commit()
            )
            .is_err()
    );
}

#[tokio::test]
async fn group_control_terminal_sources_keep_other_members_but_dissolution_overrides_stale_roles() {
    use crate::node::AccountGroupRole;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    reference
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let group = group_inventory_legacy_fixture(&owner, &member, "Terminal observation");
    copy_public_group(&member, &owner, group);
    for source in [&owner, &member] {
        let document = group_control_source_document(source, group);
        reference
            .account_documents
            .lock()
            .unwrap()
            .merge(document.meta.clone(), document.payload())
            .unwrap();
    }
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    owner.dissolve_channel(group).await.unwrap();
    let terminal = group_control_source_document(&owner, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(terminal.meta.clone(), terminal.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Dissolved,
        "a stale Member head cannot resurrect a group with a signed terminal source observation"
    );
    assert!(reference.channel_members(group).is_empty());
}

#[tokio::test]
async fn group_control_current_account_binding_replacement_revokes_old_owner_observation() {
    use crate::node::AccountGroupRole;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &account);
    let group = group_inventory_legacy_fixture(&owner, &member, "Binding replacement");
    copy_public_group(&member, &owner, group);
    let document = group_control_source_document(&owner, group);
    member
        .account_documents
        .lock()
        .unwrap()
        .merge(document.meta.clone(), document.payload())
        .unwrap();
    let member_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let member_port = member_listener.local_addr().unwrap().port();
    member
        .configure_privacy(
            &dir.path().join("member"),
            "pw",
            &member.signed_announce("Member", member_port),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::default()),
        )
        .unwrap();
    let prior = owner.signed_announce("Owner", 1);
    member
        .roster
        .lock()
        .unwrap()
        .update(&prior, "127.0.0.1".parse().unwrap(), &member.user_id());
    member.cached_peer_snapshot().unwrap();
    assert_eq!(
        member.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    let replacement = Account::generate();
    let secret = owner.identity.secret_bytes();
    let replacement_path = dir.path().join("replacement");
    let current_owner = node_with_identity(
        &replacement_path,
        &replacement,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    copy_public_group(&owner, &current_owner, group);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let current = crate::discovery::Announce::new_with_account(
        &current_owner.identity,
        &replacement,
        "Owner moved account",
        port,
    );
    current_owner
        .configure_privacy(
            &replacement_path,
            "pw",
            &current,
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::default()),
        )
        .unwrap();
    let server = tokio::spawn(current_owner.clone().run_accept_loop(listener));
    member
        .roster
        .lock()
        .unwrap()
        .update(&current, "127.0.0.1".parse().unwrap(), &member.user_id());
    let change = member
        .account_binding_changes(&account.account_id())
        .pop()
        .unwrap();
    member
        .accept_account_binding_change_guarded(&change, |commit| commit())
        .await
        .unwrap();
    server.abort();
    let _ = server.await;
    assert_eq!(
        member.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert_eq!(
        member.channel_owner(group),
        owner.user_id(),
        "public signer identity must remain unchanged"
    );
}

#[test]
fn group_control_old_head_replay_and_reopen_preserve_the_latest_signed_observation() {
    use crate::eventlog::EventKind;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &member, "Head replay");
    copy_public_group(&member, &owner, group);
    let old = group_control_source_document(&owner, group);
    owner
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            member.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(group);
    let current = group_control_source_document(&owner, group);
    let store_path = dir.path().join("store");
    let mut store =
        AccountDocumentStore::open(&store_path, "pw", &account.public().ed25519_pub).unwrap();
    assert!(store.merge(old.meta.clone(), old.payload()).unwrap());
    assert!(
        store
            .merge(current.meta.clone(), current.payload())
            .unwrap()
    );
    assert!(!store.merge(old.meta.clone(), old.payload()).unwrap());
    drop(store);
    let store =
        AccountDocumentStore::open(&store_path, "pw", &account.public().ed25519_pub).unwrap();
    assert_eq!(
        store.get(&current.meta.key).unwrap().unwrap().payload(),
        current.payload()
    );
    assert_eq!(
        store
            .list(AccountDocumentDomain::GroupControl, None, 64)
            .len(),
        1
    );
}

#[test]
fn group_control_restored_older_public_state_cannot_republish_a_newer_own_head_as_owner() {
    use crate::{
        eventlog::EventKind,
        node::{AccountGroupExecution, AccountGroupRole},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &external, "Restored public prefix");
    copy_public_group(&external, &owner, group);
    let secret = owner.identity.secret_bytes();
    let restored = node_with_identity(
        &dir.path().join("restored"),
        &account,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    copy_public_group(&owner, &restored, group);
    owner
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(group);
    let current = group_control_source_document(&owner, group);
    restored
        .account_documents
        .lock()
        .unwrap()
        .merge(current.meta.clone(), current.payload())
        .unwrap();
    let access = restored.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Member);
    assert_eq!(access.execution, AccountGroupExecution::Reconciling);
    restored.publish_account_group_access(&mut None);
    let retained = restored
        .account_document(&current.meta.key)
        .unwrap()
        .unwrap();
    assert_eq!(retained.meta, current.meta);
    assert_eq!(
        retained.payload(),
        current.payload(),
        "stale public replay must not create a higher-revision Owner head"
    );
}

#[test]
fn group_control_rejected_parent_cannot_hide_a_later_applied_owner_transfer_on_restore() {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventKind},
        node::AccountGroupRole,
    };
    let dir = tempfile::tempdir().unwrap();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let recipient = node(&dir.path().join("recipient"), &Account::generate());
    let group = new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Rejected parent causal knowledge".into(),
            members: vec![
                owner.identity.public(),
                source.identity.public(),
                recipient.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    let first = (2..18)
        .map(|clock| {
            Event::new(
                &owner.identity,
                group,
                2,
                vec![genesis.id],
                2,
                clock,
                EventKind::ChannelOwnerTransfer,
                source.user_id().into_bytes(),
            )
        })
        .min_by_key(|event| event.id)
        .unwrap();
    let rejected = (18..1042)
        .map(|clock| {
            Event::new(
                &owner.identity,
                group,
                3,
                vec![genesis.id],
                2,
                clock,
                EventKind::ChannelOwnerTransfer,
                recipient.user_id().into_bytes(),
            )
        })
        .find(|event| event.id > first.id)
        .expect("signed competing owner decision");
    for event in [&genesis, &first, &rejected] {
        source.log.lock().unwrap().append(event.clone()).unwrap();
    }
    source.process_channel(group);
    assert_eq!(source.channel_owner(group), source.user_id());
    let keys = source.identity.secret_bytes();
    let restored = node_with_identity(
        &dir.path().join("restored"),
        &account,
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
    );
    copy_public_group(&source, &restored, group);
    let applied_seq = source
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            recipient.user_id().into_bytes(),
        )
        .unwrap();
    let applied = source
        .log
        .lock()
        .unwrap()
        .authored_event(
            group,
            crate::eventlog::event::Author::from_ed25519(source.identity.public().ed25519_pub),
            applied_seq,
        )
        .unwrap()
        .id;
    source.process_channel(group);
    assert_eq!(
        source.channel_owner(group),
        recipient.user_id(),
        "the existing full reducer applies the valid new owner's transfer"
    );
    assert!(
        !source
            .account_group_projection(group)
            .authorized
            .contains(&applied),
        "the rejected competing parent means the historical authorized index omits this applied decision"
    );
    let latest = group_control_source_document(&source, group);
    restored
        .account_documents
        .lock()
        .unwrap()
        .merge(latest.meta.clone(), latest.payload())
        .unwrap();
    restored.publish_account_group_access(&mut None);
    assert!(
        restored.account_document_meta(&latest.meta.key) == Some(latest.meta.clone()),
        "knowledge of an applied role decision must survive its rejected parent; restored B must not republish Owner"
    );
    assert_eq!(
        restored.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
}

#[test]
fn group_control_unreadable_latest_own_head_cannot_be_overwritten_by_a_restored_owner() {
    use crate::{eventlog::EventKind, node::AccountGroupRole};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &external, "Unreadable latest authority");
    copy_public_group(&external, &owner, group);
    let keys = owner.identity.secret_bytes();
    let restored = node_with_identity(
        &dir.path().join("restored"),
        &account,
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
    );
    copy_public_group(&owner, &restored, group);
    owner
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(group);
    let latest = group_control_source_document(&owner, group);
    restored
        .account_documents
        .lock()
        .unwrap()
        .merge(latest.meta.clone(), latest.payload())
        .unwrap();
    restored
        .account_documents
        .lock()
        .unwrap()
        .remove_blob_for_test(latest.meta.content_hash);
    restored.publish_account_group_access(&mut None);
    assert!(
        restored.account_document_meta(&latest.meta.key) == Some(latest.meta.clone()),
        "an unreadable durable latest-source floor must not be replaced by an older local Owner projection"
    );
    assert_eq!(
        restored.account_group_access(group).unwrap().role,
        AccountGroupRole::Unknown
    );
}

#[tokio::test]
async fn group_control_source_leave_is_individual_and_all_known_left_sources_project_left() {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventKind},
        node::AccountGroupRole,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let first = node(&dir.path().join("first"), &account);
    let second = node(&dir.path().join("second"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let group = new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Individual source departures".into(),
            members: vec![
                owner.identity.public(),
                first.identity.public(),
                second.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    for node in [&owner, &first, &second] {
        node.log.lock().unwrap().append(genesis.clone()).unwrap();
        node.process_channel(group);
    }
    for source in [&first, &second] {
        let document = group_control_source_document(source, group);
        reference
            .account_documents
            .lock()
            .unwrap()
            .merge(document.meta.clone(), document.payload())
            .unwrap();
    }
    first.leave_channel(group).await.unwrap();
    let left = group_control_source_document(&first, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(left.meta.clone(), left.payload())
        .unwrap();
    let access = reference.account_group_access(group).unwrap();
    assert_eq!(
        access.role,
        AccountGroupRole::Member,
        "one source departure must retain another accepted sibling"
    );
    assert_eq!(access.sources.len(), 1);
    assert_eq!(access.sources[0].device, second.identity.public());
    second.leave_channel(group).await.unwrap();
    let left = group_control_source_document(&second, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(left.meta.clone(), left.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Left
    );
}

#[tokio::test]
async fn group_control_terminal_source_invalidates_other_cached_lifetimes_before_reinvite() {
    group_control_terminal_source_case(true).await;
}

#[tokio::test]
async fn group_control_unacknowledged_conflicting_sources_remain_reconciling() {
    group_control_terminal_source_case(false).await;
}

async fn group_control_terminal_source_case(acknowledge: bool) {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventKind},
        node::AccountGroupRole,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let first = node(&dir.path().join("first"), &account);
    let second = node(&dir.path().join("second"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let group = new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Mixed source lifetime removal".into(),
            members: vec![
                owner.identity.public(),
                first.identity.public(),
                second.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    owner.log.lock().unwrap().append(genesis).unwrap();
    owner.process_channel(group);
    for source in [&first, &second] {
        copy_public_group(&owner, source, group);
    }
    let _first_prior = group_control_source_document(&first, group);
    let stale = group_control_source_document(&second, group);
    if acknowledge {
        first
            .account_documents
            .lock()
            .unwrap()
            .merge(stale.meta.clone(), stale.payload())
            .unwrap();
    }
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(stale.meta.clone(), stale.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    owner
        .append_event(
            group,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Mixed source lifetime removal".into(),
                members: vec![owner.identity.public()],
                epoch: 1,
                owner: owner.user_id(),
            }
            .encode(),
        )
        .unwrap();
    owner.process_channel(group);
    copy_public_group(&owner, &first, group);
    let terminal = group_control_source_document(&first, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(terminal.meta.clone(), terminal.payload())
        .unwrap();
    let access = reference.account_group_access(group).unwrap();
    if !acknowledge {
        assert_eq!(
            access.role,
            AccountGroupRole::Unknown,
            "reference cannot invent causal ordering between unacknowledged conflicting source observations"
        );
        assert_eq!(
            access.execution,
            crate::node::AccountGroupExecution::Reconciling
        );
        return;
    }
    assert_eq!(
        access.role,
        AccountGroupRole::Left,
        "one source carries actual owner-signed removal of both sources; the other writer's cached Member must be invalidated"
    );
    assert!(access.sources.is_empty());
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(stale.meta.clone(), stale.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Left
    );
    assert!(
        reference.channel_members(group).is_empty(),
        "reference must still have no installed public group"
    );
    second
        .account_documents
        .lock()
        .unwrap()
        .merge(terminal.meta.clone(), terminal.payload())
        .unwrap();
    owner
        .append_event(
            group,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Mixed source lifetime removal".into(),
                members: vec![owner.identity.public(), second.identity.public()],
                epoch: 2,
                owner: owner.user_id(),
            }
            .encode_invitation(),
        )
        .unwrap();
    owner.process_channel(group);
    copy_public_group(&owner, &second, group);
    let invited = group_control_source_document(&second, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(invited.meta.clone(), invited.payload())
        .unwrap();
    let access = reference.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Invited);
    let fresh = access.sources[0].lifetime.clone();
    let old = crate::node::account_group::wire::GroupControlHead::decode(
        stale.payload(),
        &account.public().ed25519_pub,
    )
    .unwrap();
    assert_ne!(fresh.membership, old.access.source.lifetime.membership);
    assert!(fresh.acceptance.is_none());
    second.accept_channel_invite(group).await.unwrap();
    let accepted = group_control_source_document(&second, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(accepted.meta.clone(), accepted.payload())
        .unwrap();
    let access = reference.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Member);
    assert_eq!(access.sources[0].lifetime.membership, fresh.membership);
    assert!(access.sources[0].lifetime.acceptance.is_some());
    assert!(reference.channel_members(group).is_empty());
}

#[tokio::test]
async fn group_control_fresh_reinvite_keeps_private_causal_ancestors_out_of_mandatory_metadata() {
    use crate::{eventlog::EventKind, node::AccountGroupRole};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let source = node(&dir.path().join("source"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let group = group_inventory_legacy_fixture(&owner, &source, "Interleaved private ancestry");
    copy_public_group(&source, &owner, group);
    let _prior = group_control_source_document(&source, group);
    source.leave_channel(group).await.unwrap();
    copy_public_group(&source, &owner, group);
    let left = group_control_source_document(&source, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(left.meta.clone(), left.payload())
        .unwrap();
    let private = b"private ancestry payload must never enter mandatory access metadata";
    owner
        .append_event(group, EventKind::Message, private.to_vec())
        .unwrap();
    owner
        .add_channel_member(group, source.identity.public())
        .await
        .unwrap();
    copy_public_group(&owner, &source, group);
    source.accept_channel_invite(group).await.unwrap();
    assert_eq!(
        source.account_group_access(group).unwrap().role,
        AccountGroupRole::Member,
        "the full actual DAG validates the fresh invitation through an intervening private event"
    );
    let fresh = group_control_source_document(&source, group);
    assert!(
        fresh.meta.revision > left.meta.revision,
        "current accepted lifetime must publish despite private causal ancestors"
    );
    assert!(
        !fresh
            .payload()
            .windows(private.len())
            .any(|bytes| bytes == private)
    );
    let value: serde_json::Value = serde_json::from_slice(fresh.payload()).unwrap();
    let exported = value["access"]["controls"].as_array().unwrap();
    assert!(
        exported
            .iter()
            .all(|event| event["kind"] != serde_json::json!("Message"))
    );
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(fresh.meta.clone(), fresh.payload())
        .unwrap();
    let access = reference.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Member);
    assert!(access.sources[0].lifetime.acceptance.is_some());
    assert!(reference.channel_members(group).is_empty());
}

#[test]
fn group_control_equivalent_source_knowledge_reaches_a_publication_fixed_point() {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventKind},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let first = node(&dir.path().join("first"), &account);
    let second = node(&dir.path().join("second"), &account);
    let group = new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Equivalent knowledge".into(),
            members: vec![
                owner.identity.public(),
                first.identity.public(),
                second.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    for source in [&first, &second] {
        source.log.lock().unwrap().append(genesis.clone()).unwrap();
        source.process_channel(group);
    }
    let exchange = || {
        let left = group_control_source_document(&first, group);
        let right = group_control_source_document(&second, group);
        second
            .account_documents
            .lock()
            .unwrap()
            .merge(left.meta.clone(), left.payload())
            .unwrap();
        first
            .account_documents
            .lock()
            .unwrap()
            .merge(right.meta.clone(), right.payload())
            .unwrap();
    };
    for _ in 0..4 {
        exchange();
    }
    let stable_first = group_control_source_document(&first, group);
    let stable_second = group_control_source_document(&second, group);
    for _ in 0..12 {
        exchange();
    }
    assert_eq!(
        group_control_source_document(&first, group).meta,
        stable_first.meta,
        "equivalent source views must not ACK each other's ACK-only revisions forever"
    );
    assert_eq!(
        group_control_source_document(&second, group).meta,
        stable_second.meta
    );
}

#[test]
fn group_control_resource_limit_publishes_a_small_invalidating_head_and_recovers() {
    group_control_resource_limit_case(false, false);
}

#[test]
fn group_control_multifrontier_resource_invalidation_recovers_after_merge_and_reopen() {
    group_control_resource_limit_case(true, false);
}

#[test]
fn group_control_role_control_forks_recover_after_invalidation_merge_and_reopen() {
    group_control_resource_limit_case(true, true);
}

fn group_control_resource_limit_case(multifrontier: bool, role_forks: bool) {
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        eventlog::EventKind,
        node::{AccountGroupExecution, AccountGroupRole},
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let reference = node(&dir.path().join("reference"), &account);
    let group = group_inventory_legacy_fixture(&owner, &external, "Bounded account sources");
    copy_public_group(&external, &owner, group);
    let prior = group_control_source_document(&owner, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(prior.meta.clone(), prior.payload())
        .unwrap();
    let mut members = vec![owner.identity.public(), external.identity.public()];
    for _ in 0..66 {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &account, "Actual synthetic member", 9);
        owner
            .roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &owner.user_id());
        members.push(device.public());
    }
    owner
        .append_event(
            group,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Bounded account sources".into(),
                members: members.clone(),
                epoch: 1,
                owner: owner.user_id(),
            }
            .encode(),
        )
        .unwrap();
    owner.process_channel(group);
    if multifrontier {
        let parents = owner.log.lock().unwrap().heads(&group);
        let first = crate::eventlog::Event::new(
            &owner.identity,
            group,
            3,
            parents.clone(),
            3,
            3,
            if role_forks {
                EventKind::MembershipChange
            } else {
                EventKind::Message
            },
            if role_forks {
                ChannelMeta {
                    name: "Concurrent authorized membership".into(),
                    members: members.clone(),
                    epoch: 2,
                    owner: owner.user_id(),
                }
                .encode()
            } else {
                b"private first branch".to_vec()
            },
        );
        let second = crate::eventlog::Event::new(
            &external.identity,
            group,
            1,
            parents,
            3,
            4,
            if role_forks {
                EventKind::ChannelLeave
            } else {
                EventKind::Message
            },
            if role_forks {
                Vec::new()
            } else {
                b"private second branch".to_vec()
            },
        );
        owner.log.lock().unwrap().append(first).unwrap();
        owner.log.lock().unwrap().append(second).unwrap();
        assert_eq!(owner.log.lock().unwrap().heads(&group).len(), 2);
        owner.process_channel(group);
    }
    let unavailable = group_control_source_document(&owner, group);
    assert!(unavailable.meta.revision > prior.meta.revision);
    assert!(
        unavailable.payload().len() < 32 * 1024,
        "resource fallback reserves a small negative observation"
    );
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(unavailable.meta.clone(), unavailable.payload())
        .unwrap();
    let access = reference.account_group_access(group).unwrap();
    assert_eq!(access.role, AccountGroupRole::Unknown);
    assert_eq!(access.execution, AccountGroupExecution::Reconciling);
    assert_eq!(
        group_control_source_document(&owner, group).meta,
        unavailable.meta,
        "unchanged resource failure must reach a publication fixed point"
    );
    if multifrontier {
        let secrets = owner.identity.secret_bytes();
        let restored = node_with_identity(
            &dir.path().join("restored"),
            &account,
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
        );
        let mut events: Vec<_> = owner
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .cloned()
            .collect();
        events.retain(|event| event.parents.is_empty());
        for event in events {
            restored.log.lock().unwrap().append(event).unwrap();
        }
        restored
            .append_event(
                group,
                EventKind::MembershipChange,
                ChannelMeta {
                    name: "Different authenticated branch".into(),
                    members: vec![restored.identity.public(), external.identity.public()],
                    epoch: 2,
                    owner: restored.user_id(),
                }
                .encode(),
            )
            .unwrap();
        restored
            .append_event(group, EventKind::Message, b"different event one".to_vec())
            .unwrap();
        restored
            .append_event(group, EventKind::Message, b"different event two".to_vec())
            .unwrap();
        restored.process_channel(group);
        assert_eq!(
            restored.log.lock().unwrap().fingerprint_and_len(&group).1,
            owner.log.lock().unwrap().fingerprint_and_len(&group).1
        );
        restored
            .account_documents
            .lock()
            .unwrap()
            .merge(unavailable.meta.clone(), unavailable.payload())
            .unwrap();
        restored.publish_account_group_access(&mut None);
        assert_eq!(
            restored.account_document_meta(&unavailable.meta.key),
            Some(unavailable.meta.clone()),
            "a same-length restored prefix missing an original event cannot restore positive authority"
        );
        assert_eq!(
            restored.account_group_access(group).unwrap().role,
            AccountGroupRole::Unknown
        );
        owner
            .append_event(
                group,
                EventKind::Message,
                b"merge both complete original branches".to_vec(),
            )
            .unwrap();
    }
    members.truncate(3);
    owner
        .append_event(
            group,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Bounded account sources".into(),
                members,
                epoch: 3,
                owner: owner.user_id(),
            }
            .encode(),
        )
        .unwrap();
    owner.process_channel(group);
    let secrets = owner.identity.secret_bytes();
    drop(owner);
    let owner = node_with_identity(
        &dir.path().join("owner"),
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    let recovered = group_control_source_document(&owner, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(recovered.meta.clone(), recovered.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
}

#[test]
fn group_control_acknowledges_present_owner_decisions_that_lose_a_canonical_fork() {
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        discovery::Announce,
        eventlog::{Event, EventKind},
        node::AccountGroupRole,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let sibling = node(&dir.path().join("sibling"), &account);
    let foreign = node(&dir.path().join("foreign"), &Account::generate());
    let reference = node(&dir.path().join("reference"), &account);
    let group = new_channel_id();
    let genesis = Event::new(
        &owner.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Evaluated conflicting owner decisions".into(),
            members: vec![
                owner.identity.public(),
                sibling.identity.public(),
                foreign.identity.public(),
            ],
            epoch: 0,
            owner: owner.user_id(),
        }
        .encode(),
    );
    owner.log.lock().unwrap().append(genesis.clone()).unwrap();
    let old = (2..18)
        .map(|clock| {
            Event::new(
                &owner.identity,
                group,
                2,
                vec![genesis.id],
                2,
                clock,
                EventKind::ChannelOwnerTransfer,
                sibling.user_id().into_bytes(),
            )
        })
        .max_by_key(|event| event.id)
        .unwrap();
    owner.log.lock().unwrap().append(old.clone()).unwrap();
    owner.process_channel(group);
    owner.roster.lock().unwrap().update(
        &Announce::new_with_account(&sibling.identity, &account, "Actual owner adoption", 9),
        "127.0.0.1".parse().unwrap(),
        &owner.user_id(),
    );
    let prior = group_control_source_document(&owner, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(prior.meta.clone(), prior.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    sibling
        .account_documents
        .lock()
        .unwrap()
        .merge(prior.meta.clone(), prior.payload())
        .unwrap();
    let winning = (3..1024)
        .map(|clock| {
            Event::new(
                &owner.identity,
                group,
                3,
                vec![genesis.id],
                2,
                clock,
                EventKind::ChannelOwnerTransfer,
                foreign.user_id().into_bytes(),
            )
        })
        .find(|event| event.id < old.id)
        .expect("deterministic signed competing decision");
    owner.log.lock().unwrap().append(winning).unwrap();
    owner.process_channel(group);
    copy_public_group(&owner, &sibling, group);
    assert!(
        !sibling
            .account_group_projection(group)
            .authorized
            .contains(&old.id)
    );
    assert!(sibling.log.lock().unwrap().get(&old.id).is_some());
    sibling.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &foreign.identity,
            &foreign.account,
            "Foreign owner adoption",
            9,
        ),
        "127.0.0.1".parse().unwrap(),
        &sibling.user_id(),
    );
    let current = group_control_source_document(&sibling, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(current.meta.clone(), current.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member,
        "the full reducer evaluated and rejected the old decision; it is not missing and must not block exact-head supersession"
    );
}

#[test]
fn group_control_private_messages_and_reactions_do_not_republish_role_metadata() {
    use crate::eventlog::EventKind;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let member = node(&dir.path().join("member"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &member, "No private message churn");
    copy_public_group(&member, &owner, group);
    let prior = group_control_source_document(&owner, group);
    for kind in [EventKind::Message, EventKind::React, EventKind::Profile] {
        owner
            .append_event(group, kind, b"private payload".to_vec())
            .unwrap();
        owner.process_channel(group);
        assert_eq!(
            group_control_source_document(&owner, group).meta,
            prior.meta
        );
    }
}

fn assert_incomplete_publication_index_atomicity(owner: &Node, document: &AccountDocument) {
    use crate::node::account_group::wire::GroupControlHead;
    let signed =
        GroupControlHead::decode(document.payload(), &owner.account.public().ed25519_pub).unwrap();
    let group = signed.group;
    let frontier = owner
        .group_control_heads(group)
        .into_iter()
        .find(|head| head.proof.public() == owner.identity.public())
        .unwrap()
        .access
        .local_frontier;
    assert!(frontier.len() > 4096);
    let replacement = GroupControlHead::new(
        &owner.identity,
        group,
        document.meta.revision + 1,
        signed.proof.clone(),
        signed.access.clone(),
    );
    let payload = replacement.encode();
    let meta = AccountDocumentMeta::new(
        &owner.account,
        owner.user_id(),
        document.meta.key.clone(),
        replacement.revision,
        &payload,
        false,
    );
    {
        let mut store = owner.account_documents.lock().unwrap();
        store.publication_budget_for_test(0);
        assert!(
            store
                .merge_local_group_control(meta.clone(), &payload, frontier.clone())
                .is_err()
        );
        assert!(store.meta(&meta.key).as_ref() == Some(&document.meta));
        assert!(
            store
                .local_publication_frontier(&document.meta, &owner.identity.public())
                .as_ref()
                == Some(&frontier)
        );
        store.publication_budget_for_test(32 * 1024 * 1024);
        store.fail_index_for_test(true);
        assert!(
            store
                .merge_local_group_control(meta.clone(), &payload, frontier.clone())
                .is_err()
        );
        assert!(store.meta(&meta.key).as_ref() == Some(&document.meta));
        assert!(
            store
                .local_publication_frontier(&document.meta, &owner.identity.public())
                .as_ref()
                == Some(&frontier)
        );
        store.fail_index_for_test(false);
        store.compact_next_for_test();
        assert!(
            store
                .merge_local_group_control(meta.clone(), &payload, frontier.clone())
                .unwrap()
        );
        assert!(
            store
                .local_publication_frontier(&meta, &owner.identity.public())
                .as_ref()
                == Some(&frontier)
        );
        let mut wrong_source = owner.identity.public();
        wrong_source.x25519_pub[0] ^= 1;
        assert!(
            store
                .local_publication_frontier(&meta, &wrong_source)
                .is_none()
        );
        assert!(
            store
                .local_publication_frontier(&document.meta, &owner.identity.public())
                .is_none()
        );
    }
    // The ordinary remote ingestion path receives only signed wire bytes; it
    // cannot borrow the local source's receipt from this publication.
    let dir = tempfile::tempdir().unwrap();
    let mut remote =
        store::AccountDocumentStore::open(dir.path(), "pw", &owner.account.public().ed25519_pub)
            .unwrap();
    remote.merge(meta.clone(), &payload).unwrap();
    assert!(
        remote
            .local_publication_frontier(&meta, &owner.identity.public())
            .is_none()
    );
    let wire_header = serde_json::to_vec(&meta).unwrap();
    assert!(
        !wire_header
            .windows(b"_local_group_publication".len())
            .any(|part| part == b"_local_group_publication")
    );
}

#[test]
fn group_control_more_than_4096_concurrent_authorization_controls_do_not_freeze_owner() {
    use crate::{
        eventlog::{Event, EventKind},
        node::AccountGroupRole,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let reference = node(&dir.path().join("reference"), &account);
    let group = group_inventory_legacy_fixture(&owner, &external, "Large authorization frontier");
    copy_public_group(&external, &owner, group);
    let parents = owner.log.lock().unwrap().heads(&group);
    for seq in 2..=4098 {
        let event = Event::new(
            &owner.identity,
            group,
            seq,
            parents.clone(),
            2,
            seq,
            EventKind::ChannelOwnerTransfer,
            owner.user_id().into_bytes(),
        );
        owner.log.lock().unwrap().append(event).unwrap();
    }
    owner.process_channel(group);
    let prior = group_control_source_document(&owner, group);
    let signed = crate::node::account_group::wire::GroupControlHead::decode(
        prior.payload(),
        &account.public().ed25519_pub,
    )
    .unwrap();
    assert_eq!(
        signed.reference().content_hash,
        prior.meta.content_hash,
        "exact-head references reuse the document owner's stored payload hash domain"
    );
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(prior.meta.clone(), prior.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Owner
    );
    assert_incomplete_publication_index_atomicity(&owner, &prior);
    let secrets = owner.identity.secret_bytes();
    drop(owner);
    let owner = node_with_identity(
        &dir.path().join("owner"),
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    owner
        .account_documents
        .lock()
        .unwrap()
        .publication_budget_for_test(0);
    owner
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(group);
    let current = group_control_source_document(&owner, group);
    assert!(
        current.meta.revision > prior.meta.revision,
        "a bounded wire frontier must not freeze an obsolete Owner after a real transfer"
    );
    assert!(
        owner
            .account_documents
            .lock()
            .unwrap()
            .local_publication_frontier(&current.meta, &owner.identity.public())
            .is_none(),
        "a complete replacement atomically releases the old required receipt even at zero spare budget"
    );
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(current.meta.clone(), current.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
}

#[test]
fn group_control_unavailable_obsolete_cache_cannot_orphan_a_large_negative_head() {
    group_control_obsolete_cache_recovery_case(true);
}

#[test]
fn group_control_broken_obsolete_cache_does_not_downgrade_a_large_current_view() {
    group_control_obsolete_cache_recovery_case(false);
}

fn group_control_obsolete_cache_recovery_case(force_negative: bool) {
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        eventlog::{Event, EventKind},
        node::AccountGroupRole,
    };
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let secrets = owner.identity.secret_bytes();
    drop(owner);
    // A damaged obsolete cache must not prevent profile opening, nor become a
    // second durability owner for the signed head's recovery requirements.
    for entry in std::fs::read_dir(dir.path().join("owner")).unwrap() {
        let entry = entry.unwrap();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("group-publications-")
        {
            std::fs::write(entry.path(), b"damaged obsolete publication cache").unwrap();
        }
    }
    let owner = node_with_identity(
        &dir.path().join("owner"),
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    let external = node(&dir.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &external, "Atomic negative recovery");
    let mut members = vec![owner.identity.public(), external.identity.public()];
    for _ in 0..(if force_negative { 66 } else { 0 }) {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &account, "Synthetic certified member", 9);
        owner
            .roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &owner.user_id());
        members.push(device.public());
    }
    owner
        .append_event(
            group,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Atomic negative recovery".into(),
                members,
                epoch: 1,
                owner: owner.user_id(),
            }
            .encode(),
        )
        .unwrap();
    let parents = owner.log.lock().unwrap().heads(&group);
    for seq in 3..=4099 {
        owner
            .log
            .lock()
            .unwrap()
            .append(Event::new(
                &owner.identity,
                group,
                seq,
                parents.clone(),
                3,
                seq,
                EventKind::ChannelOwnerTransfer,
                owner.user_id().into_bytes(),
            ))
            .unwrap();
    }
    owner.process_channel(group);
    let negative = group_control_source_document(&owner, group);
    let head = crate::node::account_group::wire::GroupControlHead::decode(
        negative.payload(),
        &account.public().ed25519_pub,
    )
    .unwrap();
    assert_eq!(head.access.unavailable, force_negative);
    assert!(!head.access.complete_frontier);
    // Both the resource pressure and the causal frontier change. Recovery must
    // retain exact knowledge of every old branch, including across restart.
    owner
        .append_event(
            group,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Atomic negative recovery".into(),
                members: vec![owner.identity.public(), external.identity.public()],
                epoch: 2,
                owner: owner.user_id(),
            }
            .encode(),
        )
        .unwrap();
    owner
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(group);
    let secrets = owner.identity.secret_bytes();
    drop(owner);
    let owner = node_with_identity(
        &dir.path().join("owner"),
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    let current = group_control_source_document(&owner, group);
    assert!(
        current.meta.revision > negative.meta.revision,
        "a current incomplete negative head must retain an atomic recovery receipt"
    );
    let head = crate::node::account_group::wire::GroupControlHead::decode(
        current.payload(),
        &account.public().ed25519_pub,
    )
    .unwrap();
    assert!(!head.access.unavailable);
    assert_eq!(head.access.observation.role, AccountGroupRole::Member);
}

#[test]
fn group_control_normal_authorization_history_growth_keeps_bounded_current_heads() {
    use crate::{channel::ChannelMeta, eventlog::EventKind, node::AccountGroupRole};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let reference = node(&dir.path().join("reference"), &account);
    let group = group_inventory_legacy_fixture(&owner, &external, "Long lived authorization");
    copy_public_group(&external, &owner, group);
    let prior = group_control_source_document(&owner, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(prior.meta.clone(), prior.payload())
        .unwrap();
    for epoch in 1..=4096 {
        owner
            .append_event(
                group,
                EventKind::MembershipChange,
                ChannelMeta {
                    name: "Long lived authorization".into(),
                    members: vec![owner.identity.public(), external.identity.public()],
                    epoch,
                    owner: owner.user_id(),
                }
                .encode(),
            )
            .unwrap();
    }
    owner
        .append_event(
            group,
            EventKind::ChannelOwnerTransfer,
            external.user_id().into_bytes(),
        )
        .unwrap();
    owner.process_channel(group);
    assert_eq!(
        owner.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    let current = group_control_source_document(&owner, group);
    assert!(
        current.meta.revision > prior.meta.revision,
        "ordinary authorization history growth must not freeze the previous Owner head"
    );
    assert!(
        current.payload().len() < 128 * 1024,
        "wire head remains bounded by current sources and anchors"
    );
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(current.meta.clone(), current.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert_eq!(
        owner
            .account_documents(AccountDocumentDomain::GroupControl, None, 64)
            .unwrap()
            .items
            .len(),
        1
    );
}

#[tokio::test]
async fn group_control_removal_and_reinvite_never_reuse_the_old_membership_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let source = node(&dir.path().join("source"), &account);
    let reference = node(&dir.path().join("reference"), &account);
    let group = group_inventory_legacy_fixture(&owner, &source, "Fresh lifetime");
    copy_public_group(&source, &owner, group);
    copy_public_group(&source, &reference, group);
    let original = group_control_source_document(&source, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(original.meta.clone(), original.payload())
        .unwrap();
    let prior = source.account_group_access(group).unwrap().sources[0]
        .lifetime
        .clone();
    owner
        .remove_channel_member(group, &source.user_id())
        .await
        .unwrap();
    copy_public_group(&owner, &reference, group);
    assert!(
        reference
            .account_group_access(group)
            .unwrap()
            .sources
            .is_empty()
    );
    owner
        .add_channel_member(group, source.identity.public())
        .await
        .unwrap();
    copy_public_group(&owner, &source, group);
    source.accept_channel_invite(group).await.unwrap();
    copy_public_group(&source, &reference, group);
    let current = source.account_group_access(group).unwrap().sources[0]
        .lifetime
        .clone();
    assert_ne!(current.membership, prior.membership);
    assert!(current.acceptance.is_some());
    assert!(
        reference
            .account_group_access(group)
            .unwrap()
            .sources
            .iter()
            .all(|observation| observation.lifetime != prior)
    );
    let refreshed = group_control_source_document(&source, group);
    reference
        .account_documents
        .lock()
        .unwrap()
        .merge(refreshed.meta.clone(), refreshed.payload())
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().sources[0].lifetime,
        current
    );
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
    // This retained v4 fixture deliberately constructs two descriptive writers.
    // Current source-authenticated directory bodies cannot be re-signed by a sibling.
    let mut value: serde_json::Value = serde_json::from_slice(first.payload()).unwrap();
    value.as_object_mut().unwrap().remove("directory");
    let legacy_payload = serde_json::to_vec(&value).unwrap();
    let legacy_meta = AccountDocumentMeta::new(
        &account,
        a.user_id(),
        first.meta.key.clone(),
        first.meta.revision,
        &legacy_payload,
        false,
    );
    target
        .account_documents
        .lock()
        .unwrap()
        .merge(legacy_meta, &legacy_payload)
        .unwrap();
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
    for _ in 0..source.list_channels().len() {
        source.maintain_account_channels(&mut maintenance).await;
    }
    assert!(source.channel_owner_account_id(modern).is_some());
    let current_sibling = node(&dir.path().join("current-sibling"), &current);
    let invited_modern = source
        .create_channel("Current account invited", vec![])
        .await
        .unwrap();
    for _ in 0..source.list_channels().len() {
        source.maintain_account_channels(&mut maintenance).await;
    }
    source
        .try_account_channel_invite(
            invited_modern,
            &current_sibling.identity.public(),
            &current_sibling.own_group_proof(),
        )
        .unwrap();
    copy_public_group(&source, &current_sibling, invited_modern);
    let old_document = group_inventory_source_document(&source, legacy);
    source.prepare_account_transition(&current).unwrap();
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
    assert_eq!(
        adopted.channel_previous_account(modern),
        Some(previous.account_id()),
        "old modern grants remain a previous-account archive after device adoption"
    );
    assert_eq!(
        adopted.channel_previous_account(legacy),
        None,
        "a real legacy device membership survives account adoption"
    );
    assert_eq!(
        adopted.channel_previous_account(invited_modern),
        None,
        "a current-account invitation addressed to a certified sibling takes precedence"
    );
    current_sibling
        .accept_channel_invite(invited_modern)
        .await
        .unwrap();
    copy_public_group(&current_sibling, &adopted, invited_modern);
    assert_eq!(
        adopted.channel_previous_account(invited_modern),
        None,
        "an accepted current-account grant on a certified sibling takes precedence"
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
    let current_modern = adopted
        .create_channel("Current root modern", vec![])
        .await
        .unwrap();
    let mut current_maintenance =
        crate::node::channel_account_maintenance::ChannelAccountMaintenance::default();
    for _ in 0..adopted.list_channels().len() {
        adopted
            .maintain_account_channels(&mut current_maintenance)
            .await;
    }
    assert_eq!(
        adopted.channel_owner_account_id(current_modern),
        Some(current.account_id())
    );
    assert_eq!(adopted.channel_previous_account(current_modern), None);
    let sibling = node(&dir.path().join("sibling"), &current);
    sibling
        .account_documents
        .lock()
        .unwrap()
        .merge(new_document.meta.clone(), new_document.payload())
        .unwrap();
    assert!(sibling.channel_shared_reference(legacy).is_some());
    assert!(!sibling.channel_can_send(legacy));
    assert!(!sibling.channel_can_manage(legacy));
    assert!(sibling.channel_members(legacy).is_empty());
}

#[tokio::test]
async fn group_inventory_v4_cooldown_preserves_fallback_while_v5_interruptions_retry() {
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
                        observed
                            .lock()
                            .unwrap()
                            .push((request.domain, request.version));
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
        for expected in [5, 4] {
            assert!(
                target
                    .pull_account_documents_page(&peer, &mut cursor)
                    .await
                    .is_err()
            );
            assert_eq!(
                versions.lock().unwrap().last().map(|(_, version)| *version),
                Some(expected)
            );
        }
        assert!(cursor.status(target.device_sync_policy()).2);
        // An interrupted v5 request has a short retry interval; unlike the
        // rejected v4 inventory domain it is not an authenticated downgrade.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let mut hostname_rejections = 0;
        for _ in 0..16 {
            if let Err(error) = target.pull_account_documents_page(&peer, &mut cursor).await {
                let (domain, version) = *versions.lock().unwrap().last().unwrap();
                assert_eq!(version, 5);
                if domain == AccountDocumentDomain::DeviceHostname {
                    hostname_rejections += 1;
                }
                assert!(matches!(
                    domain,
                    AccountDocumentDomain::DeviceHostname | AccountDocumentDomain::GroupControl
                ));
                assert!(
                    matches!(error,
                        NodeError::Session(crate::node::session::SessionError::Transport(
                            crate::transport::TransportError::Io(error)
                        )) if error.kind() == std::io::ErrorKind::UnexpectedEof
                    ),
                    "only the fixture's deliberate v5 transport closure is retryable"
                );
            }
        }
        assert_eq!(
            hostname_rejections, 1,
            "unsupported hostname metadata has its own cooldown"
        );
        let requests = versions.lock().unwrap().clone();
        assert_eq!(
            requests
                .iter()
                .filter(|(domain, _)| *domain == AccountDocumentDomain::DeviceHostname)
                .count(),
            if optional_off { 1 } else { 2 },
            "hostname metadata retains its independent long cooldown and ordinary pooled-session retry"
        );
        assert!(
            requests
                .iter()
                .filter(|(domain, _)| *domain == AccountDocumentDomain::GroupControl)
                .count()
                >= 2,
            "the separate group-control transport interruption must retry promptly"
        );
        let versions: Vec<_> = requests.into_iter().map(|(_, version)| version).collect();
        assert_eq!(
            versions.iter().filter(|version| **version == 4).count(),
            1,
            "optional rejection has its own negative cooldown"
        );
        assert!(
            versions.iter().filter(|version| **version == 5).count() >= 2,
            "a closed v5 transport must be retried without a five-minute upgrade claim"
        );
        if optional_off {
            assert!(
                versions.iter().all(|version| matches!(version, 4 | 5)),
                "disabled optional domains cannot be attempted after selector wraps"
            );
            assert!(
                !cursor.up_to_date,
                "interrupted mandatory state must not claim synchronization is complete"
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
    let rows = target.list_channels();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let reference = target.channel_shared_reference(id).unwrap();
    assert_eq!(row.name, reference.name);
    assert_eq!(row.member_count, reference.member_count);
    assert_eq!(row.owner, reference.owner);
    assert!(row.archived);
    // A real local book remains the authority for the native/UI projection.
    let event = source.log.lock().unwrap().events(&id)[0].clone();
    target.log.lock().unwrap().append(event).unwrap();
    // The target is absent from this legacy event, so merely retaining that event
    // must not install membership or make the reference writable.
    target.process_channel(id);
    assert!(!target.channel_can_send(id));
    assert!(target.channel_members(id).is_empty());
    assert!(source.channel_shared_reference(id).is_none());
    assert!(!source.list_channels()[0].archived);
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
    assert!(!source.channel_can_send(id));
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
        target.list_channels()[0].archived
            && target.list_channels()[0].left
            && target.list_channels()[0].dissolved
    );
    assert!(target.channel_members(id).is_empty());
    assert!(!target.channel_can_send(id));
    assert!(!target.channel_can_manage(id));
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
    assert_eq!(source.list_channels()[0].member_count, 2);
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
    assert_eq!(target.list_channels()[0].member_count, 2);
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
        let count = source.list_channels()[0].member_count;
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
        assert_eq!(target.list_channels()[0].member_count, count);
        assert!(
            !target
                .channel_shared_reference(id)
                .unwrap()
                .members_complete
        );
        assert!(!target.channel_can_send(id));
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
        operation: None,
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
