use super::*;
use crate::discovery::Roster;
use crate::identity::device::DeviceIdentity;
use crate::node::SyncCategory;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

fn open(dir: &std::path::Path, name: &str) -> Arc<Node> {
    let (d, _) = mpsc::unbounded_channel();
    let (c, _) = mpsc::unbounded_channel();
    let (f, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        DeviceIdentity::generate(),
        Account::generate(),
        Arc::new(Mutex::new(Roster::default())),
        d,
        c,
        f,
        &dir.join(format!("{name}.log")),
        &dir.join(format!("{name}-sent.log")),
        "pw",
    )
    .unwrap()
}

async fn pairing_without_backfill_case(wanted: bool, source_enabled: bool, version: u8) {
    let dir = tempfile::tempdir().unwrap();
    let linker = open(dir.path(), "linker");
    let joiner = open(dir.path(), "joiner");
    linker
        .set_device_sync_category(SyncCategory::History, source_enabled)
        .unwrap();
    let contact = Account::generate();
    let conv = super::super::conversation::account_conversation_id(
        &linker.account_id(),
        &contact.account_id(),
    );
    let envelope = DmEnvelope::new(
        contact.account_id(),
        linker.account_id(),
        [13; 32],
        b"history".to_vec(),
    );
    linker
        .received
        .lock()
        .unwrap()
        .record(
            conv,
            contact.account_id(),
            1,
            &envelope.encode(),
            EventId::new([13; 32]),
        )
        .unwrap();
    assert_eq!(linker.export_account_backfill().len(), 1);
    let code = PairingCode::from_hex(&linker.start_linking()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = Arc::clone(&linker);
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &serving.identity)
            .await
            .unwrap();
        let bytes = channel.recv().await.unwrap();
        // Dispatch exactly as production does, preserving the authenticated choice.
        let (request, selected, metadata) = PairingRequest::decode_negotiated(&bytes).unwrap();
        serving
            .serve_pairing_negotiated(&mut channel, request, selected, metadata)
            .await;
    });
    let mut channel = SecureChannel::connect(
        TcpStream::connect(addr).await.unwrap(),
        &joiner.identity,
        Some(&linker.identity.public()),
    )
    .await
    .unwrap();
    let request = PairingRequest {
        joiner: joiner.identity.public(),
        tag: match version {
            1 => code.authenticator(
                &linker.identity.public().ed25519_pub,
                &joiner.identity.public().ed25519_pub,
            ),
            2 => code.authenticator_selected(
                &linker.identity.public().ed25519_pub,
                &joiner.identity.public().ed25519_pub,
                wanted,
            ),
            _ => code.authenticator_metadata(
                &linker.identity.public().ed25519_pub,
                &joiner.identity.public().ed25519_pub,
                wanted,
            ),
        },
    };
    let bytes = match version {
        1 => request.encode(),
        2 => request.encode_selected(wanted),
        _ => request.encode_metadata(wanted),
    };
    channel.send(&bytes).await.unwrap();
    let response = channel.recv().await.unwrap();
    let created_at_ms = if version == 3 {
        PairingResponse::decode_metadata(&response).unwrap().1
    } else {
        assert!(PairingResponse::decode(&response).is_some());
        linker.account.created_at_ms() // legacy framing deliberately contains no timestamp
    };
    assert_eq!(created_at_ms, linker.account.created_at_ms());
    assert!(
        channel.recv().await.unwrap().is_empty(),
        "no history bytes may cross the channel"
    );
    task.await.unwrap();
    assert_eq!(linker.linked_devices().len(), 2);
}

#[tokio::test]
async fn pairing_no_history_selection_sends_only_response_and_terminator() {
    pairing_without_backfill_case(false, true, 3).await;
}

#[tokio::test]
async fn pairing_source_history_off_sends_only_response_and_terminator() {
    pairing_without_backfill_case(true, false, 3).await;
}

async fn legacy_peer_case(history: bool, valid_response: bool) {
    let dir = tempfile::tempdir().unwrap();
    let linker = open(dir.path(), "legacy");
    let joiner = open(dir.path(), "joiner");
    joiner
        .set_device_sync_category(SyncCategory::History, history)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let public = linker.identity.public();
    let expected_secret = linker.account.secret_bytes();
    let code_hex = linker.start_linking();
    let client_code = code_hex.clone();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &linker.identity)
            .await
            .unwrap();
        let bytes = channel.recv().await.unwrap();
        assert!(
            bytes.starts_with(b"MTPQ3"),
            "upgraded initiator first requests V3"
        );
        if valid_response {
            let response = PairingResponse {
                account_secret: linker.account.secret_bytes(),
                account_ed25519_pub: linker.account.public().ed25519_pub,
                cert: linker.account.certify(&channel.peer_identity().ed25519_pub),
            };
            channel
                .send(&response.encode_metadata(linker.account.created_at_ms()))
                .await
                .unwrap();
        }
        drop(channel); // an older peer closes on the unknown V3 frame
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err(),
            "no extra enrollment connection"
        );
    });
    let result = joiner.link_to_device(addr, &public, &client_code).await;
    if valid_response {
        assert_eq!(result.unwrap().secret, expected_secret);
    } else {
        assert!(result.err().unwrap().to_string().contains("upgrade"));
    }
    task.await.unwrap();
}

#[tokio::test]
async fn pairing_legacy_peer_requires_upgrade_without_metadata_losing_retry() {
    legacy_peer_case(true, false).await;
}
#[tokio::test]
async fn pairing_strict_no_history_requires_upgrade_without_retry() {
    legacy_peer_case(false, false).await;
}
#[tokio::test]
async fn pairing_valid_response_then_disconnect_never_retries() {
    legacy_peer_case(true, true).await;
}

#[tokio::test]
async fn pairing_rejects_a_metadata_less_response_without_disabling_history_policy() {
    let dir = tempfile::tempdir().unwrap();
    let linker = open(dir.path(), "v2-linker");
    let joiner = open(dir.path(), "v3-joiner");
    joiner
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let code = linker.start_linking();
    let public = linker.identity.public();
    let original_account = joiner.account_id();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &linker.identity)
            .await
            .unwrap();
        assert!(channel.recv().await.unwrap().starts_with(b"MTPQ3"));
        let response = PairingResponse {
            account_secret: linker.account.secret_bytes(),
            account_ed25519_pub: linker.account.public().ed25519_pub,
            cert: linker.account.certify(&channel.peer_identity().ed25519_pub),
        };
        channel.send(&response.encode()).await.unwrap();
        drop(channel);
        assert!(
            tokio::time::timeout(Duration::from_millis(250), listener.accept())
                .await
                .is_err()
        );
    });
    assert!(joiner.link_to_device(addr, &public, &code).await.is_err());
    assert_eq!(joiner.account_id(), original_account);
    assert!(!joiner.device_sync_policy().history);
    task.await.unwrap();
}

#[tokio::test]
async fn pairing_transport_failure_never_downgrades_or_discards_creation_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let linker = open(dir.path(), "metadata-linker");
    let joiner = open(dir.path(), "metadata-joiner");
    let original_account = joiner.account_id();
    let original_created_at_ms = joiner.account.created_at_ms();
    let source_created_at_ms = linker.account.created_at_ms();
    assert!(source_created_at_ms.is_some());
    let code = linker.start_linking();
    let public = linker.identity.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = Arc::clone(&linker);
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let channel = SecureChannel::accept(socket, &serving.identity)
            .await
            .unwrap();
        drop(channel); // transient disconnect before the V3 request is handled
        assert!(
            tokio::time::timeout(Duration::from_millis(250), listener.accept())
                .await
                .is_err(),
            "a transient disconnect must not open a metadata-losing legacy fallback"
        );
    });
    let result = joiner.link_to_device(addr, &public, &code).await;
    assert!(result.is_err());
    task.await.unwrap();
    assert_eq!(joiner.account_id(), original_account);
    assert_eq!(joiner.account.created_at_ms(), original_created_at_ms);
    assert_eq!(linker.account.created_at_ms(), source_created_at_ms);
    assert_eq!(linker.linked_devices().len(), 1);
}

#[tokio::test]
async fn legacy_pairing_request_frames_remain_accepted_by_metadata_capable_responders() {
    pairing_without_backfill_case(false, true, 2).await;
    pairing_without_backfill_case(true, false, 1).await;
}

#[tokio::test]
async fn pairing_connection_failure_preserves_network_error_with_history_off() {
    let dir = tempfile::tempdir().unwrap();
    let joiner = open(dir.path(), "joiner");
    joiner
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let peer = DeviceIdentity::generate().public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let error = joiner
        .link_to_device(address, &peer, &PairingCode::generate().as_hex())
        .await
        .err()
        .unwrap();
    assert!(
        matches!(error,
            NodeError::Linking(LinkingError::Connection(TransportError::Io(ref io)))
            if io.kind() == std::io::ErrorKind::ConnectionRefused
        ),
        "{error}"
    );
}

#[tokio::test]
async fn pairing_authentication_timeout_does_not_retry_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let joiner = open(dir.path(), "joiner");
    let peer = DeviceIdentity::generate().public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, connected) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        accepted.send(()).unwrap();
        std::future::pending::<()>().await;
        drop(socket);
    });
    let linking = tokio::spawn(async move {
        joiner
            .link_to_device(address, &peer, &PairingCode::generate().as_hex())
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), connected)
        .await
        .unwrap()
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(11)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let finished = linking.is_finished();
    serving.abort();
    if !finished {
        linking.abort();
    }
    assert!(
        finished,
        "a transport timeout must not start a second legacy dial"
    );
    let error = linking.await.unwrap().err().unwrap().to_string();
    assert!(error.contains("connect"), "{error}");
}

#[tokio::test]
async fn pairing_authenticated_response_wait_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let joiner = open(dir.path(), "joiner");
    let identity = DeviceIdentity::generate();
    let peer = identity.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (received, request_seen) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &identity).await.unwrap();
        assert!(channel.recv().await.unwrap().starts_with(b"MTPQ3"));
        received.send(()).unwrap();
        std::future::pending::<()>().await;
        drop(channel);
    });
    let linking = tokio::spawn(async move {
        joiner
            .link_to_device(address, &peer, &PairingCode::generate().as_hex())
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), request_seen)
        .await
        .unwrap()
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let finished = linking.is_finished();
    serving.abort();
    if !finished {
        linking.abort();
    }
    assert!(
        finished,
        "an authenticated peer must not hold pairing open indefinitely"
    );
    let error = linking.await.unwrap().err().unwrap().to_string();
    assert!(error.contains("pairing response timed out"), "{error}");
}

#[tokio::test]
async fn pairing_malformed_response_never_retries_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let joiner = open(dir.path(), "joiner");
    let identity = DeviceIdentity::generate();
    let peer = identity.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &identity).await.unwrap();
        channel.recv().await.unwrap();
        channel.send(b"invalid pairing response").await.unwrap();
        drop(channel);
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_ok()
    });
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        joiner.link_to_device(address, &peer, &PairingCode::generate().as_hex()),
    )
    .await;
    assert!(
        !serving.await.unwrap(),
        "malformed response must not authorize a legacy retry"
    );
    assert!(result.unwrap().is_err());
}

#[tokio::test]
async fn pairing_relay_only_presence_explains_direct_connection_requirement() {
    let dir = tempfile::tempdir().unwrap();
    let joiner = open(dir.path(), "joiner");
    let peer = DeviceIdentity::generate();
    let proof = crate::discovery::Announce::new_with_account(
        &peer,
        &Account::generate(),
        "relay-only",
        54371,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    joiner
        .roster
        .lock()
        .unwrap()
        .update_relayed(&proof, address, &joiner.user_id());
    let error = joiner
        .link_to_device(address, &peer.public(), &PairingCode::generate().as_hex())
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("direct connection"), "{error}");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn v4_requires_source_credentials_and_returns_signed_private_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let source = open(dir.path(), "source");
    let target = open(dir.path(), "target");
    let snapshot = source
        .publish_account_access_if(b"opaque credentials".to_vec(), |commit| commit())
        .unwrap();
    source
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let code = PairingCode::from_hex(&source.start_linking()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = source.clone();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &serving.identity)
            .await
            .unwrap();
        let bytes = channel.recv().await.unwrap();
        let (request, history) = PairingRequest::decode_access(&bytes).unwrap();
        serving
            .serve_pairing_access(&mut channel, request, history)
            .await;
    });
    let mut channel = SecureChannel::connect(
        TcpStream::connect(addr).await.unwrap(),
        &target.identity,
        Some(&source.identity.public()),
    )
    .await
    .unwrap();
    let request = PairingRequest {
        joiner: target.identity.public(),
        tag: code.authenticator_access(
            &source.identity.public().ed25519_pub,
            &target.identity.public().ed25519_pub,
            false,
        ),
    };
    channel.send(&request.encode_access(false)).await.unwrap();
    let response =
        super::super::pairing::PairingAccessResponse::decode(&channel.recv().await.unwrap())
            .unwrap();
    assert_eq!(response.access, snapshot);
    assert!(
        response
            .access
            .valid_for(&response.account.account_ed25519_pub)
    );
    assert!(channel.recv().await.unwrap().is_empty());
    task.await.unwrap();
}

#[tokio::test]
async fn v4_preflight_rejection_and_unsupported_peer_preserve_target_account() {
    for source_ready in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let source = open(&dir.path().join("source"), "source");
        let target = open(&dir.path().join("target"), "target");
        if source_ready {
            source
                .publish_account_access_if(b"credentials".to_vec(), |commit| commit())
                .unwrap();
        }
        let own = target.account_id();
        let code = source.start_linking();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = source.clone();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut channel = SecureChannel::accept(socket, &serving.identity)
                .await
                .unwrap();
            let bytes = channel.recv().await.unwrap();
            let (request, history) = PairingRequest::decode_access(&bytes).unwrap();
            serving
                .serve_pairing_access(&mut channel, request, history)
                .await;
        });
        let mut checked = false;
        let result = target
            .link_to_device_with_account_access_checked_if(
                addr,
                &source.identity.public(),
                &code,
                |_| {
                    checked = true;
                    Err(NodeError::Authorization("login collision".into()))
                },
                |commit| commit(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(checked, source_ready);
        assert_eq!(target.account_id(), own);
        assert!(
            target
                .account_access
                .lock()
                .unwrap()
                .current(&source.account_id())
                .is_none()
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn v4_wrong_code_never_releases_access_or_enrolls_joiner() {
    let dir = tempfile::tempdir().unwrap();
    let source = open(&dir.path().join("source"), "source");
    let target = open(&dir.path().join("target"), "target");
    source
        .publish_account_access_if(b"credentials".to_vec(), |commit| commit())
        .unwrap();
    source.start_linking();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(socket, &serving.identity)
            .await
            .unwrap();
        let bytes = channel.recv().await.unwrap();
        let (request, history) = PairingRequest::decode_access(&bytes).unwrap();
        serving
            .serve_pairing_access(&mut channel, request, history)
            .await;
    });
    let old = target.account_id();
    assert!(
        target
            .link_to_device_with_account_access_checked_if(
                address,
                &source.identity.public(),
                &PairingCode::generate().as_hex(),
                |_| Ok(()),
                |commit| commit()
            )
            .await
            .is_err()
    );
    task.await.unwrap();
    assert_eq!(target.account_id(), old);
    assert_eq!(source.linked_devices().len(), 1);
    assert!(target.account_access_snapshot().is_none());
}

#[tokio::test]
async fn v4_client_persists_source_account_record_without_relabeling_target_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let source = open(&dir.path().join("source"), "source");
    let target = open(&dir.path().join("target"), "target");
    let expected = source
        .publish_account_access_if(b"source credentials".to_vec(), |commit| commit())
        .unwrap();
    let old = target
        .publish_account_access_if(b"old target credentials".to_vec(), |commit| commit())
        .unwrap();
    target
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let code = source.start_linking();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = source.clone();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let channel = SecureChannel::accept(socket, &serving.identity)
            .await
            .unwrap();
        serving.serve_connection(channel).await;
    });
    let linked = target
        .link_to_device_with_account_access_checked_if(
            address,
            &source.identity.public(),
            &code,
            |snapshot| {
                assert_eq!(*snapshot, expected);
                Ok(())
            },
            |commit| commit(),
        )
        .await
        .unwrap();
    assert_eq!(linked.account_id, source.account_id());
    assert_eq!(target.account_access_snapshot(), Some(old));
    assert_eq!(
        target
            .account_access
            .lock()
            .unwrap()
            .current(&linked.account_id),
        Some(expected.clone())
    );
    task.await.unwrap();
    drop(target);
    let reopened = super::super::account_access::AccountAccessStore::open(
        &dir.path().join("target/account-access.log"),
        "pw",
    )
    .unwrap();
    assert_eq!(reopened.current(&linked.account_id), Some(expected));
}
