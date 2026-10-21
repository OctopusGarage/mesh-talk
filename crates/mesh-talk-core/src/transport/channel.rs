//! Async authenticated, encrypted channel over any byte stream.
//!
//! Wire format per message: a 4-byte big-endian length prefix followed by the
//! Noise blob. The same framing carries handshake messages, the encrypted auth
//! exchange, and application messages.

use crate::discovery::Announce;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
#[cfg(test)]
use crate::transport::auth::build_auth;
use crate::transport::handshake::{Handshake, HandshakeOutput};
use crate::transport::presence_auth;
use crate::transport::session::Session;
use crate::transport::{MAX_FRAME, TransportError, VerifiedPeer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// An established, authenticated, encrypted channel to one peer.
pub struct SecureChannel<S> {
    stream: S,
    session: Session,
    peer: VerifiedPeer,
    io_admission: Option<IoAdmission>,
    sent_frame_bytes: u64,
    connection_permits: Vec<tokio::sync::OwnedSemaphorePermit>,
    host_generation: Option<u64>,
    pairing_only: bool,
}

#[derive(Clone)]
struct IoAdmission {
    gate: std::sync::Arc<tokio::sync::RwLock<()>>,
    check: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SecureChannel<S> {
    /// Dial: perform the XX handshake as initiator, then authenticate.
    /// If `expected_peer` is set, the authenticated identity must match it.
    pub async fn connect(
        stream: S,
        identity: &DeviceIdentity,
        expected_peer: Option<&PublicIdentity>,
    ) -> Result<Self, TransportError> {
        Self::connect_with_presence(stream, identity, expected_peer, None).await
    }

    /// Dial with an optional device-signed account announcement bound to this
    /// Noise session. Without a presence proof the emitted auth bytes are legacy.
    /// Supplied own proofs are validated before any handshake traffic is sent.
    pub async fn connect_with_presence(
        stream: S,
        identity: &DeviceIdentity,
        expected_peer: Option<&PublicIdentity>,
        presence: Option<&Announce>,
    ) -> Result<Self, TransportError> {
        Self::connect_with_visibility(stream, identity, expected_peer, presence, true).await
    }

    pub async fn connect_with_visibility(
        mut stream: S,
        identity: &DeviceIdentity,
        expected_peer: Option<&PublicIdentity>,
        presence: Option<&Announce>,
        publicly_visible: bool,
    ) -> Result<Self, TransportError> {
        presence_auth::validate_own(identity, presence)?;
        let x_secret = identity.secret_bytes().1;
        let mut hs = Handshake::initiator(&x_secret)?;

        // XX: -> e ; <- e, ee, s, es ; -> s, se
        let m1 = hs.write_message()?;
        write_frame(&mut stream, &m1).await?;
        let m2 = read_frame(&mut stream).await?;
        hs.read_message(&m2)?;
        // Pin the Noise-authenticated static key before disclosing our static
        // key or Ed25519 auth to an endpoint reached through a stale/spoofed
        // discovery address. The full identity is still checked below.
        if let Some(expected) = expected_peer
            && hs.remote_static()? != expected.x25519_pub
        {
            return Err(TransportError::UnexpectedPeer);
        }
        let m3 = hs.write_message()?;
        write_frame(&mut stream, &m3).await?;

        let out = hs.into_session()?;
        // Initiator authenticates first, then reads the peer's auth.
        let (session, peer) = auth_exchange(
            &mut stream,
            identity,
            presence,
            publicly_visible,
            out,
            AuthOrder::SendFirst,
            |_| true,
        )
        .await?;

        if let Some(expected) = expected_peer
            && expected != peer.public_identity()
        {
            return Err(TransportError::UnexpectedPeer);
        }
        Ok(Self {
            stream,
            session,
            peer,
            io_admission: None,
            sent_frame_bytes: 0,
            connection_permits: Vec::new(),
            host_generation: None,
            pairing_only: false,
        })
    }

    /// Accept: perform the XX handshake as responder, then authenticate.
    ///
    /// Unlike [`connect`](Self::connect), this accepts ANY peer that
    /// authenticates successfully — the responder does not know the caller in
    /// advance. Callers that care which peer connected must inspect
    /// [`peer_identity`](Self::peer_identity) after this returns.
    pub async fn accept(stream: S, identity: &DeviceIdentity) -> Result<Self, TransportError> {
        Self::accept_with_admission(stream, identity, |_| true).await
    }

    /// Authenticate the caller, then consult `admit` before sending our own
    /// identity auth or returning an application channel. The predicate sees
    /// only a cryptographically verified device identity, not an account ID;
    /// the host must bind it to a verified device-signed announcement matching
    /// this exact identity, not an account certificate alone.
    ///
    /// Denial closes the stream. Noise XX still exposes the responder's static
    /// X25519 key during the handshake: this is not network anonymity. The
    /// predicate is a connection gate, not revocation or event-author filtering;
    /// hosts must enforce their policy on subsequent application traffic too.
    pub async fn accept_with_admission<F>(
        stream: S,
        identity: &DeviceIdentity,
        admit: F,
    ) -> Result<Self, TransportError>
    where
        F: FnOnce(&PublicIdentity) -> bool,
    {
        Self::accept_with_presence(stream, identity, None, |peer| admit(peer.public_identity()))
            .await
    }

    /// Authenticate the caller's device and optional signed account proof before
    /// consulting admission and sending our own identity/presence auth.
    /// Noise XX still exposes the responder's static key; this is not anonymity.
    /// Hosts must enforce revocation and author admission on application traffic.
    pub async fn accept_with_presence<F>(
        stream: S,
        identity: &DeviceIdentity,
        presence: Option<&Announce>,
        admit: F,
    ) -> Result<Self, TransportError>
    where
        F: FnOnce(&VerifiedPeer) -> bool,
    {
        Self::accept_with_visibility(stream, identity, presence, true, admit).await
    }

    pub async fn accept_with_visibility<F>(
        mut stream: S,
        identity: &DeviceIdentity,
        presence: Option<&Announce>,
        publicly_visible: bool,
        admit: F,
    ) -> Result<Self, TransportError>
    where
        F: FnOnce(&VerifiedPeer) -> bool,
    {
        presence_auth::validate_own(identity, presence)?;
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
        let (session, peer) = auth_exchange(
            &mut stream,
            identity,
            presence,
            publicly_visible,
            out,
            AuthOrder::ReceiveFirst,
            admit,
        )
        .await?;

        Ok(Self {
            stream,
            session,
            peer,
            io_admission: None,
            sent_frame_bytes: 0,
            connection_permits: Vec::new(),
            host_generation: None,
            pairing_only: false,
        })
    }

    pub(crate) fn retain_connection_permits(
        &mut self,
        permits: Vec<tokio::sync::OwnedSemaphorePermit>,
    ) {
        self.connection_permits = permits;
    }

    pub(crate) fn set_host_generation(&mut self, generation: u64) {
        self.host_generation = Some(generation);
    }

    pub(crate) fn host_generation(&self) -> Option<u64> {
        self.host_generation
    }

    pub(crate) fn restrict_to_pairing(&mut self) {
        self.pairing_only = true;
    }

    pub(crate) fn pairing_only(&self) -> bool {
        self.pairing_only
    }

    /// The cryptographically authenticated identity of the peer.
    pub fn peer_identity(&self) -> &PublicIdentity {
        self.peer.public_identity()
    }

    /// Session-verified account announcement, when the peer supplied one.
    /// The announcement contains no observed IP address or endpoint freshness.
    pub fn peer_announcement(&self) -> Option<&Announce> {
        self.peer.announcement()
    }

    pub fn peer_publicly_visible(&self) -> bool {
        self.peer.publicly_visible()
    }

    /// Install a host policy gate. Sends hold the shared read gate across the
    /// bounded frame write; reads recheck admission after awaiting peer traffic.
    /// Hosts may capture a policy generation to invalidate old prepared frames.
    pub fn set_io_admission(
        &mut self,
        gate: std::sync::Arc<tokio::sync::RwLock<()>>,
        check: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        self.io_admission = Some(IoAdmission { gate, check });
    }
    /// Recheck a host-installed admission predicate after an external await.
    pub fn io_admitted(&self) -> bool {
        self.io_admission.as_ref().is_none_or(|a| (a.check)())
    }
    /// Encrypted application-frame bytes written on this connection, including
    /// the four-byte length prefix. The Noise handshake is excluded.
    pub fn sent_frame_bytes(&self) -> u64 {
        self.sent_frame_bytes
    }

    /// Encrypt and send one application message.
    pub async fn send(&mut self, plaintext: &[u8]) -> Result<(), TransportError> {
        self.send_if(plaintext, |commit| commit()).await
    }

    /// Admit encryption only after any policy wait. A denied operation consumes
    /// no Noise nonce; accepted bounded writes may finish after host revocation.
    pub(crate) async fn send_if(
        &mut self,
        plaintext: &[u8],
        mut authorize: impl FnMut(
            &mut dyn FnMut() -> Result<(), TransportError>,
        ) -> Result<(), TransportError>
        + Send,
    ) -> Result<(), TransportError> {
        if let Some(admission) = self.io_admission.clone() {
            let bytes = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let _guard = admission.gate.read().await;
                if !(admission.check)() {
                    return Err(TransportError::AdmissionDenied);
                }
                let ct = self.encrypt_if(plaintext, &mut authorize)?;
                write_frame(&mut self.stream, &ct).await?;
                Ok(ct.len() as u64 + 4)
            })
            .await
            .map_err(|_| TransportError::Noise("policy-gated send timed out".into()))?;
            self.sent_frame_bytes = self.sent_frame_bytes.saturating_add(bytes?);
            return Ok(());
        }
        let ct = self.encrypt_if(plaintext, &mut authorize)?;
        write_frame(&mut self.stream, &ct).await?;
        self.sent_frame_bytes = self.sent_frame_bytes.saturating_add(ct.len() as u64 + 4);
        Ok(())
    }

    fn encrypt_if(
        &mut self,
        plaintext: &[u8],
        authorize: &mut impl FnMut(
            &mut dyn FnMut() -> Result<(), TransportError>,
        ) -> Result<(), TransportError>,
    ) -> Result<Vec<u8>, TransportError> {
        let mut ciphertext = None;
        authorize(&mut || {
            if ciphertext.is_some() {
                return Err(TransportError::AdmissionDenied);
            }
            ciphertext = Some(self.session.encrypt(plaintext)?);
            Ok(())
        })?;
        ciphertext.ok_or(TransportError::AdmissionDenied)
    }

    /// Receive and decrypt one application message.
    pub async fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
        if self.io_admission.as_ref().is_some_and(|a| !(a.check)()) {
            return Err(TransportError::AdmissionDenied);
        }
        let ct = read_frame(&mut self.stream).await?;
        if self.io_admission.as_ref().is_some_and(|a| !(a.check)()) {
            return Err(TransportError::AdmissionDenied);
        }
        self.session.decrypt(&ct)
    }
}

impl SecureChannel<tokio::net::TcpStream> {
    #[cfg(test)]
    pub(crate) fn tcp_keepalive_enabled(&self) -> bool {
        socket2::SockRef::from(&self.stream).keepalive().unwrap()
    }

    pub(crate) fn tcp_peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.stream.peer_addr()
    }

    /// A reusable session has no unsolicited bytes. EOF, pending frames and
    /// socket errors all require a fresh handshake rather than nonce reuse.
    pub(crate) fn idle_tcp_alive(&self) -> bool {
        let mut byte = [0u8; 1];
        matches!(self.stream.try_read(&mut byte), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
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
    presence: Option<&Announce>,
    publicly_visible: bool,
) -> Result<Vec<u8>, TransportError> {
    let bytes =
        presence_auth::build_with_visibility(identity, handshake_hash, presence, publicly_visible)?;
    session.encrypt(&bytes)
}

/// Exchange and verify identity-auth messages over the freshly-established
/// session. Returns the session and the verified peer identity.
async fn auth_exchange<S, F>(
    stream: &mut S,
    identity: &DeviceIdentity,
    presence: Option<&Announce>,
    publicly_visible: bool,
    out: HandshakeOutput,
    order: AuthOrder,
    admit: F,
) -> Result<(Session, VerifiedPeer), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(&VerifiedPeer) -> bool,
{
    let HandshakeOutput {
        mut session,
        remote_static,
        handshake_hash,
    } = out;

    let peer = match order {
        AuthOrder::SendFirst => {
            let ct = encrypt_auth(
                &mut session,
                identity,
                &handshake_hash,
                presence,
                publicly_visible,
            )?;
            write_frame(stream, &ct).await?;
            let peer_ct = read_frame(stream).await?;
            verify_peer(&mut session, &peer_ct, &handshake_hash, &remote_static)?
        }
        AuthOrder::ReceiveFirst => {
            let peer_ct = read_frame(stream).await?;
            let peer = verify_peer(&mut session, &peer_ct, &handshake_hash, &remote_static)?;
            if !admit(&peer) {
                return Err(TransportError::AdmissionDenied);
            }
            let ct = encrypt_auth(
                &mut session,
                identity,
                &handshake_hash,
                presence,
                publicly_visible,
            )?;
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
) -> Result<VerifiedPeer, TransportError> {
    let bytes = session.decrypt(peer_ct)?;
    presence_auth::verify(&bytes, handshake_hash, remote_static)
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
    fn incomplete_frame(error: std::io::Error) -> std::io::Error {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::UnexpectedEof
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected => std::io::Error::new(ErrorKind::InvalidData, error),
            _ => error,
        }
    }

    let mut len_buf = [0u8; 4];
    // Only a close before any frame bytes is a compatibility/liveness signal.
    // Partial framing is malformed data and must never authorize a downgrade.
    let received = r.read(&mut len_buf).await?;
    if received == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "transport closed at frame boundary",
        )
        .into());
    }
    if received < len_buf.len() {
        r.read_exact(&mut len_buf[received..])
            .await
            .map_err(incomplete_frame)?;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.map_err(incomplete_frame)?;
    Ok(buf)
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod admission_tests;

#[cfg(test)]
#[path = "presence_tests.rs"]
mod presence_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[tokio::test]
    async fn read_frame_distinguishes_boundary_eof_from_truncated_frames() {
        let mut empty = &[][..];
        assert!(matches!(read_frame(&mut empty).await,
            Err(TransportError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof));
        let frame = [0, 0, 0, 3, 10, 11, 12];
        for length in 1..frame.len() {
            let mut truncated = &frame[..length];
            assert!(
                matches!(read_frame(&mut truncated).await,
                Err(TransportError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData),
                "partial frame of {length} bytes must not be a clean boundary close"
            );
        }
    }

    struct FragmentedFrame<'a> {
        bytes: &'a [u8],
        ending: Option<std::io::ErrorKind>,
    }

    impl AsyncRead for FragmentedFrame<'_> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            output: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if output.remaining() == 0 {
                return std::task::Poll::Ready(Ok(()));
            }
            if let Some((first, rest)) = self.bytes.split_first() {
                output.put_slice(&[*first]);
                self.bytes = rest;
                std::task::Poll::Ready(Ok(()))
            } else if let Some(kind) = self.ending {
                std::task::Poll::Ready(Err(std::io::Error::new(kind, "frame read failed")))
            } else {
                std::task::Poll::Ready(Ok(()))
            }
        }
    }

    #[tokio::test]
    async fn read_frame_accepts_fragmented_frames_and_preserves_boundaries() {
        let mut source = FragmentedFrame {
            bytes: &[0, 0, 0, 3, 10, 11, 12, 0, 0, 0, 0],
            ending: None,
        };
        assert_eq!(read_frame(&mut source).await.unwrap(), vec![10, 11, 12]);
        assert!(read_frame(&mut source).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_frame_classifies_partial_reset_and_preserves_other_errors() {
        use std::io::ErrorKind;
        for prefix in [&[0][..], &[0, 0, 0, 3][..], &[0, 0, 0, 3, 10][..]] {
            for ending in [ErrorKind::ConnectionReset, ErrorKind::ConnectionAborted] {
                let mut source = FragmentedFrame {
                    bytes: prefix,
                    ending: Some(ending),
                };
                assert!(matches!(read_frame(&mut source).await,
                    Err(TransportError::Io(error)) if error.kind() == ErrorKind::InvalidData));
            }
            let mut source = FragmentedFrame {
                bytes: prefix,
                ending: Some(ErrorKind::PermissionDenied),
            };
            assert!(matches!(read_frame(&mut source).await,
                Err(TransportError::Io(error)) if error.kind() == ErrorKind::PermissionDenied));
        }
        let mut boundary_reset = FragmentedFrame {
            bytes: &[],
            ending: Some(ErrorKind::ConnectionReset),
        };
        assert!(matches!(read_frame(&mut boundary_reset).await,
            Err(TransportError::Io(error)) if error.kind() == ErrorKind::ConnectionReset));
    }

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
        assert_eq!(ch.sent_frame_bytes(), 0);
        ch.send(b"ping").await.expect("send");
        assert_eq!(ch.sent_frame_bytes(), 4 + 4 + 16);
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
    async fn unexpected_peer_never_receives_our_authenticated_identity() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let caller = DeviceIdentity::generate();
        let responder = DeviceIdentity::generate();
        let expected = DeviceIdentity::generate().public();
        let server =
            tokio::spawn(async move { SecureChannel::accept(server_io, &responder).await.is_ok() });
        let _ = SecureChannel::connect(client_io, &caller, Some(&expected)).await;
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(2), server)
                .await
                .expect("server terminates")
                .unwrap(),
            "an unexpected peer must not obtain the caller's authenticated identity"
        );
    }

    #[tokio::test]
    async fn denied_peer_gets_no_responder_identity_or_channel() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let caller = DeviceIdentity::generate();
        let caller_public = caller.public();
        let responder = DeviceIdentity::generate();
        let responder_public = responder.public();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let server = tokio::spawn(async move {
            SecureChannel::accept_with_admission(server_io, &responder, |peer| {
                assert_eq!(peer, &caller_public);
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                false
            })
            .await
            .is_ok()
        });
        let client = SecureChannel::connect(client_io, &caller, Some(&responder_public)).await;
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(2), server)
                .await
                .expect("denied server terminates")
                .unwrap(),
            "denied peer must not receive a channel"
        );
        assert!(matches!(client, Err(TransportError::Io(_))));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
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
