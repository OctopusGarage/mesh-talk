//! Identity-binding auth exchange. After the Noise handshake, each side proves
//! its X25519 static key belongs to its Ed25519 identity by signing the
//! per-session handshake hash. This makes the channel's `user_id` meaningful.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::transport::TransportError;
use serde::{Deserialize, Serialize};

/// Domain separator so these signatures can never be mistaken for any other
/// signature the identity key produces.
pub const AUTH_DOMAIN: &[u8] = b"mesh-talk-transport-auth-v1";

/// What each side sends (encrypted) during the auth exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMessage {
    /// The sender's public identity (Ed25519 + X25519 keys).
    pub public: PublicIdentity,
    /// Ed25519 signature over `AUTH_DOMAIN || handshake_hash`. 64 bytes; held
    /// as a `Vec<u8>` because serde does not derive for `[u8; 64]`.
    pub signature: Vec<u8>,
}

fn signing_input(handshake_hash: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(AUTH_DOMAIN.len() + handshake_hash.len());
    input.extend_from_slice(AUTH_DOMAIN);
    input.extend_from_slice(handshake_hash);
    input
}

/// Build our auth message for a completed handshake.
pub fn build_auth(identity: &DeviceIdentity, handshake_hash: &[u8; 32]) -> AuthMessage {
    let signature = identity.sign(&signing_input(handshake_hash)).to_vec();
    AuthMessage {
        public: identity.public(),
        signature,
    }
}

/// Verify a peer's auth message against this session's handshake hash and the
/// X25519 static key Noise authenticated. Returns the verified [`PublicIdentity`].
pub fn verify_auth(
    msg: &AuthMessage,
    handshake_hash: &[u8; 32],
    remote_static: &[u8; 32],
) -> Result<PublicIdentity, TransportError> {
    // The advertised X25519 key must be the one Noise actually authenticated.
    if &msg.public.x25519_pub != remote_static {
        return Err(TransportError::IdentityMismatch);
    }
    // The signature must be 64 bytes and verify against the advertised Ed25519 key.
    let signature: [u8; 64] = msg
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| TransportError::IdentityMismatch)?;
    if !DeviceIdentity::verify(
        &msg.public.ed25519_pub,
        &signing_input(handshake_hash),
        &signature,
    ) {
        return Err(TransportError::IdentityMismatch);
    }
    Ok(msg.public.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_then_verify_round_trips() {
        let id = DeviceIdentity::generate();
        let hh = [42u8; 32];
        let msg = build_auth(&id, &hh);

        let verified = verify_auth(&msg, &hh, &id.public().x25519_pub).expect("verify");
        assert_eq!(verified, id.public());
        assert_eq!(verified.user_id(), id.user_id());
    }

    #[test]
    fn verify_rejects_different_handshake_hash() {
        // A signature captured from one session must not validate in another.
        let id = DeviceIdentity::generate();
        let msg = build_auth(&id, &[1u8; 32]);
        assert!(matches!(
            verify_auth(&msg, &[2u8; 32], &id.public().x25519_pub),
            Err(TransportError::IdentityMismatch)
        ));
    }

    #[test]
    fn verify_rejects_static_key_mismatch() {
        // Advertised x25519 key differs from the Noise-authenticated static key.
        let id = DeviceIdentity::generate();
        let hh = [7u8; 32];
        let msg = build_auth(&id, &hh);
        let wrong_static = [0u8; 32];
        assert!(matches!(
            verify_auth(&msg, &hh, &wrong_static),
            Err(TransportError::IdentityMismatch)
        ));
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let id = DeviceIdentity::generate();
        let hh = [9u8; 32];
        let mut msg = build_auth(&id, &hh);
        msg.signature[0] ^= 0xFF;
        assert!(matches!(
            verify_auth(&msg, &hh, &id.public().x25519_pub),
            Err(TransportError::IdentityMismatch)
        ));
    }

    #[test]
    fn verify_rejects_wrong_length_signature() {
        let id = DeviceIdentity::generate();
        let hh = [3u8; 32];
        let mut msg = build_auth(&id, &hh);
        msg.signature.truncate(10);
        assert!(matches!(
            verify_auth(&msg, &hh, &id.public().x25519_pub),
            Err(TransportError::IdentityMismatch)
        ));
    }

    #[test]
    fn verify_rejects_sig_from_a_presented_as_identity_b() {
        // A valid signature from A, but the advertised identity is swapped to B.
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let hh = [5u8; 32];
        let mut msg = build_auth(&a, &hh);
        msg.public = b.public();
        // Verifying against B's static key: the signature was made by A's
        // ed25519 key, so verification under B's advertised ed25519 key fails.
        assert!(matches!(
            verify_auth(&msg, &hh, &b.public().x25519_pub),
            Err(TransportError::IdentityMismatch)
        ));
    }
}
