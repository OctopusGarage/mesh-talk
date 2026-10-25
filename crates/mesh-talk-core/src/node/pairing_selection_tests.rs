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
