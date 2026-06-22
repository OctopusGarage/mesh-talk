//! Async authenticated, encrypted channel over any byte stream.
//!
//! Wire format per message: a 4-byte big-endian length prefix followed by the
//! Noise blob. The same framing carries handshake messages, the encrypted auth
//! exchange, and application messages.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::transport::auth::{build_auth, verify_auth, AuthMessage};
use crate::transport::handshake::{Handshake, HandshakeOutput};
use crate::transport::session::Session;
use crate::transport::{TransportError, MAX_FRAME};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// An established, authenticated, encrypted channel to one peer.
pub struct SecureChannel<S> {
    stream: S,
    session: Session,
    peer: PublicIdentity,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SecureChannel<S> {
    /// Dial: perform the XX handshake as initiator, then authenticate.
    /// If `expected_peer` is set, the authenticated identity must match it.
    pub async fn connect(
        mut stream: S,
        identity: &DeviceIdentity,
        expected_peer: Option<&PublicIdentity>,
    ) -> Result<Self, TransportError> {
        let x_secret = identity.secret_bytes().1;
        let mut hs = Handshake::initiator(&x_secret)?;

        // XX: -> e ; <- e, ee, s, es ; -> s, se
        let m1 = hs.write_message()?;
        write_frame(&mut stream, &m1).await?;
        let m2 = read_frame(&mut stream).await?;
        hs.read_message(&m2)?;
        let m3 = hs.write_message()?;
        write_frame(&mut stream, &m3).await?;

        let out = hs.into_session()?;
        // Initiator authenticates first, then reads the peer's auth.
        let (session, peer) =
            auth_exchange(&mut stream, identity, out, AuthOrder::SendFirst).await?;

        if let Some(expected) = expected_peer {
            if expected != &peer {
                return Err(TransportError::UnexpectedPeer);
            }
        }
        Ok(Self {
            stream,
            session,
            peer,
        })
    }

    /// Accept: perform the XX handshake as responder, then authenticate.
    ///
    /// Unlike [`connect`](Self::connect), this accepts ANY peer that
    /// authenticates successfully — the responder does not know the caller in
    /// advance. Callers that care which peer connected must inspect
    /// [`peer_identity`](Self::peer_identity) after this returns.
    pub async fn accept(mut stream: S, identity: &DeviceIdentity) -> Result<Self, TransportError> {
        let x_secret = identity.secret_bytes().1;
        let mut hs = Handshake::responder(&x_secret)?;

        let m1 = read_frame(&mut stream).await?;
        hs.read_message(&m1)?;
        let m2 = hs.write_message()?;
        write_frame(&mut stream, &m2).await?;
        let m3 = read_frame(&mut stream).await?;
        hs.read_message(&m3)?;

        let out = hs.into_session()?;
        // Responder reads the peer's auth first, then sends its own.
        let (session, peer) =
            auth_exchange(&mut stream, identity, out, AuthOrder::ReceiveFirst).await?;

        Ok(Self {
            stream,
            session,
            peer,
        })
    }

    /// The cryptographically authenticated identity of the peer.
    pub fn peer_identity(&self) -> &PublicIdentity {
        &self.peer
    }

    /// Encrypt and send one application message.
    pub async fn send(&mut self, plaintext: &[u8]) -> Result<(), TransportError> {
        let ct = self.session.encrypt(plaintext)?;
        write_frame(&mut self.stream, &ct).await
    }

    /// Receive and decrypt one application message.
    pub async fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
        let ct = read_frame(&mut self.stream).await?;
        self.session.decrypt(&ct)
    }
}

/// Whether this side sends or receives its auth message first.
///
/// The initiator sends first and the responder receives first, so the two
/// ends are always in complementary states — neither can block on a read while
/// the other also blocks on a read. Noise tracks send/receive nonce counters
/// independently per direction, so this single shared [`Session`] stays in sync.
enum AuthOrder {
    SendFirst,
    ReceiveFirst,
}

/// Build and encrypt our auth message using the current session state.
fn encrypt_auth(
    session: &mut Session,
    identity: &DeviceIdentity,
    handshake_hash: &[u8; 32],
) -> Result<Vec<u8>, TransportError> {
    let auth = build_auth(identity, handshake_hash);
    let bytes =
        bincode::serialize(&auth).map_err(|e| TransportError::Serialization(e.to_string()))?;
    session.encrypt(&bytes)
}

/// Exchange and verify identity-auth messages over the freshly-established
/// session. Returns the session and the verified peer identity.
async fn auth_exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    identity: &DeviceIdentity,
    out: HandshakeOutput,
    order: AuthOrder,
) -> Result<(Session, PublicIdentity), TransportError> {
    let HandshakeOutput {
        mut session,
        remote_static,
        handshake_hash,
    } = out;

    let peer = match order {
        AuthOrder::SendFirst => {
            let ct = encrypt_auth(&mut session, identity, &handshake_hash)?;
            write_frame(stream, &ct).await?;
            let peer_ct = read_frame(stream).await?;
            verify_peer(&mut session, &peer_ct, &handshake_hash, &remote_static)?
        }
        AuthOrder::ReceiveFirst => {
            let peer_ct = read_frame(stream).await?;
            let peer = verify_peer(&mut session, &peer_ct, &handshake_hash, &remote_static)?;
            let ct = encrypt_auth(&mut session, identity, &handshake_hash)?;
            write_frame(stream, &ct).await?;
            peer
        }
    };
    Ok((session, peer))
}

fn verify_peer(
    session: &mut Session,
    peer_ct: &[u8],
    handshake_hash: &[u8; 32],
    remote_static: &[u8; 32],
) -> Result<PublicIdentity, TransportError> {
    let bytes = session.decrypt(peer_ct)?;
    let auth: AuthMessage =
        bincode::deserialize(&bytes).map_err(|e| TransportError::Serialization(e.to_string()))?;
    verify_auth(&auth, handshake_hash, remote_static)
}

/// Write a length-prefixed frame (4-byte big-endian length + blob) and flush.
pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    blob: &[u8],
) -> Result<(), TransportError> {
    let len = blob.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(blob).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed frame, rejecting absurd lengths before allocating.
pub(crate) async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, TransportError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[tokio::test]
    async fn channel_handshakes_authenticates_and_exchanges_messages() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (a_pub, b_pub) = (a.public(), b.public());

        let server = tokio::spawn(async move {
            let mut ch = SecureChannel::accept(server_io, &b).await.expect("accept");
            // Server confirms it is talking to A.
            assert_eq!(ch.peer_identity(), &a_pub);
            let got = ch.recv().await.expect("recv");
            assert_eq!(got, b"ping");
            ch.send(b"pong").await.expect("send");
        });

        let mut ch = SecureChannel::connect(client_io, &a, Some(&b_pub))
            .await
            .expect("connect");
        // Client confirms it is talking to B.
        assert_eq!(ch.peer_identity(), &b_pub);
        ch.send(b"ping").await.expect("send");
        let got = ch.recv().await.expect("recv");
        assert_eq!(got, b"pong");

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn connect_rejects_unexpected_peer() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let wrong = DeviceIdentity::generate().public();

        // Responder completes its side normally.
        let server = tokio::spawn(async move {
            let _ = SecureChannel::accept(server_io, &b).await;
        });

        // Initiator expects `wrong` but actually talks to B -> UnexpectedPeer.
        let result = SecureChannel::connect(client_io, &a, Some(&wrong)).await;
        assert!(matches!(result, Err(TransportError::UnexpectedPeer)));
        let _ = server.await;
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_length() {
        let (mut a, mut b) = tokio::io::duplex(64);
        // Write a length prefix far above MAX_FRAME, no body.
        let bogus = (MAX_FRAME as u32 + 1).to_be_bytes();
        a.write_all(&bogus).await.expect("write len");
        a.flush().await.expect("flush");
        let result = read_frame(&mut b).await;
        assert!(matches!(result, Err(TransportError::FrameTooLarge(_))));
    }

    #[tokio::test]
    async fn connect_fails_when_peer_drops_mid_handshake() {
        let (client_io, mut server_io) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();

        let server = tokio::spawn(async move {
            // Read the initiator's first handshake message, then drop the
            // stream — the client's next read must hit EOF, not hang or panic.
            let _ = read_frame(&mut server_io).await;
        });

        let result = SecureChannel::connect(client_io, &a, None).await;
        assert!(matches!(result, Err(TransportError::Io(_))));
        let _ = server.await;
    }
}
