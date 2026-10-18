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

async fn pairing_without_backfill_case(wanted: bool, source_enabled: bool) {
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
        let (request, selected) = PairingRequest::decode_selected(&bytes).unwrap();
        serving
            .serve_pairing_selected(&mut channel, request, selected)
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
        tag: code.authenticator_selected(
            &linker.identity.public().ed25519_pub,
            &joiner.identity.public().ed25519_pub,
            wanted,
        ),
    };
    channel
        .send(&request.encode_selected(wanted))
        .await
        .unwrap();
    assert!(PairingResponse::decode(&channel.recv().await.unwrap()).is_some());
    assert!(
        channel.recv().await.unwrap().is_empty(),
        "no history bytes may cross the channel"
    );
    task.await.unwrap();
    assert_eq!(linker.linked_devices().len(), 2);
}

#[tokio::test]
async fn pairing_no_history_selection_sends_only_response_and_terminator() {
    pairing_without_backfill_case(false, true).await;
}

#[tokio::test]
async fn pairing_source_history_off_sends_only_response_and_terminator() {
    pairing_without_backfill_case(true, false).await;
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
            bytes.starts_with(b"MTPQ2"),
            "upgraded initiator first requests V2"
        );
        if valid_response {
            let response = PairingResponse {
                account_secret: linker.account.secret_bytes(),
                account_ed25519_pub: linker.account.public().ed25519_pub,
                cert: linker.account.certify(&channel.peer_identity().ed25519_pub),
            };
            channel.send(&response.encode()).await.unwrap();
        }
        drop(channel); // a legacy peer closes on the unknown V2 first frame
        if history && !valid_response {
            let (socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut channel = SecureChannel::accept(socket, &linker.identity)
                .await
                .unwrap();
            let request = PairingRequest::decode(&channel.recv().await.unwrap()).unwrap();
            assert!(PairingCode::from_hex(&code_hex).unwrap().verify(
                &linker.identity.public().ed25519_pub,
                &request.joiner.ed25519_pub,
                &request.tag
            ));
            linker.serve_pairing(&mut channel, request).await;
            assert_eq!(linker.linked_devices().len(), 2);
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err(),
            "no extra enrollment connection"
        );
    });
    let result = joiner.link_to_device(addr, &public, &client_code).await;
    if history || valid_response {
        assert_eq!(result.unwrap().secret, expected_secret);
    } else {
        assert!(result.err().unwrap().to_string().contains("upgrade"));
    }
    task.await.unwrap();
}

#[tokio::test]
async fn pairing_legacy_peer_allows_one_history_retry() {
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
