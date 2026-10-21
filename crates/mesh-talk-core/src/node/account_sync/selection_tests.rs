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

fn reusable_sibling_peer(
    directory: &std::path::Path,
    receiver: &Node,
    source: &Node,
    address: std::net::SocketAddr,
) -> PeerRecord {
    let announce = source.signed_announce("source", address.port());
    source
        .configure_privacy(
            &directory.join("source"),
            "pw",
            &announce,
            Arc::new(crate::discovery::visibility::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    receiver
        .roster
        .lock()
        .unwrap()
        .update(&announce, address.ip(), &receiver.user_id());
    receiver.routing_peer(&source.user_id()).unwrap()
}

// Old implementations close on the optional tag before accepting the original
// one-shot request. Keep downgrade fixtures explicit about this extra probe.
pub(super) async fn accept_legacy_account_request(
    listener: &tokio::net::TcpListener,
    node: &Arc<Node>,
) -> (SecureChannel<TcpStream>, Vec<u8>) {
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = node.privacy_accept(stream).await.unwrap();
        let bytes = channel.recv().await.unwrap();
        if matches!(
            AccountHistoryFrame::decode_versioned(&bytes),
            Some((_, AccountHistoryFrame::SessionRequest { .. }))
        ) {
            continue;
        }
        return (channel, bytes);
    }
}

// A malformed data fixture must complete capability negotiation first so the
// receiver still reaches the history/attachment assertion under test.
async fn accept_account_test_request(
    listener: &tokio::net::TcpListener,
    node: &Arc<Node>,
) -> (
    SecureChannel<TcpStream>,
    AccountSyncVersion,
    AccountHistoryFrame,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut channel = node.privacy_accept(stream).await.unwrap();
    let bytes = channel.recv().await.unwrap();
    let (version, frame) = AccountHistoryFrame::decode_versioned(&bytes).unwrap();
    let frame = match frame {
        AccountHistoryFrame::SessionRequest {
            request_id,
            request,
        } => {
            send_frame(
                &mut channel,
                version,
                &AccountHistoryFrame::SessionReady { request_id },
            )
            .await
            .unwrap();
            request.into_frame()
        }
        frame => frame,
    };
    (channel, version, frame)
}

#[tokio::test]
async fn account_history_pages_reuse_one_authenticated_connection() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    let (dir, source, receiver) = sibling_nodes();
    source
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    receiver
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    let contents = b"reusable sibling attachment";
    let path = dir.path().join("reuse.bin");
    std::fs::write(&path, contents).unwrap();
    let (_, file) = source
        .enqueue_file_to_account(&source.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = reusable_sibling_peer(
        dir.path(),
        &receiver,
        &source,
        listener.local_addr().unwrap(),
    );
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_accepted = accepted.clone();
    let server = tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_accepted.fetch_add(1, Ordering::SeqCst);
            let source = source.clone();
            sessions.spawn(async move {
                let channel = source.privacy_accept(stream).await.unwrap();
                source.serve_connection(channel).await;
            });
        }
    });
    for _ in 0..3 {
        let mut rejected = false;
        receiver
            .pull_account_history_version(
                &peer,
                None,
                [0; 32],
                AccountSyncVersion::V2,
                &mut rejected,
            )
            .await
            .unwrap();
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    receiver.send_account_wake(&peer, 1).await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    receiver
        .pull_account_attachment(&peer, file, AccountSyncVersion::V2)
        .await
        .unwrap();
    let saved = dir.path().join("received-reuse.bin");
    receiver.save_directory_archive(file, &saved).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), contents);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn account_legacy_v2_rejects_one_session_probe_then_keeps_one_shot_requests() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    let (_dir, source, receiver) = sibling_nodes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_accepted = accepted.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_accepted.fetch_add(1, Ordering::SeqCst);
            let mut channel = source.privacy_accept(stream).await.unwrap();
            let bytes = channel.recv().await.unwrap();
            match AccountHistoryFrame::decode_versioned(&bytes).unwrap() {
                (_, AccountHistoryFrame::SessionRequest { .. }) => {}
                (
                    version,
                    AccountHistoryFrame::Request {
                        account_id,
                        cursor,
                        known_digest,
                        wanted,
                        ..
                    },
                ) => {
                    assert_eq!(version, AccountSyncVersion::V2);
                    source
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
                _ => panic!("unexpected legacy account request"),
            }
        }
    });
    for _ in 0..3 {
        let (_, _, version) = receiver
            .pull_account_history_page(&peer, None, [0; 32])
            .await
            .unwrap();
        assert_eq!(version, AccountSyncVersion::V2);
    }
    server.abort();
    let _ = server.await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        4,
        "one optional probe plus three legacy requests"
    );
}

#[tokio::test]
async fn account_session_ready_errors_never_retry_a_legacy_request() {
    use tokio::net::TcpListener;
    for response in ["malformed", "wrong nonce", "ready then eof"] {
        let (_dir, source, receiver) = sibling_nodes();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = source.privacy_accept(stream).await.unwrap();
            let bytes = channel.recv().await.unwrap();
            let Some((version, AccountHistoryFrame::SessionRequest { mut request_id, .. })) =
                AccountHistoryFrame::decode_versioned(&bytes)
            else {
                panic!("expected optional session request");
            };
            if response == "malformed" {
                channel
                    .send(b"invalid account session reply")
                    .await
                    .unwrap();
            } else {
                if response == "wrong nonce" {
                    request_id[0] ^= 1;
                    if request_id == [0; 16] {
                        request_id[1] = 1;
                    }
                }
                send_frame(
                    &mut channel,
                    version,
                    &AccountHistoryFrame::SessionReady { request_id },
                )
                .await
                .unwrap();
            }
            drop(channel);
            assert!(
                tokio::time::timeout(Duration::from_millis(300), listener.accept())
                    .await
                    .is_err(),
                "a malformed, mismatched, or acknowledged session failure must not trigger a legacy retry"
            );
        });
        assert!(
            receiver
                .pull_account_history_page(&peer, None, [0; 32])
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn account_and_sync_reuse_separate_authenticated_connections() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    let (dir, source, receiver) = sibling_nodes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = reusable_sibling_peer(
        dir.path(),
        &receiver,
        &source,
        listener.local_addr().unwrap(),
    );
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_accepted = accepted.clone();
    let server = tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_accepted.fetch_add(1, Ordering::SeqCst);
            let source = source.clone();
            sessions.spawn(async move {
                let channel = source.privacy_accept(stream).await.unwrap();
                source.serve_connection(channel).await;
            });
        }
    });
    receiver
        .pull_account_history_page(&peer, None, [0; 32])
        .await
        .unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    let conversation =
        crate::node::conversation::dm_conversation_id(&receiver.identity.public(), &peer.public);
    receiver.deliver_direct(&peer, conversation).await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
    receiver
        .pull_account_history_page(&peer, None, [0; 32])
        .await
        .unwrap();
    receiver.deliver_direct(&peer, conversation).await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
    server.abort();
    let _ = server.await;
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
            let (mut channel, first) =
                accept_legacy_account_request(&listener, &server_source).await;
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
                let (mut channel, bytes) =
                    accept_legacy_account_request(&listener, &server_source).await;
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
            let (mut channel, _, request) = accept_account_test_request(&listener, &serving).await;
            assert!(matches!(
                request,
                AccountHistoryFrame::AttachmentRequest { .. }
            ));
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

#[tokio::test]
async fn account_sync_pending_channel_requester_receives_no_archive_or_file_bytes() {
    use tokio::net::TcpListener;
    let (dir, source, receiver) = sibling_nodes();
    source
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    sibling_peer(&source, &receiver, "127.0.0.1:9".parse().unwrap());
    let group = source
        .create_channel_invited("private", vec![receiver.identity.public()])
        .await
        .unwrap();
    source
        .send_channel_message(group, b"accepted members only")
        .await
        .unwrap();
    let path = dir.path().join("private.txt");
    std::fs::write(&path, b"private file bytes").unwrap();
    let file = source
        .send_file_channel(group, &path, crate::file::FileKind::File)
        .await
        .unwrap();
    source
        .save_directory_archive(file, &dir.path().join("retained.txt"))
        .unwrap();
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    for accepted in [false, true] {
        if accepted {
            source
                .channels
                .lock()
                .unwrap()
                .state_mut(&group)
                .unwrap()
                .accept_invite(&receiver.user_id(), EventId::new([71; 32]));
        }
        let mut io = receiver
            .privacy_dial(peer.addr, &peer.public)
            .await
            .unwrap();
        send_frame(
            &mut io,
            AccountSyncVersion::V2,
            &AccountHistoryFrame::Request {
                version: 2,
                account_id: source.account_id(),
                cursor: None,
                known_digest: [0; 32],
                wanted: DeviceSyncPolicy {
                    attachments: true,
                    ..DeviceSyncPolicy::default()
                },
            },
        )
        .await
        .unwrap();
        let mut saw_channel = false;
        let mut saw_file = false;
        loop {
            let bytes = tokio::time::timeout(Duration::from_secs(3), io.recv())
                .await
                .unwrap()
                .unwrap();
            match AccountHistoryFrame::decode_versioned(&bytes).unwrap().1 {
                AccountHistoryFrame::ChannelHeader { .. } => saw_channel = true,
                AccountHistoryFrame::FileCardHeader { .. } => saw_file = true,
                AccountHistoryFrame::Complete { .. } => break,
                _ => {}
            }
        }
        assert_eq!(
            saw_channel, accepted,
            "pending invitation must not disclose channel plaintext"
        );
        assert_eq!(
            saw_file, accepted,
            "pending invitation must not disclose file cards"
        );
        assert_eq!(
            source
                .attachment_for_sibling(file, &receiver.identity.public())
                .is_some(),
            accepted,
            "attachment serving requires accepted requester membership"
        );
    }
    server.abort();
    let _ = server.await;
}

#[test]
fn pairing_export_stops_at_record_budget_before_building_the_remaining_archive() {
    let (_dir, source, _) = sibling_nodes();
    let own = source.account_id();
    let conv = account_conversation_id(&own, &own);
    let mut sent = source.sentlog.lock().unwrap();
    for index in 0..4100u64 {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&index.to_be_bytes());
        let wire = DmEnvelope::new(own.clone(), own.clone(), id, b"bounded".to_vec()).encode();
        sent.record(conv, index, index, &wire).unwrap();
    }
    drop(sent);
    assert_eq!(source.export_account_backfill().len(), 4096);
}

#[tokio::test]
async fn account_history_worker_drains_continuation_without_waiting_retry_interval() {
    use tokio::net::TcpListener;
    let (_dir, source, receiver) = sibling_nodes();
    for index in 0..40 {
        source
            .enqueue_to_account(
                &source.account_id(),
                format!("history-{index}").as_bytes(),
                None,
            )
            .await
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    let worker = tokio::spawn(receiver.clone().run_account_history_loop());
    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if receiver.account_history(&receiver.account_id(), 100).len() == 40 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    worker.abort();
    server.abort();
    let _ = worker.await;
    let _ = server.await;
    outcome.expect("backlog drains promptly instead of waiting ten seconds per page");
}

#[test]
fn growing_archive_continuation_keeps_progress_without_claiming_up_to_date() {
    let mut state = PeerCursor::default();
    assert!(!state.complete_page(
        Some(HistoryCursor {
            tag: 0,
            id: [1; 32]
        }),
        [1; 32]
    ));
    assert!(!state.complete_page(
        Some(HistoryCursor {
            tag: 0,
            id: [2; 32]
        }),
        [2; 32]
    ));
    assert_eq!(state.next.unwrap().id, [2; 32]);
    // Finishing a changed sweep schedules a fresh sweep to recover lower new IDs.
    assert!(!state.complete_page(None, [3; 32]));
    assert!(state.next.is_none());
    assert_eq!(state.known_digest, [0; 32]);
    assert!(state.complete_page(None, [3; 32]));
}

#[tokio::test]
async fn archive_selectors_reuse_pages_and_scan_only_appends_but_invalidate_erasure() {
    let (_dir, source, receiver) = sibling_nodes();
    for index in 0..40 {
        source
            .enqueue_to_account(
                &source.account_id(),
                format!("row-{index}").as_bytes(),
                None,
            )
            .await
            .unwrap();
    }
    let public = receiver.identity.public();
    let (_, next, _) =
        source.export_sync_records(&public, None, true, false, AccountSyncVersion::V2);
    let scanned = source.account_archive_cache.lock().unwrap().scanned_rows;
    source.export_sync_records(&public, next, true, false, AccountSyncVersion::V2);
    {
        let cache = source.account_archive_cache.lock().unwrap();
        assert_eq!(cache.builds, 1, "another page uses the same metadata index");
        assert_eq!(cache.scanned_rows, scanned);
    }
    source
        .enqueue_to_account(&source.account_id(), b"one append", None)
        .await
        .unwrap();
    source.export_sync_records(&public, None, true, false, AccountSyncVersion::V2);
    let appended = source.account_archive_cache.lock().unwrap().scanned_rows;
    assert!(
        appended <= scanned + 2,
        "appending must not decode the retained archive again"
    );
    source
        .sentlog
        .lock()
        .unwrap()
        .remove_where(|_| true)
        .unwrap();
    let (records, next, _) =
        source.export_sync_records(&public, None, true, false, AccountSyncVersion::V2);
    assert!(
        records.is_empty(),
        "erasure invalidates index positions and old payload selectors"
    );
    assert!(next.is_none());
}

#[test]
fn known_account_index_skips_unchanged_group_rows_and_invalidates_erased_routes() {
    let (_dir, source, receiver) = sibling_nodes();
    let conversation = ConversationId::new([77; 32]);
    let record = |index: u64, plaintext: &[u8]| {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&index.to_le_bytes());
        source
            .received
            .lock()
            .unwrap()
            .record(
                conversation,
                source.user_id(),
                index,
                plaintext,
                EventId::new(id),
            )
            .unwrap();
    };
    for index in 0..500 {
        record(index, b"group plaintext");
    }
    source.known_account_ids();
    let scanned = source.known_account_cache.lock().unwrap().scanned_rows;
    assert_eq!(scanned, 500);
    for _ in 0..3 {
        source.export_sync_records(
            &receiver.identity.public(),
            None,
            true,
            true,
            AccountSyncVersion::V2,
        );
    }
    assert_eq!(
        source.known_account_cache.lock().unwrap().scanned_rows,
        scanned
    );
    record(500, b"one group append");
    source.export_sync_records(
        &receiver.identity.public(),
        None,
        true,
        true,
        AccountSyncVersion::V2,
    );
    assert_eq!(
        source.known_account_cache.lock().unwrap().scanned_rows,
        scanned + 1
    );
    let other = "a".repeat(32);
    let envelope = super::super::DmEnvelope::new(
        source.account_id(),
        other.clone(),
        [11; 32],
        b"body".to_vec(),
    )
    .encode();
    record(501, &envelope);
    assert!(source.known_account_ids().contains(&other));
    source
        .received
        .lock()
        .unwrap()
        .remove_where(|entry| entry.wall_clock == 501)
        .unwrap();
    assert!(
        !source.known_account_ids().contains(&other),
        "erased first-DM metadata must not retain authority"
    );
    assert_eq!(
        source.known_account_cache.lock().unwrap().scanned_rows,
        scanned + 503
    );
}

#[tokio::test]
async fn stalled_account_attachment_does_not_block_history_and_policy_off_cancels_it() {
    use tokio::net::TcpListener;
    let (dir, source, receiver) = sibling_nodes();
    for node in [&source, &receiver] {
        node.set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
    }
    let path = dir.path().join("stalled.txt");
    std::fs::write(&path, b"retained bytes").unwrap();
    source
        .enqueue_file_to_account(&source.account_id(), &path, crate::file::FileKind::File)
        .await
        .unwrap();
    let (cards, _, _) = source.export_sync_records(
        &receiver.identity.public(),
        None,
        false,
        true,
        AccountSyncVersion::V2,
    );
    receiver
        .apply_account_history_page(&cards, AccountSyncVersion::V2)
        .unwrap();
    for index in 0..40 {
        source
            .enqueue_to_account(
                &source.account_id(),
                format!("history-{index}").as_bytes(),
                None,
            )
            .await
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let server_source = source.clone();
    let server_started = started.clone();
    let server_cancelled = cancelled.clone();
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let source = server_source.clone(); let started = server_started.clone(); let cancelled = server_cancelled.clone();
                    connections.spawn(async move {
                        let mut io = source.privacy_accept(stream).await.unwrap();
                        let bytes = io.recv().await.unwrap();
                        let (version, request) = AccountHistoryFrame::decode_versioned(&bytes).unwrap();
                        match request {
                            AccountHistoryFrame::SessionRequest { .. } => {},
                            AccountHistoryFrame::Request { account_id, cursor, known_digest, wanted, .. } => {
                                source.serve_account_history(&mut io, version, &account_id, cursor, known_digest, wanted).await;
                            }
                            AccountHistoryFrame::AttachmentRequest { .. } => {
                                started.notify_one();
                                // No reply: only receiver cancellation can close this channel promptly.
                                assert!(io.recv().await.is_err());
                                cancelled.notify_one();
                            }
                            _ => panic!("unexpected sibling request"),
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let worker = tokio::spawn(receiver.clone().run_account_history_loop());
    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
        started.notified().await;
        loop {
            if receiver.account_history(&receiver.account_id(), 100).len() >= 41 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        receiver
            .set_device_sync_category(SyncCategory::Attachments, false)
            .unwrap();
        cancelled.notified().await;
    })
    .await;
    worker.abort();
    server.abort();
    let _ = worker.await;
    let _ = server.await;
    outcome.expect(
        "history stays live and disabling attachments cancels its pending network exchange",
    );
}

#[tokio::test]
async fn archive_authorization_transition_rebuilds_cached_membership_selection() {
    let (_dir, source, receiver) = sibling_nodes();
    let channel = source
        .create_channel_invited("private", vec![receiver.identity.public()])
        .await
        .unwrap();
    source
        .send_channel_message(channel, b"accepted only")
        .await
        .unwrap();
    let public = receiver.identity.public();
    let (rows, _, _) =
        source.export_sync_records(&public, None, true, false, AccountSyncVersion::V2);
    assert!(rows.is_empty());
    source
        .channels
        .lock()
        .unwrap()
        .state_mut(&channel)
        .unwrap()
        .accept_invite(&receiver.user_id(), EventId::new([91; 32]));
    let (rows, _, _) =
        source.export_sync_records(&public, None, true, false, AccountSyncVersion::V2);
    assert_eq!(rows.len(), 1);
    assert_eq!(source.account_archive_cache.lock().unwrap().builds, 2);
}

#[tokio::test]
async fn account_history_worker_reaches_original_archive_while_source_keeps_appending() {
    use tokio::net::TcpListener;
    let (_dir, source, receiver) = sibling_nodes();
    for index in 0..96 {
        source
            .enqueue_to_account(
                &source.account_id(),
                format!("original-{index}").as_bytes(),
                None,
            )
            .await
            .unwrap();
    }
    let original: HashSet<_> = source
        .account_history(&source.account_id(), 200)
        .into_iter()
        .map(|row| row.id)
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&receiver, &source, listener.local_addr().unwrap());
    let pages = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let served_pages = pages.clone();
    let server_source = source.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut io, bytes) = accept_legacy_account_request(&listener, &server_source).await;
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
                panic!("expected history request");
            };
            server_source
                .serve_account_history(&mut io, version, &account_id, cursor, known_digest, wanted)
                .await;
            let index = served_pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            server_source
                .enqueue_to_account(
                    &server_source.account_id(),
                    format!("concurrent-{index}").as_bytes(),
                    None,
                )
                .await
                .unwrap();
        }
    });
    let worker = tokio::spawn(receiver.clone().run_account_history_loop());
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let imported: HashSet<_> = receiver
                .account_history(&receiver.account_id(), 20_000)
                .into_iter()
                .map(|row| row.id)
                .collect();
            if original.is_subset(&imported) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    worker.abort();
    server.abort();
    let _ = worker.await;
    let _ = server.await;
    assert!(
        pages.load(std::sync::atomic::Ordering::SeqCst) >= 3,
        "the source must change between at least three continuation pages"
    );
    outcome.expect("append-only source changes must preserve forward cursor progress");
}

#[tokio::test]
async fn account_preference_local_edit_wakes_sibling_before_periodic_retry() {
    use tokio::net::TcpListener;
    let (_dir, source, receiver) = sibling_nodes();
    for node in [&source, &receiver] {
        node.set_device_sync_category(SyncCategory::Appearance, true)
            .unwrap();
    }
    let source_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let receiver_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&receiver, &source, source_listener.local_addr().unwrap());
    sibling_peer(&source, &receiver, receiver_listener.local_addr().unwrap());
    let source_server = tokio::spawn(source.clone().run_accept_loop(source_listener));
    let receiver_server = tokio::spawn(receiver.clone().run_accept_loop(receiver_listener));
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if [&source, &receiver].iter().all(|node| {
                node.device_sync_status()
                    .iter()
                    .any(|status| status.phase == SyncPhase::UpToDate)
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        source
            .set_account_preference(
                SyncCategory::Appearance,
                "theme".into(),
                Some("dark".into()),
            )
            .unwrap();
        loop {
            if receiver
                .account_preferences(SyncCategory::Appearance)
                .iter()
                .any(|record| record.value.as_deref() == Some("dark"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    source_server.abort();
    receiver_server.abort();
    let _ = source_server.await;
    let _ = receiver_server.await;
    outcome.expect(
        "local preference publication must wake the certified sibling before its ten-second retry",
    );
}

#[tokio::test]
async fn account_wake_requires_exact_linked_noise_identity_and_deduplicates_revisions() {
    use tokio::net::TcpListener;
    let (dir, source, sibling) = sibling_nodes();
    let outsider = open_sibling(&dir.path().join("outsider"), &Account::generate());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_source = source.clone();
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            let io = server_source.privacy_accept(stream).await.unwrap();
            server_source.serve_connection(io).await;
        }
    });
    for (index, sender) in [&outsider, &sibling, &sibling].into_iter().enumerate() {
        let mut io = sender
            .privacy_dial(address, &source.identity.public())
            .await
            .unwrap();
        send_frame(
            &mut io,
            AccountSyncVersion::V2,
            &AccountHistoryFrame::Wake {
                account_id: source.account_id(),
                epoch: [7; 16],
                revision: 1,
            },
        )
        .await
        .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(2), io.recv())
            .await
            .unwrap();
        if index == 0 {
            assert!(
                reply.is_err(),
                "an unrelated Noise identity gets no wake acknowledgement"
            );
        } else {
            assert!(matches!(
                AccountHistoryFrame::decode_versioned(&reply.unwrap()),
                Some((
                    AccountSyncVersion::V2,
                    AccountHistoryFrame::WakeAck {
                        epoch,
                        revision: 1
                    }
                )) if epoch == [7; 16]
            ));
        }
        let notified = tokio::time::timeout(
            Duration::from_millis(30),
            source.account_sync_notify.notified(),
        )
        .await;
        assert_eq!(
            notified.is_ok(),
            index == 1,
            "only the first certified revision wakes a pull"
        );
    }
    server.await.unwrap();
}

#[tokio::test]
async fn legacy_sibling_rejects_optional_wake_once_and_periodic_history_still_repairs() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    let (_dir, source, legacy) = sibling_nodes();
    for node in [&source, &legacy] {
        node.set_device_sync_category(SyncCategory::Appearance, true)
            .unwrap();
    }
    let source_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let legacy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&legacy, &source, source_listener.local_addr().unwrap());
    sibling_peer(&source, &legacy, legacy_listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let legacy_attempts = attempts.clone();
    let legacy_server_node = legacy.clone();
    let legacy_server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = legacy_listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let node = legacy_server_node.clone(); let attempts = legacy_attempts.clone();
                    connections.spawn(async move {
                        let mut io = node.privacy_accept(stream).await.unwrap();
                        let bytes = io.recv().await.unwrap();
                        let (version, request) = AccountHistoryFrame::decode_versioned(&bytes).unwrap();
                        match request {
                            AccountHistoryFrame::SessionRequest { .. } => {},
                            AccountHistoryFrame::Wake { .. } => { attempts.fetch_add(1, Ordering::SeqCst); }
                            AccountHistoryFrame::Request { account_id, cursor, known_digest, wanted, .. } => {
                                node.serve_account_history(&mut io, version, &account_id, cursor, known_digest, wanted).await;
                            }
                            _ => panic!("unexpected legacy request"),
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let source_server = tokio::spawn(source.clone().run_accept_loop(source_listener));
    let legacy_worker = tokio::spawn(legacy.clone().run_account_history_loop());
    let outcome = tokio::time::timeout(Duration::from_secs(13), async {
        loop {
            if attempts.load(Ordering::SeqCst) == 1
                && legacy
                    .device_sync_status()
                    .iter()
                    .any(|status| status.phase == SyncPhase::UpToDate)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        for value in ["dark", "light", "oled"] {
            source
                .set_account_preference(
                    SyncCategory::Appearance,
                    "theme".into(),
                    Some(value.into()),
                )
                .unwrap();
        }
        loop {
            if legacy
                .account_preferences(SyncCategory::Appearance)
                .iter()
                .any(|record| record.value.as_deref() == Some("oled"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "unsupported peers stay in capability cooldown while ordinary pulls repair"
        );
    })
    .await;
    source_server.abort();
    legacy_server.abort();
    legacy_worker.abort();
    let _ = source_server.await;
    let _ = legacy_server.await;
    let _ = legacy_worker.await;
    outcome.expect(
        "a legacy sibling keeps its ten-second history fallback after rejecting optional hints",
    );
}

#[tokio::test]
async fn account_network_children_retain_retirement_admission_until_cancelled() {
    use tokio::net::TcpListener;
    for wake in [false, true] {
        let (_dir, local, remote) = sibling_nodes();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        sibling_peer(&local, &remote, listener.local_addr().unwrap());
        let (started, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut io = remote.privacy_accept(stream).await.unwrap();
            io.recv().await.unwrap();
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let worker = if wake {
            tokio::spawn(local.clone().run_account_wake_loop())
        } else {
            tokio::spawn(local.clone().run_account_history_loop())
        };
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        local.runtime_work.close();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), local.runtime_work.drain())
                .await
                .is_err(),
            "an admitted account network child must remain visible to retirement (wake={wake})"
        );
        worker.abort();
        let _ = worker.await;
        tokio::time::timeout(Duration::from_secs(2), local.runtime_work.drain())
            .await
            .unwrap();
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn account_wake_stays_quiet_with_all_sync_categories_off_then_wakes_on_enable() {
    use tokio::net::TcpListener;
    let (_dir, local, remote) = sibling_nodes();
    local
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&local, &remote, listener.local_addr().unwrap());
    let worker = tokio::spawn(local.clone().run_account_wake_loop());
    local
        .set_account_preference(
            SyncCategory::Appearance,
            "theme".into(),
            Some("dark".into()),
        )
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), listener.accept())
            .await
            .is_err()
    );
    local
        .set_device_sync_category(SyncCategory::Appearance, true)
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), listener.accept()).await;
    worker.abort();
    let _ = worker.await;
    assert!(
        result.is_ok(),
        "enabling a selected category advertises its retained state"
    );
}

#[tokio::test]
async fn account_wake_failed_route_cooldown_does_not_suppress_a_new_endpoint() {
    use tokio::net::TcpListener;
    let (_dir, local, remote) = sibling_nodes();
    let old_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&local, &remote, old_listener.local_addr().unwrap());
    let old_remote = remote.clone();
    let old_server = tokio::spawn(async move {
        let (stream, _) = old_listener.accept().await.unwrap();
        let mut io = old_remote.privacy_accept(stream).await.unwrap();
        io.recv().await.unwrap();
        // Legacy endpoint closes on the appended optional request tag.
    });
    let worker = tokio::spawn(local.clone().run_account_wake_loop());
    tokio::time::timeout(Duration::from_secs(2), old_server)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let new_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    sibling_peer(&local, &remote, new_listener.local_addr().unwrap());
    let outcome = tokio::time::timeout(Duration::from_secs(2), async {
        let (mut io, version, frame) = accept_account_test_request(&new_listener, &remote).await;
        let AccountHistoryFrame::Wake {
            account_id,
            epoch,
            revision,
        } = frame
        else {
            panic!("expected wake request");
        };
        remote
            .serve_account_wake(&mut io, version, &account_id, epoch, revision)
            .await;
    })
    .await;
    worker.abort();
    let _ = worker.await;
    outcome.expect("a changed authenticated route bypasses the old endpoint's capability cooldown");
}
