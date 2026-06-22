//! Direct-message payload sealing (X3DH-style v1).
//!
//! [`seal`] encrypts a plaintext so only the recipient can read it, deriving a
//! one-time AES-256-GCM key from two X25519 Diffie-Hellman outputs:
//! `DH(sender_static, recipient_static)` (authenticates the sender to the
//! recipient) and `DH(ephemeral, recipient_static)` (fresh per message). There
//! is no forward secrecy — compromising a long-term X25519 key exposes past
//! messages — which is the accepted v1 tradeoff (Double Ratchet is Phase 3).
//!
//! The sealed bytes fill an event's `ciphertext` field. The event is separately
//! Ed25519-signed over its content (Plan 3), which binds the ciphertext to its
//! conversation and author, so this module does not bind those itself.

use crate::identity::device::DeviceIdentity;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use bincode::Options;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};
use zeroize::Zeroize;

/// Domain separator for DM key derivation.
const DM_DOMAIN: &[u8] = b"mesh-talk-dm-v1";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// A recipient-sealed message: the sender's per-message ephemeral X25519 public
/// key plus the AES-256-GCM ciphertext (including its 16-byte tag).
///
/// Wire-evolution policy: this is a SECURITY/sealed type, so [`open`] parses it
/// strictly (`reject_trailing_bytes`) — a malleable parse that ignored trailing
/// bytes would let an attacker mint distinct byte-strings that open to the same
/// plaintext (a malleability/oracle foothold). It must STAY strict; evolution
/// happens via a new framing/version, never by tolerating extra bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedEnvelope {
    pub ephemeral_pub: [u8; 32],
    pub ciphertext: Vec<u8>,
}

/// Errors from sealing / opening a DM.
#[derive(Debug)]
pub enum DmError {
    /// AEAD encryption failed (should not happen for valid inputs).
    Encrypt,
    /// AEAD decryption failed — wrong key, wrong sender, or tampered ciphertext.
    Decrypt,
    /// (De)serialization of the envelope failed.
    Serialization(String),
}

impl std::fmt::Display for DmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DmError::Encrypt => write!(f, "dm encryption failed"),
            DmError::Decrypt => write!(f, "dm decryption failed"),
            DmError::Serialization(m) => write!(f, "dm serialization error: {m}"),
        }
    }
}

impl std::error::Error for DmError {}

/// Derive the AEAD key and nonce from the two DH outputs and the public context.
///
/// - `dh1` = `DH(sender_static, recipient_static)` — authenticates the sender.
/// - `dh2` = `DH(ephemeral, recipient_static)` — fresh per message.
///
/// `info` binds the key to the sender, recipient, and ephemeral public keys (in
/// that fixed order, so direction matters) so a ciphertext cannot be
/// reinterpreted under a different triple. Callers MUST pass `dh1`/`dh2` in this
/// order on both the seal and open sides or the keys will not match.
fn derive_key_nonce(
    dh1: &[u8; 32],
    dh2: &[u8; 32],
    sender_x25519_pub: &[u8; 32],
    recipient_x25519_pub: &[u8; 32],
    ephemeral_pub: &[u8; 32],
) -> ([u8; KEY_LEN], [u8; NONCE_LEN]) {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(dh1);
    ikm[32..].copy_from_slice(dh2);

    let mut info = Vec::with_capacity(DM_DOMAIN.len() + 32 * 3); // sender|recipient|ephemeral
    info.extend_from_slice(DM_DOMAIN);
    info.extend_from_slice(sender_x25519_pub);
    info.extend_from_slice(recipient_x25519_pub);
    info.extend_from_slice(ephemeral_pub);

    // No salt: both DH outputs are already uniformly random, so HKDF-Extract
    // with a zero-length salt is sound (RFC 5869 §2.2).
    let hk = Hkdf::<Sha256>::new(None, &ikm);
    let mut okm = [0u8; KEY_LEN + NONCE_LEN];
    hk.expand(&info, &mut okm)
        .expect("hkdf okm length is valid");

    let mut key = [0u8; KEY_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    key.copy_from_slice(&okm[..KEY_LEN]);
    nonce.copy_from_slice(&okm[KEY_LEN..]);
    // Wipe the intermediate keying material — `ikm` holds both DH shared secrets
    // (incl. the static-static DH that also wraps every group key) and `okm` holds
    // the derived key+nonce — so it does not linger in freed stack memory. The
    // returned `key`/`nonce` are wiped by the caller after the AEAD op.
    ikm.zeroize();
    okm.zeroize();
    (key, nonce)
}

/// Seal `plaintext` so only the holder of `recipient_x25519_pub`'s secret can
/// read it. Returns the serialized [`SealedEnvelope`] (goes into an event's
/// `ciphertext`). The recipient authenticates the sender via the static-static
/// DH, so they must know which sender key to expect when opening.
pub fn seal(
    sender: &DeviceIdentity,
    recipient_x25519_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>, DmError> {
    let sender_static = StaticSecret::from(sender.secret_bytes().1);
    let sender_x_pub = sender.public().x25519_pub;
    let recipient_pub = PublicKey::from(*recipient_x25519_pub);

    // Fresh per-message ephemeral; compute its public key before the DH consumes it.
    let ephemeral = EphemeralSecret::random_from_rng(rand_core::OsRng);
    let ephemeral_pub = PublicKey::from(&ephemeral);

    let mut dh1 = sender_static.diffie_hellman(&recipient_pub).to_bytes();
    let mut dh2 = ephemeral.diffie_hellman(&recipient_pub).to_bytes();

    let (mut key, mut nonce) = derive_key_nonce(
        &dh1,
        &dh2,
        &sender_x_pub,
        recipient_x25519_pub,
        &ephemeral_pub.to_bytes(),
    );
    dh1.zeroize();
    dh2.zeroize();

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| DmError::Encrypt)?;
    let result = cipher.encrypt(Nonce::from_slice(&nonce), plaintext);
    key.zeroize();
    nonce.zeroize();
    let ciphertext = result.map_err(|_| DmError::Encrypt)?;

    let envelope = SealedEnvelope {
        ephemeral_pub: ephemeral_pub.to_bytes(),
        ciphertext,
    };
    bincode::serialize(&envelope).map_err(|e| DmError::Serialization(e.to_string()))
}

/// Open a sealed envelope addressed to `recipient`, authenticating it came from
/// `sender_x25519_pub`. Returns the plaintext, or [`DmError::Decrypt`] if the
/// sender key is wrong or the envelope was tampered.
pub fn open(
    recipient: &DeviceIdentity,
    sender_x25519_pub: &[u8; 32],
    envelope_bytes: &[u8],
) -> Result<Vec<u8>, DmError> {
    // Strict parse: reject any trailing bytes (fail closed). This config uses
    // fixed-int little-endian encoding, byte-identical to `bincode::serialize`
    // used in `seal`, so legitimate envelopes still round-trip.
    let envelope: SealedEnvelope = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize(envelope_bytes)
        .map_err(|e| DmError::Serialization(e.to_string()))?;

    let recipient_static = StaticSecret::from(recipient.secret_bytes().1);
    let recipient_x_pub = recipient.public().x25519_pub;
    let sender_pub = PublicKey::from(*sender_x25519_pub);
    let ephemeral_pub = PublicKey::from(envelope.ephemeral_pub);

    let mut dh1 = recipient_static.diffie_hellman(&sender_pub).to_bytes();
    let mut dh2 = recipient_static.diffie_hellman(&ephemeral_pub).to_bytes();

    let (mut key, mut nonce) = derive_key_nonce(
        &dh1,
        &dh2,
        sender_x25519_pub,
        &recipient_x_pub,
        &envelope.ephemeral_pub,
    );
    dh1.zeroize();
    dh2.zeroize();

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| DmError::Decrypt)?;
    let result = cipher.decrypt(Nonce::from_slice(&nonce), envelope.ciphertext.as_ref());
    key.zeroize();
    nonce.zeroize();
    result.map_err(|_| DmError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, Event, EventKind};
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn derivation_is_deterministic_and_input_sensitive() {
        let dh1 = [1u8; 32];
        let dh2 = [2u8; 32];
        let a = [3u8; 32];
        let b = [4u8; 32];
        let e = [5u8; 32];

        let (k1, n1) = derive_key_nonce(&dh1, &dh2, &a, &b, &e);
        let (k2, n2) = derive_key_nonce(&dh1, &dh2, &a, &b, &e);
        assert_eq!(k1, k2);
        assert_eq!(n1, n2);

        // Any change in the inputs changes the derived key.
        let (k3, _) = derive_key_nonce(&[9u8; 32], &dh2, &a, &b, &e);
        assert_ne!(k1, k3);
        let (k4, _) = derive_key_nonce(&dh1, &dh2, &a, &b, &[7u8; 32]);
        assert_ne!(k1, k4);
        // Swapping sender/recipient context changes the key (direction matters).
        let (k5, _) = derive_key_nonce(&dh1, &dh2, &b, &a, &e);
        assert_ne!(k1, k5);
        // dh2 (the ephemeral-static DH) must independently affect the key.
        let (k6, _) = derive_key_nonce(&dh1, &[9u8; 32], &a, &b, &e);
        assert_ne!(k1, k6);
        // Key and nonce are distinct derivations, not the same bytes copied twice.
        assert_ne!(&k1[..NONCE_LEN], &n1[..]);
    }

    #[test]
    fn sealed_envelope_encodes_to_golden_bytes() {
        // Pins the SealedEnvelope wire layout (default bincode: u64 length prefix on
        // the ciphertext Vec). Reordering fields or changing the bincode config
        // breaks this — which would break interop with already-deployed peers.
        let env = SealedEnvelope {
            ephemeral_pub: [8u8; 32],
            ciphertext: vec![1, 2, 3, 4],
        };
        assert_eq!(
            hex::encode(bincode::serialize(&env).unwrap()),
            "0808080808080808080808080808080808080808080808080808080808080808\
0400000000000000\
01020304"
        );
        // Round-trip on the golden value.
        let back: SealedEnvelope =
            bincode::deserialize(&bincode::serialize(&env).unwrap()).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn envelope_round_trips_through_bincode() {
        let env = SealedEnvelope {
            ephemeral_pub: [8u8; 32],
            ciphertext: vec![1, 2, 3, 4],
        };
        let bytes = bincode::serialize(&env).unwrap();
        let back: SealedEnvelope = bincode::deserialize(&bytes).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn seal_then_open_round_trips() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        let sealed = seal(&alice, &bob.public().x25519_pub, b"hello bob").unwrap();
        let opened = open(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened, b"hello bob");
    }

    #[test]
    fn open_rejects_trailing_bytes() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut sealed = seal(&alice, &bob.public().x25519_pub, b"hi").unwrap();
        // It opens cleanly as-is.
        assert!(open(&bob, &alice.public().x25519_pub, &sealed).is_ok());
        // Appending junk must make it fail (strict parse), not silently succeed.
        sealed.push(0xAB);
        assert!(open(&bob, &alice.public().x25519_pub, &sealed).is_err());
    }

    #[test]
    fn round_trips_empty_and_large_plaintexts() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Empty payload.
        let sealed = seal(&alice, &bob.public().x25519_pub, b"").unwrap();
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &sealed).unwrap(),
            b""
        );

        // Large payload.
        let big = vec![0x5au8; 100_000];
        let sealed = seal(&alice, &bob.public().x25519_pub, &big).unwrap();
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &sealed).unwrap(),
            big
        );
    }

    #[test]
    fn wrong_recipient_cannot_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let eve = DeviceIdentity::generate();

        // Sealed for Bob; Eve (using Alice's key as the claimed sender) cannot open.
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();
        assert!(matches!(
            open(&eve, &alice.public().x25519_pub, &sealed),
            Err(DmError::Decrypt)
        ));
    }

    #[test]
    fn wrong_sender_key_fails_authentication() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mallory = DeviceIdentity::generate();

        // Sealed by Alice for Bob. Bob tries to open it claiming Mallory is the
        // sender → the static-static DH mismatches → decryption fails.
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();
        assert!(matches!(
            open(&bob, &mallory.public().x25519_pub, &sealed),
            Err(DmError::Decrypt)
        ));
        // With the correct sender key it opens.
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &sealed).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();

        // Flip the last byte (inside the AEAD tag region of the serialized envelope).
        let mut bytes = sealed.clone();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert!(matches!(
            open(&bob, &alice.public().x25519_pub, &bytes),
            Err(DmError::Decrypt)
        ));
    }

    #[test]
    fn tampered_ephemeral_key_fails_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();

        // Deserialize, corrupt the ephemeral public key, re-serialize.
        let mut env: SealedEnvelope = bincode::deserialize(&sealed).unwrap();
        env.ephemeral_pub[0] ^= 0xFF;
        let tampered = bincode::serialize(&env).unwrap();
        assert!(matches!(
            open(&bob, &alice.public().x25519_pub, &tampered),
            Err(DmError::Decrypt)
        ));
    }

    #[test]
    fn each_seal_uses_a_fresh_ephemeral() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Two seals of the same plaintext differ (fresh ephemeral each time) and
        // both still open correctly.
        let s1 = seal(&alice, &bob.public().x25519_pub, b"same").unwrap();
        let s2 = seal(&alice, &bob.public().x25519_pub, b"same").unwrap();
        assert_ne!(s1, s2);
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &s1).unwrap(),
            b"same"
        );
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &s2).unwrap(),
            b"same"
        );
    }

    #[test]
    fn garbage_envelope_is_rejected_without_panic() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        // Not a valid bincode SealedEnvelope.
        assert!(open(&bob, &alice.public().x25519_pub, &[0u8; 3]).is_err());
        // Empty input.
        assert!(open(&bob, &alice.public().x25519_pub, &[]).is_err());
    }

    #[test]
    fn sealed_payload_round_trips_inside_an_event() {
        // The sealed bytes are what fills an event's `ciphertext`. Seal, place it
        // in a Message event authored by Alice, then open from the event field.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        let sealed = seal(&alice, &bob.public().x25519_pub, b"hi from a dm event").unwrap();
        let event = Event::new(
            &alice,
            ConversationId::new([1u8; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            sealed,
        );

        let opened = open(&bob, &alice.public().x25519_pub, &event.ciphertext).unwrap();
        assert_eq!(opened, b"hi from a dm event");
    }

    #[test]
    fn envelope_cannot_be_opened_in_reverse_direction() {
        // Alice seals FOR Bob. The reverse direction (Alice as recipient, Bob as
        // the claimed sender) must NOT open it — direction is bound into the key.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let sealed = seal(&alice, &bob.public().x25519_pub, b"one way").unwrap();

        assert!(matches!(
            open(&alice, &bob.public().x25519_pub, &sealed),
            Err(DmError::Decrypt)
        ));
        // The intended direction still works.
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &sealed).unwrap(),
            b"one way"
        );
    }

    #[test]
    fn envelope_for_one_recipient_cannot_be_opened_by_another() {
        // Alice seals the same message separately for Bob and for Carol. Each
        // opens only their own copy; neither can open the other's.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();

        let for_bob = seal(&alice, &bob.public().x25519_pub, b"per-recipient").unwrap();
        let for_carol = seal(&alice, &carol.public().x25519_pub, b"per-recipient").unwrap();

        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &for_bob).unwrap(),
            b"per-recipient"
        );
        assert_eq!(
            open(&carol, &alice.public().x25519_pub, &for_carol).unwrap(),
            b"per-recipient"
        );
        // Cross-open fails for both.
        assert!(matches!(
            open(&carol, &alice.public().x25519_pub, &for_bob),
            Err(DmError::Decrypt)
        ));
        assert!(matches!(
            open(&bob, &alice.public().x25519_pub, &for_carol),
            Err(DmError::Decrypt)
        ));
    }
}
