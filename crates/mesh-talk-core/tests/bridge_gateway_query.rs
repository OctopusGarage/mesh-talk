//! The gateway accepts only a fresh signed proof for the pinned relay.

use mesh_talk_core::discovery::Announce;
use mesh_talk_core::discovery::query::{decode_query, encode_response};
use mesh_talk_core::identity::account::Account;
use mesh_talk_core::identity::device::DeviceIdentity;
use mesh_talk_core::postoffice::gateway::query_pinned_relay;
use tokio::net::UdpSocket;

#[tokio::test]
async fn pinned_gateway_query_accepts_only_the_expected_relay() {
    let relay = DeviceIdentity::generate();
    let id = relay.user_id();
    let account = Account::generate();
    let proof = Announce::new_post_office_with_account(&relay, &account, "F", 47475);
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let endpoint = socket.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut buf = [0; 2048];
        for _ in 0..2 {
            let (len, source) = socket.recv_from(&mut buf).await.unwrap();
            let nonce = decode_query(&buf[..len]).unwrap();
            socket
                .send_to(&encode_response(nonce, &proof, &relay).unwrap(), source)
                .await
                .unwrap();
        }
    });

    let found = query_pinned_relay(endpoint, &id).await.unwrap();
    assert_eq!(found.user_id, id);
    assert!(
        query_pinned_relay(endpoint, &"0".repeat(32))
            .await
            .is_none()
    );
    server.await.unwrap();
}
