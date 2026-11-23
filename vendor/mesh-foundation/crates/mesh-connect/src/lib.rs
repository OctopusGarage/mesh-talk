//! Pinned-key Noise sessions over reliable, ordered byte streams.

pub mod discovery;
pub mod frame;
pub mod handshake;
pub mod net;
pub mod session;

use frame::{read_frame, write_frame};
use handshake::Handshake;
use session::Session;
use tokio::io::{AsyncRead, AsyncWrite};

pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Maximum size of a single Noise message on the wire (Noise spec limit).
pub const MAX_FRAME: usize = 65535;

/// Maximum plaintext per frame = `MAX_FRAME` minus the 16-byte AEAD tag.
pub const MAX_PLAINTEXT: usize = MAX_FRAME - 16;

/// Errors from the transport layer.
#[derive(Debug)]
pub enum TransportError {
    /// Underlying Noise/`snow` failure (handshake or AEAD).
    Noise(String),
    /// I/O failure on the byte stream.
    Io(std::io::Error),
    /// A declared frame length exceeded [`MAX_FRAME`].
    FrameTooLarge(usize),
    /// Plaintext exceeded [`MAX_PLAINTEXT`] for a single frame.
    PlaintextTooLarge(usize),
    /// Handshake ended without a peer static key.
    MissingRemoteStatic,
    /// Identity auth failed (bad signature or static-key mismatch).
    IdentityMismatch,
    /// Authenticated peer was not the one the caller expected.
    UnexpectedPeer,
    /// The host's admission policy rejected the verified caller.
    AdmissionDenied,
    /// (De)serialization of the auth payload failed.
    Serialization(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Noise(m) => write!(f, "noise error: {m}"),
            TransportError::Io(e) => write!(f, "io error: {e}"),
            TransportError::FrameTooLarge(n) => write!(f, "frame too large: {n} bytes"),
            TransportError::PlaintextTooLarge(n) => write!(f, "plaintext too large: {n} bytes"),
            TransportError::MissingRemoteStatic => {
                write!(f, "handshake produced no remote static key")
            }
            TransportError::IdentityMismatch => write!(f, "peer identity authentication failed"),
            TransportError::UnexpectedPeer => {
                write!(f, "authenticated peer was not the expected one")
            }
            TransportError::AdmissionDenied => write!(f, "peer admission denied"),
            TransportError::Serialization(m) => write!(f, "serialization error: {m}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<snow::Error> for TransportError {
    fn from(e: snow::Error) -> Self {
        TransportError::Noise(e.to_string())
    }
}

const PEER_SESSION_PROLOGUE: &[u8] = b"mesh-connect/pinned-session/1";

/// An encrypted peer session with local admission. Drop the session on I/O error
/// or if an in-flight frame operation is cancelled.
pub struct PeerSession<S> {
    stream: S,
    cipher: Session,
    peer_key: [u8; 32],
}

impl<S: AsyncRead + AsyncWrite + Unpin> PeerSession<S> {
    /// Dial a peer with a previously trusted X25519 public key.
    pub async fn connect(
        mut stream: S,
        secret: &[u8; 32],
        expected_peer: &[u8; 32],
    ) -> Result<Self, TransportError> {
        let mut handshake = Handshake::build(secret, true, PEER_SESSION_PROLOGUE)?;
        write_frame(&mut stream, &handshake.write_message()?).await?;
        handshake.read_message(&read_frame(&mut stream).await?)?;
        if &handshake.remote_static()? != expected_peer {
            return Err(TransportError::UnexpectedPeer);
        }
        write_frame(&mut stream, &handshake.write_message()?).await?;
        let output = handshake.into_session()?;
        Ok(Self {
            stream,
            cipher: output.session,
            peer_key: output.remote_static,
        })
    }

    /// Authenticate the initiating static key and apply local admission policy.
    pub async fn accept(
        mut stream: S,
        secret: &[u8; 32],
        admit: impl FnOnce(&[u8; 32]) -> bool,
    ) -> Result<Self, TransportError> {
        let mut handshake = Handshake::build(secret, false, PEER_SESSION_PROLOGUE)?;
        handshake.read_message(&read_frame(&mut stream).await?)?;
        write_frame(&mut stream, &handshake.write_message()?).await?;
        handshake.read_message(&read_frame(&mut stream).await?)?;
        let output = handshake.into_session()?;
        if !admit(&output.remote_static) {
            return Err(TransportError::AdmissionDenied);
        }
        Ok(Self {
            stream,
            cipher: output.session,
            peer_key: output.remote_static,
        })
    }

    pub fn peer_key(&self) -> &[u8; 32] {
        &self.peer_key
    }

    pub async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        let ciphertext = self.cipher.encrypt(frame)?;
        write_frame(&mut self.stream, &ciphertext).await
    }

    pub async fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
        self.cipher.decrypt(&read_frame(&mut self.stream).await?)
    }
}
