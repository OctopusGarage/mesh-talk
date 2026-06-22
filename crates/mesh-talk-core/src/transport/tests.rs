//! Module-level transport tests, including a real-TCP integration test.

use crate::identity::device::DeviceIdentity;
use crate::transport::{SecureChannel, NOISE_PARAMS};
use tokio::net::{TcpListener, TcpStream};

#[test]
fn noise_params_are_valid() {
    let parsed: Result<snow::params::NoiseParams, _> = NOISE_PARAMS.parse();
    assert!(parsed.is_ok(), "NOISE_PARAMS must be a valid Noise pattern");
}

#[tokio::test]
async fn secure_channel_over_real_tcp() {
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let (a_pub, b_pub) = (a.public(), b.public());

    // Bind an ephemeral localhost port for the responder.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        let (sock, _peer_addr) = listener.accept().await.expect("accept tcp");
        let mut ch = SecureChannel::accept(sock, &b)
            .await
            .expect("secure accept");
        assert_eq!(ch.peer_identity(), &a_pub);
        let msg = ch.recv().await.expect("recv");
        assert_eq!(msg, b"hello over tcp");
        ch.send(b"ack").await.expect("send");
    });

    let sock = TcpStream::connect(addr).await.expect("connect tcp");
    let mut ch = SecureChannel::connect(sock, &a, Some(&b_pub))
        .await
        .expect("secure connect");
    assert_eq!(ch.peer_identity(), &b_pub);
    ch.send(b"hello over tcp").await.expect("send");
    assert_eq!(ch.recv().await.expect("recv"), b"ack");

    server.await.expect("server task");
}
