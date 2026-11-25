//! The Double Ratchet key schedule: root-chain step, symmetric-chain step, and
//! per-message AEAD key/nonce derivation. All HKDF-SHA256 over 32-byte keys.
//! Historical KDF labels are fixed cryptographic profile bytes. Changing
//! them changes derived keys; a new profile requires explicit version negotiation.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize;

/// Root KDF: derive the next root key + a new chain key from the current root key
/// and a fresh DH output. `salt = rk`, `ikm = dh_out`. The HKDF output buffer is wiped
/// before return; the returned keys are owned by the caller.
pub fn kdf_rk(rk: &[u8; 32], dh_out: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    derive_pair(rk, dh_out, b"MeshTalk-Ratchet-RK")
}

/// Chain KDF: derive the next chain key + a single-use message key from a chain key.
/// `salt = ck`, `ikm = empty`.
pub fn kdf_ck(ck: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    derive_pair(ck, &[], b"MeshTalk-Ratchet-CK")
}

fn derive_pair(salt: &[u8; 32], input: &[u8], label: &[u8]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(salt), input);
    let mut material = [0u8; 64];
    hk.expand(label, &mut material)
        .expect("64 is a valid HKDF length");
    let (first, second) = material.split_at(32);
    let mut next = [0u8; 32];
    let mut derived = [0u8; 32];
    next.copy_from_slice(first);
    derived.copy_from_slice(second);
    material.zeroize();
    (next, derived)
}

/// Derive the AES-256-GCM key + 96-bit nonce for one message from its message key.
pub fn message_keys(mk: &[u8; 32]) -> ([u8; 32], [u8; 12]) {
    let hk = Hkdf::<Sha256>::new(Some(mk), &[]);
    let mut okm = [0u8; 44];
    hk.expand(b"MeshTalk-Ratchet-MSG", &mut okm)
        .expect("44 is a valid HKDF length");
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    key.copy_from_slice(&okm[..32]);
    nonce.copy_from_slice(&okm[32..]);
    okm.zeroize();
    (key, nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected bytes independently calculated with Python hmac/hashlib HKDF.
    // These pin the historical profile, not merely repeat its implementation.
    #[test]
    fn historical_profile_known_answers() {
        let (root, chain) = kdf_rk(&[1; 32], &[2; 32]);
        assert_eq!(
            hex::encode([root, chain].concat()),
            "9a1f8e71d9d1ed2e4a9c5da71f58aa699c563ee04350321b0621498d3677e052c710ec66bd0a85ddaa89cf24f79fad5d32dbdcd948e060544d40f35a9ad7eb07"
        );
        let (chain, message) = kdf_ck(&[3; 32]);
        assert_eq!(
            hex::encode([chain, message].concat()),
            "84fb02f9f1e1ed67e990bba3368d61e261090273bb7cacb7fe7ab240fd8c73e603d99d6bd4eedd6a4f5f21392a1f7678acf90d7c05dc91f6dc09f0c140de0288"
        );
        let (key, nonce) = message_keys(&[4; 32]);
        assert_eq!(
            hex::encode([key.as_slice(), nonce.as_slice()].concat()),
            "e732b25796bcd2ff49b744bd2be44eb0e053b5a4aa24e78bb73f423f7db4a7028c257a261d76857de9a03b15"
        );
    }

    #[test]
    fn kdf_steps_are_deterministic_and_distinct() {
        let rk = [1u8; 32];
        let dh = [2u8; 32];
        let (rk1, ck1) = kdf_rk(&rk, &dh);
        let (rk2, ck2) = kdf_rk(&rk, &dh);
        assert_eq!((rk1, ck1), (rk2, ck2)); // deterministic
        assert_ne!(rk1, ck1); // root and chain outputs differ
        assert_ne!(rk1, rk); // advances

        let (cka, mka) = kdf_ck(&ck1);
        let (ckb, mkb) = kdf_ck(&ck1);
        assert_eq!((cka, mka), (ckb, mkb));
        assert_ne!(cka, mka);
        assert_ne!(cka, ck1);

        let (k1, n1) = message_keys(&mka);
        let (k2, n2) = message_keys(&mka);
        assert_eq!((k1, n1), (k2, n2));
        // different message keys → different aead keys (advance chain to get a distinct mk)
        let (_, mk_next) = kdf_ck(&cka);
        let (k3, _) = message_keys(&mk_next);
        assert_ne!(k1, k3);
    }
}
