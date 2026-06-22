//! Device identity: Ed25519 (signing) + X25519 (key exchange) keys, with a
//! self-certifying `user_id` derived from the signing public key.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

/// The public half of a device identity — safe to share on the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicIdentity {
    /// Ed25519 verifying (signature) key.
    pub ed25519_pub: [u8; 32],
    /// X25519 public key for Diffie-Hellman key exchange.
    pub x25519_pub: [u8; 32],
}

impl PublicIdentity {
    /// Stable, self-certifying user id = hex of the first 16 bytes of
    /// SHA-256("mesh-talk-id-v1" || ed25519_pub). 32 hex chars.
    pub fn user_id(&self) -> String {
        Self::user_id_from(&self.ed25519_pub)
    }

    pub fn user_id_from(ed25519_pub: &[u8; 32]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"mesh-talk-id-v1");
        hasher.update(ed25519_pub);
        let digest = hasher.finalize();
        hex::encode(&digest[..16])
    }
}

/// A device's secret identity. Hold in memory only; persist via `keystore`.
pub struct DeviceIdentity {
    signing_key: SigningKey,
    dh_secret: StaticSecret,
}

impl DeviceIdentity {
    /// Generate a fresh identity from the OS CSPRNG.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut rand_core::OsRng);
        let dh_secret = StaticSecret::random_from_rng(rand_core::OsRng);
        Self {
            signing_key,
            dh_secret,
        }
    }

    /// Reconstruct from raw secret-key bytes (used by the keystore on load).
    pub fn from_secret_bytes(ed25519_secret: [u8; 32], x25519_secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&ed25519_secret),
            dh_secret: StaticSecret::from(x25519_secret),
        }
    }

    /// The two 32-byte secret scalars, for sealing into the keystore.
    pub fn secret_bytes(&self) -> ([u8; 32], [u8; 32]) {
        (self.signing_key.to_bytes(), self.dh_secret.to_bytes())
    }

    /// The shareable public identity.
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.signing_key.verifying_key().to_bytes(),
            x25519_pub: X25519Public::from(&self.dh_secret).to_bytes(),
        }
    }

    pub fn user_id(&self) -> String {
        self.public().user_id()
    }

    /// Sign a message with the Ed25519 signing key.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }

    /// Verify a signature against a raw Ed25519 public key.
    pub fn verify(ed25519_pub: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(ed25519_pub) else {
            return false;
        };
        // verify_strict rejects non-canonical / small-order signatures (malleability), so
        // a signature can't be massaged into a different valid form for the same message.
        // All signatures we produce via `sign` are canonical, so this never rejects ours.
        vk.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_signs_and_exposes_matching_public() {
        let id = DeviceIdentity::generate();
        let public = id.public();

        // user_id from the device matches the one derived from its public bundle.
        assert_eq!(id.user_id(), public.user_id());

        // A signature over a message verifies against the public signing key.
        let msg = b"hello mesh";
        let sig = id.sign(msg);
        assert!(DeviceIdentity::verify(&public.ed25519_pub, msg, &sig));

        // A tampered message does not verify.
        assert!(!DeviceIdentity::verify(
            &public.ed25519_pub,
            b"hello mesh!",
            &sig
        ));

        // Two generated identities differ.
        let other = DeviceIdentity::generate();
        assert_ne!(id.user_id(), other.user_id());
    }

    #[test]
    fn verify_rejects_signature_from_different_identity() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let msg = b"test message";
        let sig_from_a = a.sign(msg);

        // Verifying with A's own key succeeds.
        assert!(DeviceIdentity::verify(
            &a.public().ed25519_pub,
            msg,
            &sig_from_a
        ));
        // Verifying with B's key rejects A's signature.
        assert!(!DeviceIdentity::verify(
            &b.public().ed25519_pub,
            msg,
            &sig_from_a
        ));
    }

    #[test]
    fn user_id_is_stable_and_derived_from_signing_key() {
        let pid = PublicIdentity {
            ed25519_pub: [7u8; 32],
            x25519_pub: [9u8; 32],
        };
        let id = pid.user_id();
        assert_eq!(id.len(), 32, "user_id is 32 hex chars");
        // Deterministic: same key -> same id.
        assert_eq!(id, pid.user_id());
        // Independent of the x25519 key.
        let pid2 = PublicIdentity {
            ed25519_pub: [7u8; 32],
            x25519_pub: [0u8; 32],
        };
        assert_eq!(id, pid2.user_id());
        // Different signing key -> different id.
        let pid3 = PublicIdentity {
            ed25519_pub: [8u8; 32],
            x25519_pub: [9u8; 32],
        };
        assert_ne!(id, pid3.user_id());
    }
}
