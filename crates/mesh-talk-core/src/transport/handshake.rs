//! The Noise XX handshake state machine, wrapping `snow`. Drives the three
//! XX messages and, on completion, yields a [`Session`] plus the peer's
//! authenticated X25519 static key and the channel-binding handshake hash.

use crate::transport::session::Session;
use crate::transport::{TransportError, MAX_FRAME, NOISE_PARAMS};

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
        Self::build(x25519_secret, true)
    }

    /// Start the handshake as the responder (listening side).
    pub fn responder(x25519_secret: &[u8; 32]) -> Result<Self, TransportError> {
        Self::build(x25519_secret, false)
    }

    fn build(x25519_secret: &[u8; 32], initiator: bool) -> Result<Self, TransportError> {
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|_| TransportError::Noise("invalid noise params".into()))?;
        let builder = snow::Builder::new(params).local_private_key(x25519_secret);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    /// Drive the full XX handshake in memory and return both completed outputs.
    fn run_handshake(
        initiator_secret: &[u8; 32],
        responder_secret: &[u8; 32],
    ) -> (HandshakeOutput, HandshakeOutput) {
        let mut ini = Handshake::initiator(initiator_secret).expect("initiator");
        let mut res = Handshake::responder(responder_secret).expect("responder");

        // XX: -> e ; <- e, ee, s, es ; -> s, se
        let m1 = ini.write_message().expect("m1");
        res.read_message(&m1).expect("read m1");
        let m2 = res.write_message().expect("m2");
        ini.read_message(&m2).expect("read m2");
        let m3 = ini.write_message().expect("m3");
        res.read_message(&m3).expect("read m3");

        assert!(ini.is_finished() && res.is_finished());
        (
            ini.into_session().expect("ini session"),
            res.into_session().expect("res session"),
        )
    }

    #[test]
    fn handshake_binds_static_keys_and_agrees_on_hash() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (out_a, out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        // Both ends derive the identical channel-binding hash.
        assert_eq!(out_a.handshake_hash, out_b.handshake_hash);
        assert_eq!(out_a.handshake_hash.len(), 32);

        // Each side authenticated the other's real X25519 static key.
        assert_eq!(out_a.remote_static, b.public().x25519_pub);
        assert_eq!(out_b.remote_static, a.public().x25519_pub);
    }

    #[test]
    fn sessions_round_trip_messages_both_directions() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (mut out_a, mut out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        let ct = out_a
            .session
            .encrypt(b"hello from a")
            .expect("encrypt a->b");
        assert_eq!(
            out_b.session.decrypt(&ct).expect("decrypt"),
            b"hello from a"
        );

        let ct2 = out_b
            .session
            .encrypt(b"hello from b")
            .expect("encrypt b->a");
        assert_eq!(
            out_a.session.decrypt(&ct2).expect("decrypt"),
            b"hello from b"
        );
    }

    #[test]
    fn tampered_ciphertext_fails_to_decrypt() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (mut out_a, mut out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        // Flip a ciphertext-body byte.
        let mut ct = out_a.session.encrypt(b"secret").expect("encrypt");
        ct[0] ^= 0xFF;
        assert!(out_b.session.decrypt(&ct).is_err());

        // Flip a tag byte on a fresh message — must also fail.
        let mut ct2 = out_a.session.encrypt(b"secret two").expect("encrypt");
        let last = ct2.len() - 1;
        ct2[last] ^= 0xFF;
        assert!(out_b.session.decrypt(&ct2).is_err());
    }

    #[test]
    fn replayed_ciphertext_fails_to_decrypt() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (mut out_a, mut out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        let ct = out_a.session.encrypt(b"hello").expect("encrypt");
        out_b.session.decrypt(&ct).expect("first decrypt ok");
        // Replaying the same ciphertext uses a stale nonce → must fail.
        assert!(out_b.session.decrypt(&ct).is_err());
    }

    #[test]
    fn into_session_before_finished_returns_err() {
        let a = DeviceIdentity::generate();
        let ini = Handshake::initiator(&a.secret_bytes().1).unwrap();
        // No messages exchanged — into_session must not succeed.
        assert!(ini.into_session().is_err());
    }

    #[test]
    fn oversized_plaintext_is_rejected() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (mut out_a, _out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        let big = vec![0u8; crate::transport::MAX_PLAINTEXT + 1];
        assert!(matches!(
            out_a.session.encrypt(&big),
            Err(TransportError::PlaintextTooLarge(_))
        ));
    }
}
