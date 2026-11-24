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
