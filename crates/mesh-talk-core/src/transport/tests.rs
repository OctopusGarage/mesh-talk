//! Module-level transport tests, including a real-TCP integration test.

use crate::identity::device::DeviceIdentity;
use crate::transport::{NOISE_PARAMS, SecureChannel};
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

#[tokio::test]
async fn denied_frame_commit_over_loopback_preserves_noise_nonce() {
    use crate::transport::TransportError;
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_public = bob.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let receiver = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(stream, &bob).await.unwrap();
        // A rejected commit must neither write a frame nor consume its nonce.
        assert_eq!(channel.recv().await.unwrap(), b"authorized");
    });
    let stream = TcpStream::connect(address).await.unwrap();
    let mut channel = SecureChannel::connect(stream, &alice, Some(&bob_public))
        .await
        .unwrap();
    let gate = std::sync::Arc::new(tokio::sync::RwLock::new(()));
    channel.set_io_admission(gate.clone(), std::sync::Arc::new(|| true));
    let held = gate.write().await;
    let allowed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let task_allowed = allowed.clone();
    let (began, waiting) = tokio::sync::oneshot::channel();
    let sender = tokio::spawn(async move {
        {
            let send = channel.send_if(b"obsolete", |commit| {
                if task_allowed.load(std::sync::atomic::Ordering::SeqCst) {
                    commit()
                } else {
                    Err(TransportError::AdmissionDenied)
                }
            });
            tokio::pin!(send);
            tokio::select! {
                biased;
                result = &mut send => panic!("send unexpectedly bypassed policy wait: {result:?}"),
                _ = std::future::ready(()) => { began.send(()).unwrap(); },
            }
            assert!(matches!(send.await, Err(TransportError::AdmissionDenied)));
        }
        channel.send(b"authorized").await.unwrap();
    });
    waiting.await.unwrap();
    allowed.store(false, std::sync::atomic::Ordering::SeqCst);
    drop(held);
    sender.await.unwrap();
    receiver.await.unwrap();
}
