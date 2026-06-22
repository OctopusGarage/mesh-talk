//! Establishing authenticated, encrypted channels over TCP. `dial` connects to a peer and
//! runs the Noise handshake as initiator (optionally pinning the expected peer);
//! `secure_accept` authenticates one already-accepted inbound stream, bounded by
//! [`HANDSHAKE_TIMEOUT`] (accept loops run it in a per-connection task so a stalled
//! handshake can't wedge the loop). Both return a [`SecureChannel`] keyed by the
//! cryptographically-verified peer.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::transport::{SecureChannel, TransportError};
use std::net::SocketAddr;
use std::time::Duration;
#[cfg(test)]
use tokio::net::TcpListener;
use tokio::net::TcpStream;

/// Wall-clock bound on the server-side handshake. A peer that completes the TCP connect
/// but stalls mid-handshake (sends nothing / a partial frame) must not tie up the caller —
/// accept loops run [`secure_accept`] in a per-connection task, but the timeout is the
/// backstop that frees the slot. Generous: a real LAN handshake is sub-second.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Dial `addr`, perform the Noise XX handshake + identity auth as the initiator.
/// If `expected_peer` is `Some`, the authenticated identity must match it
/// (`TransportError::UnexpectedPeer` otherwise).
pub async fn dial(
    addr: SocketAddr,
    identity: &DeviceIdentity,
    expected_peer: Option<&PublicIdentity>,
) -> Result<SecureChannel<TcpStream>, TransportError> {
    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
    crate::transport::net::set_tcp_keepalive(&stream);
    SecureChannel::connect(stream, identity, expected_peer).await
}

/// Run the server-side Noise handshake + identity auth on an already-accepted `stream`,
/// bounded by [`HANDSHAKE_TIMEOUT`]. Accept loops call this in a per-connection task so a
/// stalled handshake can never park the loop (and the timeout frees the slot regardless).
pub async fn secure_accept(
    stream: TcpStream,
    identity: &DeviceIdentity,
) -> Result<SecureChannel<TcpStream>, TransportError> {
    stream.set_nodelay(true)?;
    crate::transport::net::set_tcp_keepalive(&stream);
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, SecureChannel::accept(stream, identity)).await {
        Ok(result) => result,
        Err(_) => Err(TransportError::Noise("accept handshake timed out".into())),
    }
}

/// Test-only convenience: accept one inbound connection and authenticate the peer in a
/// single await. Production accept loops instead `listener.accept()` then run
/// [`secure_accept`] in a spawned task (so a stalled handshake can't wedge the loop) —
/// see `run_accept_loop` / `run_relay_accept_loop`.
#[cfg(test)]
async fn accept(
    listener: &TcpListener,
    identity: &DeviceIdentity,
) -> Result<SecureChannel<TcpStream>, TransportError> {
    let (stream, _addr) = listener.accept().await?;
    secure_accept(stream, identity).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dial_and_accept_mutually_authenticate_over_tcp() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (a_pub, b_pub) = (a.public(), b.public());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let ch = accept(&listener, &b).await.expect("accept");
            assert_eq!(ch.peer_identity(), &a_pub);
        });

        let ch = dial(addr, &a, Some(&b_pub)).await.expect("dial");
        assert_eq!(ch.peer_identity(), &b_pub);

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn dial_rejects_an_unexpected_peer() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let wrong = DeviceIdentity::generate().public();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let _ = accept(&listener, &b).await;
        });

        let result = dial(addr, &a, Some(&wrong)).await;
        assert!(matches!(result, Err(TransportError::UnexpectedPeer)));
        let _ = server.await;
    }

    #[tokio::test]
    async fn dial_with_no_expected_peer_still_authenticates() {
        // `None` does not PIN a peer, but the channel is still authenticated:
        // `peer_identity()` reflects the real, cryptographically-verified peer.
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let b_pub = b.public();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            accept(&listener, &b).await.expect("accept");
        });

        let ch = dial(addr, &a, None).await.expect("dial with no pin");
        assert_eq!(ch.peer_identity(), &b_pub);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn dial_to_a_closed_port_errors() {
        // Bind then drop to get a port that is now closed; dialing it must return
        // an error (connection refused), never panic.
        let a = DeviceIdentity::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        assert!(dial(addr, &a, None).await.is_err());
    }
}
