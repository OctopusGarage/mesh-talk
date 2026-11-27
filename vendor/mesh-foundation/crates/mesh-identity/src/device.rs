//! Device identity: Ed25519 (signing) + X25519 (key exchange) keys, with a
//! self-certifying node ID derived from the signing public key.
//! Account enrollment and application user identifiers belong to the host.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

use super::random_signing_key;

#[cfg(test)]
thread_local! {
    static PUBLIC_DERIVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn derive_x_public(secret: &StaticSecret) -> [u8; 32] {
    #[cfg(test)]
    PUBLIC_DERIVATIONS.with(|calls| calls.set(calls.get() + 1));
    X25519Public::from(secret).to_bytes()
}

/// The public half of a device identity — safe to share on the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicIdentity {
    /// Ed25519 verifying (signature) key.
    pub ed25519_pub: [u8; 32],
    /// X25519 public key for Diffie-Hellman key exchange.
    pub x25519_pub: [u8; 32],
}

impl PublicIdentity {
    /// Stable, self-certifying node id = hex of the first 16 bytes of
    /// SHA-256("mesh-foundation-node-id-v1" || ed25519_pub). 32 hex chars.
    pub fn node_id(&self) -> String {
        Self::node_id_from(&self.ed25519_pub)
    }

    pub fn node_id_from(ed25519_pub: &[u8; 32]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"mesh-foundation-node-id-v1");
        hasher.update(ed25519_pub);
        let digest = hasher.finalize();
        hex::encode(&digest[..16])
    }
}

/// A device's secret identity. Hosts must protect any persisted secret-key bytes.
pub struct DeviceIdentity {
    signing_key: SigningKey,
    dh_secret: StaticSecret,
    public: PublicIdentity,
}

impl DeviceIdentity {
    /// Generate a fresh identity from the OS CSPRNG.
    pub fn generate() -> Self {
        Self::from_keys(random_signing_key(), StaticSecret::random())
    }

    /// Reconstruct from raw secret-key bytes (after host-controlled secure loading).
    pub fn from_secret_bytes(ed25519_secret: [u8; 32], x25519_secret: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&ed25519_secret);
        let dh_secret = StaticSecret::from(x25519_secret);
        Self::from_keys(signing_key, dh_secret)
    }

    fn from_keys(signing_key: SigningKey, dh_secret: StaticSecret) -> Self {
        let public = PublicIdentity {
            ed25519_pub: signing_key.verifying_key().to_bytes(),
            x25519_pub: derive_x_public(&dh_secret),
        };
        Self {
            signing_key,
            dh_secret,
            public,
        }
    }

    /// The two 32-byte secret scalars, for host-controlled secure persistence.
    pub fn secret_bytes(&self) -> ([u8; 32], [u8; 32]) {
        (self.signing_key.to_bytes(), self.dh_secret.to_bytes())
    }

    /// The shareable public identity.
    pub fn public(&self) -> PublicIdentity {
        self.public.clone()
    }

    pub fn node_id(&self) -> String {
        self.public.node_id()
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

        // node_id from the device matches the one derived from its public bundle.
        assert_eq!(id.node_id(), public.node_id());

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
        assert_ne!(id.node_id(), other.node_id());
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
    fn node_id_is_stable_and_derived_from_signing_key() {
        let pid = PublicIdentity {
            ed25519_pub: [7u8; 32],
            x25519_pub: [9u8; 32],
        };
        let id = pid.node_id();
        assert_eq!(id.len(), 32, "node_id is 32 hex chars");
        // Deterministic: same key -> same id.
        assert_eq!(id, pid.node_id());
        // Independent of the x25519 key.
        let pid2 = PublicIdentity {
            ed25519_pub: [7u8; 32],
            x25519_pub: [0u8; 32],
        };
        assert_eq!(id, pid2.node_id());
        // Different signing key -> different id.
        let pid3 = PublicIdentity {
            ed25519_pub: [8u8; 32],
            x25519_pub: [9u8; 32],
        };
        assert_ne!(id, pid3.node_id());
    }

    fn assert_public_reads_are_derived_once(identity: DeviceIdentity) {
        let (ed, x) = identity.secret_bytes();
        let expected = PublicIdentity {
            ed25519_pub: SigningKey::from_bytes(&ed).verifying_key().to_bytes(),
            x25519_pub: X25519Public::from(&StaticSecret::from(x)).to_bytes(),
        };
        let expected_id = expected.node_id();
        let message = b"immutable public identity fixture";
        let signature = identity.sign(message);
        assert!(DeviceIdentity::verify(
            &expected.ed25519_pub,
            message,
            &signature
        ));
        let other = StaticSecret::from([31; 32]);
        let expected_shared = identity
            .dh_secret
            .diffie_hellman(&X25519Public::from(&other));
        let actual_shared = other.diffie_hellman(&X25519Public::from(identity.public().x25519_pub));
        assert!(expected_shared.as_bytes() == actual_shared.as_bytes());
        let mut observed = Vec::new();
        for count in [32, 128] {
            PUBLIC_DERIVATIONS.with(|calls| calls.set(0));
            for _ in 0..count {
                assert!(identity.public() == expected);
                assert!(identity.node_id() == expected_id);
            }
            observed.push(PUBLIC_DERIVATIONS.with(|calls| calls.get()));
        }
        assert_eq!(
            observed,
            [0, 0],
            "immutable public and device-ID reads must not repeat X25519 derivation"
        );
    }

    #[test]
    fn generated_public_reads_reuse_constructor_derivation() {
        assert_public_reads_are_derived_once(DeviceIdentity::generate());
    }

    #[test]
    fn restored_public_reads_reuse_constructor_derivation() {
        assert_public_reads_are_derived_once(DeviceIdentity::from_secret_bytes([7; 32], [8; 32]));
    }

    #[test]
    fn public_clone_mutation_cannot_change_identity_or_its_signatures() {
        for identity in [
            DeviceIdentity::generate(),
            DeviceIdentity::from_secret_bytes([7; 32], [8; 32]),
        ] {
            let original = identity.public();
            let mut returned = original.clone();
            returned.ed25519_pub[0] ^= 1;
            returned.x25519_pub[0] ^= 1;
            assert!(returned != original);
            assert!(identity.public() == original);
            let message = b"public clone isolation";
            assert!(DeviceIdentity::verify(
                &original.ed25519_pub,
                message,
                &identity.sign(message)
            ));
        }
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DeviceIdentity>();
    }
}

#[cfg(test)]
mod strict_signature_tests {
    use super::DeviceIdentity;
    #[test]
    fn noncanonical_scalar_and_small_order_identity_are_rejected() {
        let device = DeviceIdentity::from_secret_bytes([3; 32], [2; 32]);
        let message = b"strict verifier contract";
        let mut signature = device.sign(message);
        assert!(DeviceIdentity::verify(
            &device.public().ed25519_pub,
            message,
            &signature
        ));
        signature[32..].copy_from_slice(
            &hex::decode("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010")
                .unwrap(),
        );
        assert!(!DeviceIdentity::verify(
            &device.public().ed25519_pub,
            message,
            &signature
        ));
        let mut weak_key = [0; 32];
        weak_key[0] = 1;
        let mut weak_signature = [0; 64];
        weak_signature[0] = 1;
        assert!(!DeviceIdentity::verify(&weak_key, message, &weak_signature));
    }
}
