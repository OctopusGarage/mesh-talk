use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::net::{TcpListener, TcpStream};

const DEADLINE: Duration = Duration::from_secs(3);

async fn raw_initiator(stream: &mut DuplexStream, identity: &DeviceIdentity) -> HandshakeOutput {
    let mut hs = Handshake::initiator(&identity.secret_bytes().1).unwrap();
    write_frame(stream, &hs.write_message().unwrap())
        .await
        .unwrap();
    hs.read_message(&read_frame(stream).await.unwrap()).unwrap();
    write_frame(stream, &hs.write_message().unwrap())
        .await
        .unwrap();
    hs.into_session().unwrap()
}

#[tokio::test]
async fn static_pin_rejects_before_sending_noise_message_three() {
    let (client_io, mut server_io) = tokio::io::duplex(64 * 1024);
    let caller = DeviceIdentity::generate();
    let responder = DeviceIdentity::generate();
    let wrong = DeviceIdentity::generate().public();
    let server = tokio::spawn(async move {
        let mut hs = Handshake::responder(&responder.secret_bytes().1).unwrap();
        hs.read_message(&read_frame(&mut server_io).await.unwrap())
            .unwrap();
        write_frame(&mut server_io, &hs.write_message().unwrap())
            .await
            .unwrap();
        read_frame(&mut server_io).await.is_ok()
    });
    assert!(matches!(
        tokio::time::timeout(
            DEADLINE,
            SecureChannel::connect(client_io, &caller, Some(&wrong))
        )
        .await
        .unwrap(),
        Err(TransportError::UnexpectedPeer)
    ));
    assert!(!tokio::time::timeout(DEADLINE, server)
        .await
        .unwrap()
        .unwrap());
}

#[tokio::test]
async fn matching_static_key_does_not_bypass_full_identity_pin() {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let caller = DeviceIdentity::generate();
    let responder = DeviceIdentity::generate();
    let mut wrong = DeviceIdentity::generate().public();
    wrong.x25519_pub = responder.public().x25519_pub;
    let server =
        tokio::spawn(async move { SecureChannel::accept(server_io, &responder).await.is_ok() });
    assert!(matches!(
        tokio::time::timeout(
            DEADLINE,
            SecureChannel::connect(client_io, &caller, Some(&wrong))
        )
        .await
        .unwrap(),
        Err(TransportError::UnexpectedPeer)
    ));
    assert!(tokio::time::timeout(DEADLINE, server)
        .await
        .unwrap()
        .unwrap());
}

#[tokio::test]
async fn forged_auth_never_reaches_admission_or_gets_responder_auth() {
    let (mut client_io, server_io) = tokio::io::duplex(64 * 1024);
    let caller = DeviceIdentity::generate();
    let responder = DeviceIdentity::generate();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = tokio::spawn(async move {
        SecureChannel::accept_with_admission(server_io, &responder, |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await
    });
    let mut out = tokio::time::timeout(DEADLINE, raw_initiator(&mut client_io, &caller))
        .await
        .unwrap();
    let mut auth = build_auth(&caller, &out.handshake_hash);
    auth.signature[0] ^= 1;
    let bytes = out
        .session
        .encrypt(&bincode::serialize(&auth).unwrap())
        .unwrap();
    write_frame(&mut client_io, &bytes).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(DEADLINE, server)
            .await
            .unwrap()
            .unwrap(),
        Err(TransportError::IdentityMismatch)
    ));
    assert!(matches!(
        tokio::time::timeout(DEADLINE, read_frame(&mut client_io))
            .await
            .unwrap(),
        Err(TransportError::Io(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verified_caller_can_exchange_application_data_with_admission_over_tcp() {
    let caller = DeviceIdentity::generate();
    let caller_public = caller.public();
    let responder = DeviceIdentity::generate();
    let responder_public = responder.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel =
            SecureChannel::accept_with_admission(stream, &responder, |peer| peer == &caller_public)
                .await
                .unwrap();
        assert_eq!(channel.recv().await.unwrap(), b"private ping");
        channel.send(b"private pong").await.unwrap();
    });
    tokio::time::timeout(DEADLINE, async {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut channel = SecureChannel::connect(stream, &caller, Some(&responder_public))
            .await
            .unwrap();
        channel.send(b"private ping").await.unwrap();
        assert_eq!(channel.recv().await.unwrap(), b"private pong");
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn denied_verified_caller_cannot_obtain_responder_auth_over_tcp() {
    let caller = DeviceIdentity::generate();
    let responder = DeviceIdentity::generate();
    let responder_public = responder.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        SecureChannel::accept_with_admission(stream, &responder, |_| false).await
    });
    tokio::time::timeout(DEADLINE, async {
        let stream = TcpStream::connect(address).await.unwrap();
        assert!(matches!(
            SecureChannel::connect(stream, &caller, Some(&responder_public)).await,
            Err(TransportError::Io(_))
        ));
        assert!(matches!(
            server.await.unwrap(),
            Err(TransportError::AdmissionDenied)
        ));
    })
    .await
    .unwrap();
}
