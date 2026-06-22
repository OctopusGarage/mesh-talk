//! Authenticated, end-to-end-encrypted peer transport.
//!
//! A Noise XX handshake (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) keyed by the
//! device's X25519 identity key establishes a [`session::Session`]; a short
//! identity-auth exchange ([`auth`]) then binds the Noise static key to the
//! device's Ed25519 identity. [`channel::SecureChannel`] drives the whole
//! thing over any async stream with length-prefixed framing.

pub mod auth;
pub mod channel;
pub mod handshake;
/// Low-level socket plumbing (UDP discovery socket, multicast joins, dual-stack
/// TCP listener) shared by discovery and the node runtime. Dependency-free of the
/// higher layers, so it sits at the bottom of the stack.
pub mod net;
pub mod session;

pub use channel::SecureChannel;
pub use session::Session;

/// Noise pattern: XX (mutual static-key auth), X25519 DH, ChaChaPoly AEAD,
/// BLAKE2s hash.
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

#[cfg(test)]
mod tests;
