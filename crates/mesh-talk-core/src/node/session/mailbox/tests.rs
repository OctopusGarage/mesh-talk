use super::*;
use crate::identity::device::DeviceIdentity;
use crate::postoffice::{IndexedPostOffice, federation::IndexedRelayEndpoint};
use crate::storage::profile::ProfileStorageLease;

fn office(root: &tempfile::TempDir) -> IndexedPostOffice {
    IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &root.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap()
}

#[tokio::test]
async fn mailbox_rpc_negotiates_indexed_server_then_pages_gets_and_uploads() {
    let root = tempfile::tempdir().unwrap();
    let office = office(&root);
    let author = DeviceIdentity::generate();
    let recipient = DeviceIdentity::generate();
    let mailbox =
        crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public()).unwrap();
    let inner = Event::new(
        &author,
        ConversationId::new([4; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![7; 128],
    );
    let outer = mailbox.wrap(&inner).unwrap();
    let endpoint = IndexedRelayEndpoint::new(office, None, None);
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let channel = SecureChannel::accept(server, &server_identity)
            .await
            .unwrap();
        endpoint.serve_connection_checked(channel, None).await
    });
    let mut channel = SecureChannel::connect(client, &author, Some(&expected))
        .await
        .unwrap();
    assert!(negotiate(&mut channel, mailbox.id()).await.unwrap());
    let empty = delta(&mut channel, mailbox.id(), None).await.unwrap();
    assert!(empty.complete && empty.ids.is_empty());
    assert!(upload(&mut channel, outer.clone()).await.unwrap());
    let page = delta(&mut channel, mailbox.id(), Some(&empty.cursor))
        .await
        .unwrap();
    assert_eq!(page.ids, [outer.id]);
    assert!(page.complete);
    assert_eq!(
        get(&mut channel, mailbox.id(), &page.cursor, &[outer.id])
            .await
            .unwrap(),
        [outer]
    );
    let idle = delta(&mut channel, mailbox.id(), Some(&page.cursor))
        .await
        .unwrap();
    assert_eq!(idle.cursor, page.cursor);
    assert!(idle.ids.is_empty());
    drop(channel);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn mailbox_rpc_legacy_probe_keeps_same_connection_usable() {
    let root = tempfile::tempdir().unwrap();
    let store = Mutex::new(office(&root));
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let author = DeviceIdentity::generate();
    let conv = ConversationId::new([3; 32]);
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let mut channel = SecureChannel::accept(server, &server_identity)
            .await
            .unwrap();
        let budget = RemoteHaveBudget::default();
        for _ in 0..2 {
            assert!(matches!(
                serve_one_budgeted(&mut channel, &store, &budget)
                    .await
                    .unwrap(),
                Served::Handled(_)
            ));
        }
    });
    let mut channel = SecureChannel::connect(client, &author, Some(&expected))
        .await
        .unwrap();
    assert!(!negotiate(&mut channel, conv).await.unwrap());
    assert!(!negotiate(&mut channel, conv).await.unwrap());
    task.await.unwrap();
}

#[tokio::test]
async fn mailbox_rpc_malformed_probe_response_is_an_error() {
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let author = DeviceIdentity::generate();
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let mut channel = SecureChannel::accept(server, &server_identity)
            .await
            .unwrap();
        channel.recv().await.unwrap();
        channel.send(b"MTOM1broken").await.unwrap();
    });
    let mut channel = SecureChannel::connect(client, &author, Some(&expected))
        .await
        .unwrap();
    assert!(
        negotiate(&mut channel, ConversationId::new([3; 32]))
            .await
            .is_err()
    );
    drop(channel);
    task.await.unwrap();
}

#[test]
fn mailbox_rpc_codec_rejects_trailing_and_oversized_frames() {
    let mut frame = encode_frame(&Frame::Supported {
        conversation: ConversationId::new([8; 32]),
    })
    .unwrap();
    assert!(decode_frame(&frame).is_ok());
    frame.push(0);
    assert!(decode_frame(&frame).is_err());
    assert!(decode_frame(&vec![0; MAX_PLAINTEXT + 1]).is_err());
    let conv = ConversationId::new([1; 32]);
    assert_ne!(marker(conv), marker(ConversationId::new([2; 32])));
    let empty: [u8; 32] = Sha256::digest(0u64.to_le_bytes()).into();
    assert_ne!(marker(conv), empty);
}

#[tokio::test]
async fn mailbox_rpc_custom_requests_require_current_conversation_probe() {
    for probed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let endpoint = IndexedRelayEndpoint::new(office(&root), None, None);
        let server_identity = DeviceIdentity::generate();
        let expected = server_identity.public();
        let author = DeviceIdentity::generate();
        let (client, server) = tokio::io::duplex(256 * 1024);
        let task = tokio::spawn(async move {
            let channel = SecureChannel::accept(server, &server_identity)
                .await
                .unwrap();
            endpoint.serve_connection_checked(channel, None).await
        });
        let mut channel = SecureChannel::connect(client, &author, Some(&expected))
            .await
            .unwrap();
        if probed {
            assert!(
                negotiate(&mut channel, ConversationId::new([1; 32]))
                    .await
                    .unwrap()
            );
        }
        channel
            .send(
                &encode_frame(&Frame::Delta {
                    conversation: ConversationId::new([2; 32]),
                    prior: None,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(channel.recv().await.is_err());
        assert!(task.await.unwrap().is_err());
    }
}

#[tokio::test]
async fn mailbox_rpc_reprobe_changes_scope_on_same_channel() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = IndexedRelayEndpoint::new(office(&root), None, None);
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let author = DeviceIdentity::generate();
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let channel = SecureChannel::accept(server, &server_identity)
            .await
            .unwrap();
        endpoint.serve_connection_checked(channel, None).await
    });
    let mut channel = SecureChannel::connect(client, &author, Some(&expected))
        .await
        .unwrap();
    for byte in [1, 2] {
        let conv = ConversationId::new([byte; 32]);
        assert!(negotiate(&mut channel, conv).await.unwrap());
        assert!(delta(&mut channel, conv, None).await.unwrap().complete);
    }
    drop(channel);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn mailbox_rpc_failure_after_supported_does_not_downgrade() {
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let author = DeviceIdentity::generate();
    let conversation = ConversationId::new([3; 32]);
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let mut channel = SecureChannel::accept(server, &server_identity)
            .await
            .unwrap();
        channel.recv().await.unwrap();
        channel
            .send(&encode_frame(&Frame::Supported { conversation }).unwrap())
            .await
            .unwrap();
        channel.recv().await.unwrap();
        channel
            .send(&encode(&SyncWire::FpResponse(FpResponse { matched: false })).unwrap())
            .await
            .unwrap();
    });
    let mut channel = SecureChannel::connect(client, &author, Some(&expected))
        .await
        .unwrap();
    assert!(negotiate(&mut channel, conversation).await.unwrap());
    assert!(delta(&mut channel, conversation, None).await.is_err());
    task.await.unwrap();
}
