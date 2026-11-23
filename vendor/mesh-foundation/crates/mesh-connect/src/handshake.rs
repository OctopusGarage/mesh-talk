//! The Noise XX handshake state machine, wrapping `snow`. Drives the three
//! XX messages and, on completion, yields a [`Session`] plus the peer's
//! authenticated X25519 static key and the channel-binding handshake hash.

use crate::session::Session;
use crate::{MAX_FRAME, NOISE_PARAMS, TransportError};

/// One side of an in-progress Noise XX handshake.
pub struct Handshake {
    state: snow::HandshakeState,
}

/// The result of a completed handshake.
pub struct HandshakeOutput {
    /// The transport session for `send`/`recv`.
    pub session: Session,
    /// The peer's X25519 static public key (what Noise authenticated).
    pub remote_static: [u8; 32],
    /// The Noise channel-binding hash (32 bytes for BLAKE2s) — identical on
    /// both ends, unique per session. Signed during identity auth.
    pub handshake_hash: [u8; 32],
}

impl Handshake {
    /// Start the handshake as the initiator (dialing side).
    pub fn initiator(x25519_secret: &[u8; 32]) -> Result<Self, TransportError> {
        Self::build(x25519_secret, true, &[])
    }

    /// Start the handshake as the responder (listening side).
    pub fn responder(x25519_secret: &[u8; 32]) -> Result<Self, TransportError> {
        Self::build(x25519_secret, false, &[])
    }

    pub(crate) fn build(
        x25519_secret: &[u8; 32],
        initiator: bool,
        prologue: &[u8],
    ) -> Result<Self, TransportError> {
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|_| TransportError::Noise("invalid noise params".into()))?;
        let builder = snow::Builder::new(params)
            .local_private_key(x25519_secret)
            .prologue(prologue);
        let state = if initiator {
            builder.build_initiator()?
        } else {
            builder.build_responder()?
        };
        Ok(Self { state })
    }

    /// Produce the next handshake message (payload is empty for our use).
    pub fn write_message(&mut self) -> Result<Vec<u8>, TransportError> {
        let mut buf = vec![0u8; MAX_FRAME];
        let len = self.state.write_message(&[], &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Consume an incoming handshake message.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), TransportError> {
        let mut buf = vec![0u8; MAX_FRAME];
        self.state.read_message(message, &mut buf)?;
        Ok(())
    }

    /// Whether the handshake has completed.
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// The static key authenticated by the messages received so far. On the
    /// initiator this is available after message two, before revealing its own
    /// static key in message three.
    pub fn remote_static(&self) -> Result<[u8; 32], TransportError> {
        self.state
            .get_remote_static()
            .ok_or(TransportError::MissingRemoteStatic)?
            .try_into()
            .map_err(|_| TransportError::Noise("remote static key is not 32 bytes".into()))
    }

    /// Finish: capture the binding hash and peer static key, then switch to
    /// transport mode.
    pub fn into_session(self) -> Result<HandshakeOutput, TransportError> {
        if !self.state.is_handshake_finished() {
            return Err(TransportError::Noise("handshake not yet finished".into()));
        }
        let handshake_hash: [u8; 32] = self
            .state
            .get_handshake_hash()
            .try_into()
            .map_err(|_| TransportError::Noise("handshake hash is not 32 bytes".into()))?;
        let remote_static: [u8; 32] = self
            .state
            .get_remote_static()
            .ok_or(TransportError::MissingRemoteStatic)?
            // try_into fails only on a non-32-byte key; unreachable for XX + X25519.
            .try_into()
            .map_err(|_| TransportError::Noise("remote static key is not 32 bytes".into()))?;
        let transport = self.state.into_transport_mode()?;
        Ok(HandshakeOutput {
            session: Session::new(transport),
            remote_static,
            handshake_hash,
        })
    }
}
