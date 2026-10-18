use super::*;
use crate::identity::account::Account;

fn open_sibling(dir: &std::path::Path, account: &Account) -> Arc<Node> {
    use crate::{discovery::Roster, identity::device::DeviceIdentity};
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        DeviceIdentity::generate(),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}

fn sibling_nodes() -> (tempfile::TempDir, Arc<Node>, Arc<Node>) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = open_sibling(&dir.path().join("source"), &account);
    let receiver = open_sibling(&dir.path().join("receiver"), &account);
    for (owner, other) in [(&source, &receiver), (&receiver, &source)] {
        owner
            .account_devices
            .lock()
            .unwrap()
            .insert(super::super::account_devices::AccountDevice {
                public: other.identity.public(),
                certificate: account.certify(&other.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    (dir, source, receiver)
}

fn sibling_peer(receiver: &Node, source: &Node, address: std::net::SocketAddr) -> PeerRecord {
    receiver.roster.lock().unwrap().update(
        &crate::discovery::Announce::new_with_account(
            &source.identity,
            &source.account,
            "source",
            address.port(),
        ),
        address.ip(),
        &receiver.user_id(),
    );
    receiver.routing_peer(&source.user_id()).unwrap()
}

#[tokio::test]
async fn account_sync_v2_falls_back_once_only_before_a_valid_reply() {
    use tokio::net::TcpListener;
    for valid_reply in [false, true] {
        let (_dir, source, receiver) = sibling_nodes();
        source
            .enqueue_to_account(&source.account_id(), b"legacy compatible history", None)
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
        let server_source = source.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = server_source.privacy_accept(stream).await.unwrap();
            let first = channel.recv().await.unwrap();
            let (version, _) = AccountHistoryFrame::decode_versioned(&first).unwrap();
            assert_eq!(version, AccountSyncVersion::V2);
            if valid_reply {
                let device = server_source
                    .linked_devices()
                    .into_iter()
                    .find(|device| device.public == server_source.identity.public())
                    .unwrap();
                send_frame(
                    &mut channel,
                    version,
                    &AccountHistoryFrame::Device { device },
                )
                .await
                .unwrap();
                drop(channel);
                assert!(
                    tokio::time::timeout(Duration::from_millis(300), listener.accept())
                        .await
                        .is_err(),
                    "a valid response forbids downgrade"
                );
            } else {
                drop(channel);
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = server_source.privacy_accept(stream).await.unwrap();
                let bytes = channel.recv().await.unwrap();
                let (
                    version,
                    AccountHistoryFrame::Request {
                        account_id,
                        cursor,
                        known_digest,
                        wanted,
                        ..
                    },
                ) = AccountHistoryFrame::decode_versioned(&bytes).unwrap()
                else {
                    panic!("request");
                };
                assert_eq!(version, AccountSyncVersion::V1);
                assert!(
                    !wanted.attachments,
                    "metadata fallback must not enable bytes"
                );
                server_source
                    .serve_account_history(
                        &mut channel,
                        version,
                        &account_id,
                        cursor,
                        known_digest,
                        wanted,
                    )
                    .await;
            }
        });
        let result = receiver
            .pull_account_history_page(&peer, None, [0; 32])
            .await;
        if valid_reply {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().2, AccountSyncVersion::V1);
            assert_eq!(
                receiver.account_history(&receiver.account_id(), 10).len(),
                1
            );
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn account_sync_v2_personal_file_metadata_and_bytes_have_independent_categories() {
    use tokio::net::TcpListener;
    for files_only in [false, true] {
        let (dir, source, receiver) = sibling_nodes();
        source
            .set_device_sync_category(SyncCategory::Attachments, files_only)
            .unwrap();
        source
            .set_device_sync_category(SyncCategory::History, !files_only)
            .unwrap();
        receiver
            .set_device_sync_category(SyncCategory::Attachments, files_only)
            .unwrap();
        receiver
            .set_device_sync_category(SyncCategory::History, !files_only)
            .unwrap();
        let contents = b"offline personal attachment with durable bytes";
        let path = dir.path().join("personal.bin");
        std::fs::write(&path, contents).unwrap();
        let (id, file) = source
            .enqueue_file_to_account(&source.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
        let server = tokio::spawn(source.clone().run_accept_loop(listener));
        for _ in 0..2 {
            let (_, _, version) = receiver
                .pull_account_history_page(&peer, None, [0; 32])
                .await
                .unwrap();
            assert_eq!(version, AccountSyncVersion::V2);
        }
        let history = receiver.account_history(&receiver.account_id(), 10);
        assert_eq!(
            history.len(),
            1,
            "repeated metadata import must deduplicate"
        );
        assert_eq!(history[0].id, id);
        assert!(history[0].file.is_some());
        assert!(
            !receiver.media.contains(file),
            "file card import alone must not copy bytes"
        );
        assert_eq!(receiver.file_progress(file).unwrap().done, 0);
        if files_only {
            receiver
                .pull_account_attachment(&peer, file, AccountSyncVersion::V2)
                .await
                .unwrap();
            let saved = dir.path().join("received.bin");
            receiver.save_directory_archive(file, &saved).unwrap();
            assert_eq!(std::fs::read(saved).unwrap(), contents);
        }
        // An old decoder must never receive the personal proof extension.
        let (legacy, _, _) = source.export_sync_records(
            &receiver.identity.public(),
            None,
            true,
            true,
            AccountSyncVersion::V1,
        );
        assert!(
            !legacy
                .iter()
                .any(|record| matches!(record, HistoryRecord::FileCard(_)))
        );
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn account_sync_personal_file_proof_rejects_changed_digest_host_author_and_signature() {
    use crate::identity::device::DeviceIdentity;
    let (dir, source, receiver) = sibling_nodes();
    let path = dir.path().join("proof.bin");
    std::fs::write(&path, b"proof binding").unwrap();
    source
        .enqueue_file_to_account(&source.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let (records, _, _) = source.export_sync_records(
        &receiver.identity.public(),
        None,
        true,
        true,
        AccountSyncVersion::V2,
    );
    let card = records
        .iter()
        .find_map(|record| {
            if let HistoryRecord::FileCard(card) = record {
                Some(card.clone())
            } else {
                None
            }
        })
        .unwrap();
    for defect in ["digest", "host", "author", "signature"] {
        let mut altered = card.clone();
        let mut payload: FileCardPayload = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .deserialize(&altered.payload)
            .unwrap();
        match defect {
            "host" => altered.host = [8; 32],
            "signature" => payload.proof.sig[0] ^= 1,
            _ => {
                let outsider = DeviceIdentity::generate();
                let signer = if defect == "author" {
                    &outsider
                } else {
                    &source.identity
                };
                let mut cipher = payload.proof.ciphertext.clone();
                if defect == "digest" {
                    cipher[6] ^= 1;
                }
                payload.proof = Event::new(
                    signer,
                    payload.proof.conversation_id,
                    payload.proof.seq,
                    payload.proof.parents.clone(),
                    payload.proof.lamport,
                    payload.proof.wall_clock,
                    crate::eventlog::EventKind::FileManifest,
                    cipher,
                );
                altered.event_id = *payload.proof.id.as_bytes();
                altered.from = payload.proof.author.user_id();
            }
        }
        altered.payload = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&payload)
            .unwrap();
        assert!(
            receiver
                .import_account_file_card(&altered, AccountSyncVersion::V2)
                .is_err(),
            "accepted invalid {defect}"
        );
        assert!(
            receiver
                .account_history(&receiver.account_id(), 10)
                .is_empty()
        );
    }
    receiver
        .import_account_file_card(&card, AccountSyncVersion::V2)
        .unwrap();
    assert_eq!(
        receiver.account_history(&receiver.account_id(), 10).len(),
        1
    );
}

#[tokio::test]
async fn account_sync_attachment_receiver_rejects_off_on_revision_before_commit() {
    use tokio::net::TcpListener;
    for scenario in ["aba", "clear", "interrupted"] {
        let (dir, source, receiver) = sibling_nodes();
        source
            .set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
        receiver
            .set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
        let bytes = b"stale attachment stream";
        let path = dir.path().join("stale.bin");
        std::fs::write(&path, bytes).unwrap();
        let (_, file) = source
            .enqueue_file_to_account(&source.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let (records, _, _) = source.export_sync_records(
            &receiver.identity.public(),
            None,
            true,
            true,
            AccountSyncVersion::V2,
        );
        receiver
            .apply_account_history_page(&records, AccountSyncVersion::V2)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
        let serving = source.clone();
        let changing = receiver.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = serving.privacy_accept(stream).await.unwrap();
            channel.recv().await.unwrap();
            let checksum: [u8; 32] = Sha256::digest(bytes).into();
            send_frame(
                &mut channel,
                AccountSyncVersion::V2,
                &AccountHistoryFrame::AttachmentStart {
                    file_conv: *file.as_bytes(),
                    name: "stale.bin".into(),
                    size: bytes.len() as u64,
                    sha256: checksum,
                },
            )
            .await
            .unwrap();
            send_frame(
                &mut channel,
                AccountSyncVersion::V2,
                &AccountHistoryFrame::AttachmentChunk {
                    bytes: bytes.to_vec(),
                },
            )
            .await
            .unwrap();
            match scenario {
                "aba" => {
                    changing
                        .set_device_sync_category(SyncCategory::Attachments, false)
                        .unwrap();
                    changing
                        .set_device_sync_category(SyncCategory::Attachments, true)
                        .unwrap();
                }
                "clear" => {
                    changing
                        .clear_account_conversation(&changing.account_id())
                        .unwrap();
                }
                _ => return,
            }
            send_frame(
                &mut channel,
                AccountSyncVersion::V2,
                &AccountHistoryFrame::AttachmentComplete,
            )
            .await
            .unwrap();
        });
        assert!(
            receiver
                .pull_account_attachment(&peer, file, AccountSyncVersion::V2)
                .await
                .is_err()
        );
        assert!(!receiver.media.contains(file));
        server.await.unwrap();
        if scenario == "clear" {
            receiver
                .apply_account_history_page(&records, AccountSyncVersion::V2)
                .unwrap();
            assert!(
                receiver
                    .account_history(&receiver.account_id(), 10)
                    .is_empty(),
                "backfill must not resurrect a cleared card"
            );
        }
    }
}

#[tokio::test]
async fn account_sync_stale_source_revision_never_sends_header_chunk_or_complete() {
    use tokio::net::TcpListener;
    let (_dir, source, receiver) = sibling_nodes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = serving.privacy_accept(stream).await.unwrap();
        let revision = serving.device_sync_policy_snapshot().1;
        serving
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        serving
            .set_device_sync_category(SyncCategory::History, true)
            .unwrap();
        for frame in [
            AccountHistoryFrame::RecordHeader {
                conversation: [1; 32],
                from: "a".repeat(32),
                wall_clock: 1,
                event_id: [2; 32],
                total_len: 1,
            },
            AccountHistoryFrame::RecordChunk { bytes: vec![1] },
            AccountHistoryFrame::Complete {
                next_cursor: None,
                digest: [0; 32],
            },
            AccountHistoryFrame::AttachmentComplete,
        ] {
            assert!(
                serving
                    .send_account_frame(&mut channel, AccountSyncVersion::V2, revision, &frame)
                    .await
                    .is_err()
            );
        }
    });
    let mut channel = receiver
        .privacy_dial(address, &source.identity.public())
        .await
        .unwrap();
    assert!(channel.recv().await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn account_sync_history_revision_changes_only_after_new_durable_import() {
    let (_dir, source, receiver) = sibling_nodes();
    source
        .enqueue_to_account(&source.account_id(), b"visible durable import", None)
        .await
        .unwrap();
    let (records, _, _) = source.export_sync_records(
        &receiver.identity.public(),
        None,
        true,
        false,
        AccountSyncVersion::V2,
    );
    let initial = receiver.account_history_revision();
    receiver
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    assert!(
        receiver
            .apply_account_history_page(&records, AccountSyncVersion::V2)
            .is_err()
    );
    assert_eq!(receiver.account_history_revision(), initial);
    receiver
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    receiver
        .apply_account_history_page(&records, AccountSyncVersion::V2)
        .unwrap();
    let imported = receiver.account_history_revision();
    assert_ne!(
        imported, initial,
        "new durable history must invalidate the UI projection"
    );
    receiver
        .apply_account_history_page(&records, AccountSyncVersion::V2)
        .unwrap();
    assert_eq!(
        receiver.account_history_revision(),
        imported,
        "duplicate imports must not cause repeated UI reloads"
    );
}

#[tokio::test]
async fn account_sync_third_sibling_uses_durable_copy_after_original_goes_offline() {
    use tokio::net::TcpListener;
    let (dir, source, middle) = sibling_nodes();
    source
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    middle
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    let bytes = b"verified sibling copy remains available independently";
    let path = dir.path().join("three.bin");
    std::fs::write(&path, bytes).unwrap();
    let (id, file) = source
        .enqueue_file_to_account(&source.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&middle, &source, listener.local_addr().unwrap());
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    middle
        .pull_account_history_page(&peer, None, [0; 32])
        .await
        .unwrap();
    middle
        .pull_account_attachment(&peer, file, AccountSyncVersion::V2)
        .await
        .unwrap();
    server.abort();
    let _ = server.await;
    let third = open_sibling(&dir.path().join("third"), &source.account);
    for (owner, other) in [(&middle, &third), (&third, &middle)] {
        owner
            .account_devices
            .lock()
            .unwrap()
            .insert(super::super::account_devices::AccountDevice {
                public: other.identity.public(),
                certificate: owner.account.certify(&other.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&third, &middle, listener.local_addr().unwrap());
    let server = tokio::spawn(middle.clone().run_accept_loop(listener));
    for _ in 0..2 {
        third
            .pull_account_history_page(&peer, None, [0; 32])
            .await
            .unwrap();
    }
    let history = third.account_history(&third.account_id(), 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert!(!third.media.contains(file));
    third
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    third
        .pull_account_attachment(&peer, file, AccountSyncVersion::V2)
        .await
        .unwrap();
    let output = dir.path().join("third-output.bin");
    third.save_directory_archive(file, &output).unwrap();
    assert_eq!(std::fs::read(output).unwrap(), bytes);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn account_sync_legacy_sibling_file_offer_cannot_bypass_bytes_off() {
    let (dir, source, receiver) = sibling_nodes();
    let address = "127.0.0.1:9".parse().unwrap();
    sibling_peer(&source, &receiver, address);
    sibling_peer(&receiver, &source, address);
    let path = dir.path().join("legacy.bin");
    std::fs::write(&path, b"legacy sibling bytes").unwrap();
    let (id, file) = source
        .enqueue_file_dm(&receiver.user_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let event = source
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .event
        .clone();
    // Reconstruct a manifest accepted by an older build, before category controls.
    let plain = crate::dm::open(
        &receiver.identity,
        &source.identity.public().x25519_pub,
        &event.ciphertext,
    )
    .unwrap();
    let manifest = super::super::files::validated_manifest(&plain).unwrap();
    receiver
        .log
        .lock()
        .unwrap()
        .append_durable(event.clone())
        .unwrap();
    receiver
        .files
        .lock()
        .unwrap()
        .record_event(event.id, event.author, manifest);
    assert_eq!(
        receiver.attachment_consent_for_peer(file, id, &source.identity.public()),
        super::super::attachment_consent::ReceiveDecision::Pending
    );
    assert!(!receiver.attachment_chunk_author_allowed(file, &source.identity.public().ed25519_pub));
    receiver
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    assert_eq!(
        receiver.attachment_consent_for_peer(file, id, &source.identity.public()),
        super::super::attachment_consent::ReceiveDecision::Accepted
    );
    assert!(receiver.attachment_chunk_author_allowed(file, &source.identity.public().ed25519_pub));
}

#[tokio::test]
async fn account_sync_legacy_sibling_raw_file_export_respects_attachment_category() {
    use crate::eventlog::sync::SyncStore;
    let (dir, source, receiver) = sibling_nodes();
    sibling_peer(&source, &receiver, "127.0.0.1:9".parse().unwrap());
    let path = dir.path().join("raw-legacy.bin");
    std::fs::write(&path, b"raw legacy sibling bytes").unwrap();
    let (id, _) = source
        .enqueue_file_dm(&receiver.user_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let event = source
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .event
        .clone();
    let store = source.serve_sync_store(&receiver.identity.public());
    let visible = || {
        store
            .lock()
            .unwrap()
            .events_excluding(&event.conversation_id, &HashSet::new())
    };
    assert!(
        visible().is_empty(),
        "raw legacy file metadata must not bypass bytes-off; V2 metadata has its own negotiated path"
    );
    source
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    source
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    assert!(visible().iter().any(|row| row.id == event.id));
}

#[tokio::test]
async fn account_sync_sibling_profile_remains_live_when_history_ancestor_is_paused() {
    use tokio::net::TcpListener;
    let (_dir, source, receiver) = sibling_nodes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&source, &receiver, listener.local_addr().unwrap());
    sibling_peer(&receiver, &source, "127.0.0.1:9".parse().unwrap());
    source
        .enqueue_to_account(&source.account_id(), b"paused ancestor", None)
        .await
        .unwrap();
    source
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    receiver
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let avatar = b"data:image/png;base64,QQ==".to_vec();
    let payload =
        super::super::profile::ProfilePayload::new(&source.account, Some(avatar.clone()), 42);
    let conv = super::super::conversation::dm_conversation_id(
        &source.identity.public(),
        &receiver.identity.public(),
    );
    let sealed = crate::dm::seal(
        &source.identity,
        &receiver.identity.public().x25519_pub,
        &payload.encode(),
    )
    .unwrap();
    source
        .append_event(conv, crate::eventlog::EventKind::Profile, sealed)
        .unwrap();
    let profile = source
        .log
        .lock()
        .unwrap()
        .events(&conv)
        .into_iter()
        .find(|event| event.kind == crate::eventlog::EventKind::Profile)
        .unwrap()
        .clone();
    assert!(
        profile.parents.is_empty(),
        "sibling profile leaves must not depend on paused content"
    );
    let server = tokio::spawn(receiver.clone().run_accept_loop(listener));
    source.deliver_direct(&peer, conv).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while receiver.peer_avatar(&receiver.account_id()).as_ref() != Some(&avatar) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        receiver
            .account_history(&receiver.account_id(), 10)
            .is_empty()
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn account_sync_restart_replays_retained_sibling_cipher_without_discovery() {
    use crate::{discovery::Roster, identity::device::DeviceIdentity};
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    let (dir, source, receiver) = sibling_nodes();
    sibling_peer(&source, &receiver, "127.0.0.1:9".parse().unwrap());
    sibling_peer(&receiver, &source, "127.0.0.1:9".parse().unwrap());
    let profile = dir.path().join("receiver");
    receiver
        .configure_privacy(
            &profile,
            "pw",
            &receiver.signed_announce("receiver", 9),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    let identity = receiver.identity.secret_bytes();
    let account = receiver.account.secret_bytes();
    receiver
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let id = source
        .enqueue_to_account(
            &source.account_id(),
            b"retained across receiver restart",
            None,
        )
        .await
        .unwrap();
    let event = source
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .event
        .clone();
    receiver
        .log
        .lock()
        .unwrap()
        .append_durable(event.clone())
        .unwrap();
    receiver.emit_new_messages(event.conversation_id);
    assert!(
        receiver
            .account_history(&receiver.account_id(), 10)
            .is_empty()
    );
    drop(receiver);
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let receiver = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(identity.0, identity.1),
        Account::from_secret_bytes(account),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &profile.join("events.log"),
        &profile.join("sent.log"),
        "pw",
    )
    .unwrap();
    receiver
        .configure_privacy(
            &profile,
            "pw",
            &receiver.signed_announce("receiver", 9),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    assert!(
        receiver
            .routing_peers()
            .iter()
            .all(|peer| !peer.has_lan_route())
    );
    receiver
        .set_device_sync_category(SyncCategory::History, true)
        .unwrap();
    assert_eq!(
        receiver.account_history(&receiver.account_id(), 10).len(),
        1
    );
}
