use mesh_connect::{PeerSession, TransportError};
use tokio::net::{TcpListener, TcpStream};
use x25519_dalek::{PublicKey, StaticSecret};

fn public(secret: [u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(secret)).to_bytes()
}

#[tokio::test]
async fn authenticated_tcp_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client_key = [11; 32];
    let server_key = [22; 32];
    let ((), ()) = tokio::join!(
        async {
            let stream = TcpStream::connect(address).await.unwrap();
            let mut peer = PeerSession::connect(stream, &client_key, &public(server_key))
                .await
                .unwrap();
            peer.send(b"request").await.unwrap();
            assert_eq!(peer.recv().await.unwrap(), b"response");
        },
        async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut peer =
                PeerSession::accept(stream, &server_key, |key| *key == public(client_key))
                    .await
                    .unwrap();
            assert_eq!(peer.peer_key(), &public(client_key));
            assert_eq!(peer.recv().await.unwrap(), b"request");
            peer.send(b"response").await.unwrap();
        }
    );
}

#[tokio::test]
async fn wrong_responder_key_aborts_before_client_static_key_is_sent() {
    let (client, server) = tokio::io::duplex(1024);
    let wrong_peer = public([33; 32]);
    let (outgoing, incoming) = tokio::join!(
        PeerSession::connect(client, &[11; 32], &wrong_peer),
        PeerSession::accept(server, &[22; 32], |_| panic!("client revealed its key")),
    );
    assert!(matches!(outgoing, Err(TransportError::UnexpectedPeer)));
    assert!(matches!(incoming, Err(TransportError::Io(_))));
}

#[tokio::test]
async fn oversized_outbound_frame_is_rejected_before_writing() {
    use mesh_connect::{MAX_FRAME, frame};
    let mut output = Vec::new();
    let error = frame::write_frame(&mut output, &vec![0; MAX_FRAME + 1])
        .await
        .unwrap_err();
    assert!(matches!(error, TransportError::FrameTooLarge(n) if n == MAX_FRAME + 1));
    assert!(output.is_empty());
}

#[tokio::test]
async fn cancelling_backpressured_send_retires_the_session() {
    let (client, server) = tokio::io::duplex(256);
    let server_key = public([22; 32]);
    let (client, server) = tokio::join!(
        PeerSession::connect(client, &[11; 32], &server_key),
        PeerSession::accept(server, &[22; 32], |_| true),
    );
    let mut client = client.unwrap();
    let _server = server.unwrap();
    let payload = [7; 1024];
    {
        use std::{
            future::{Future, poll_fn},
            task::Poll,
        };
        let mut send = std::pin::pin!(client.send(&payload));
        poll_fn(|cx| {
            assert!(send.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    let mut retry = std::pin::pin!(client.send(b"retry"));
    poll_fn(|cx| {
        assert!(matches!(
            retry.as_mut().poll(cx),
            Poll::Ready(Err(TransportError::Io(error)))
                if error.kind() == std::io::ErrorKind::NotConnected
        ));
        Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn cancelling_partial_receive_closes_the_stream() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };

    let (client, server) = tokio::io::duplex(256);
    let server_key = public([22; 32]);
    let (client, server) = tokio::join!(
        PeerSession::connect(client, &[11; 32], &server_key),
        PeerSession::accept(server, &[22; 32], |_| true),
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    assert!(matches!(
        client.send(&vec![0; mesh_connect::MAX_PLAINTEXT + 1]).await,
        Err(TransportError::PlaintextTooLarge(_))
    ));
    client.send(b"healthy").await.unwrap();
    assert_eq!(server.recv().await.unwrap(), b"healthy");

    let payload = [7; 1024];
    let mut send = std::pin::pin!(client.send(&payload));
    {
        let mut recv = std::pin::pin!(server.recv());
        poll_fn(|cx| {
            assert!(send.as_mut().poll(cx).is_pending());
            assert!(recv.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert!(matches!(
        server.recv().await,
        Err(TransportError::Io(error)) if error.kind() == std::io::ErrorKind::NotConnected
    ));
    assert!(matches!(send.await, Err(TransportError::Io(_))));
}

#[tokio::test]
async fn discovery_config_rejects_unicast_and_retains_the_assigned_port() {
    use mesh_connect::net::DiscoveryConfig;
    use std::net::Ipv4Addr;

    assert!(DiscoveryConfig::new(47474, Ipv4Addr::LOCALHOST).is_err());
    let group = Ipv4Addr::new(239, 42, 1, 2);
    let discovery = DiscoveryConfig::new(0, group).unwrap().bind().unwrap();
    let local = discovery.socket().local_addr().unwrap();
    assert_ne!(local.port(), 0);
    assert_eq!(discovery.multicast_target(), (group, local.port()).into());
}

#[tokio::test]
async fn vectored_frame_write_coalesces_header_and_handles_short_writes() {
    use std::{
        io::{self, IoSlice},
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::AsyncWrite;

    #[derive(Default)]
    struct ShortWriter {
        stalled: bool,
        bytes: Vec<u8>,
        vectored_calls: usize,
        flushes: usize,
    }

    impl AsyncWrite for ShortWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            panic!("a vectored writer should receive the complete frame as slices")
        }

        fn is_write_vectored(&self) -> bool {
            true
        }

        fn poll_write_vectored(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            slices: &[IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            self.vectored_calls += 1;
            if self.stalled {
                return Poll::Ready(Ok(0));
            }
            let start = self.bytes.len();
            // Stop first inside the header, then inside the body.
            self.bytes.extend(
                slices
                    .iter()
                    .flat_map(|slice| slice.iter().copied())
                    .take(3),
            );
            Poll::Ready(Ok(self.bytes.len() - start))
        }

        fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.flushes += 1;
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    let mut writer = ShortWriter::default();
    mesh_connect::frame::write_frame(&mut writer, b"hello")
        .await
        .unwrap();
    assert_eq!(writer.bytes, b"\0\0\0\x05hello");
    assert_eq!(writer.vectored_calls, 3);
    assert_eq!(writer.flushes, 1);

    let mut stalled = ShortWriter {
        stalled: true,
        ..ShortWriter::default()
    };
    assert!(matches!(
        mesh_connect::frame::write_frame(&mut stalled, b"hello").await,
        Err(TransportError::Io(error)) if error.kind() == io::ErrorKind::WriteZero
    ));
    assert!(stalled.bytes.is_empty());
    assert_eq!(stalled.flushes, 0);
}

#[test]
fn tcp_session_connect_future_is_send() {
    fn require_send(_: impl Send) {}
    async fn connect(stream: TcpStream) {
        let secret = [11; 32];
        let expected_peer = public([22; 32]);
        let _ = PeerSession::connect(stream, &secret, &expected_peer).await;
    }

    fn assert_connect(stream: TcpStream) {
        require_send(connect(stream));
    }

    let _: fn(TcpStream) = assert_connect;
}
