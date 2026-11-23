//! The Double Ratchet key schedule: root-chain step, symmetric-chain step, and
//! per-message AEAD key/nonce derivation. All HKDF-SHA256 over 32-byte keys.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize;

/// Root KDF: derive the next root key + a new chain key from the current root key
/// and a fresh DH output. `salt = rk`, `ikm = dh_out`. The HKDF output buffer is wiped
/// before return (the returned keys are owned by the caller) — consistent with `dm.rs`.
pub fn kdf_rk(rk: &[u8; 32], dh_out: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(rk), dh_out);
    let mut okm = [0u8; 64];
    hk.expand(b"MeshTalk-Ratchet-RK", &mut okm)
        .expect("64 is a valid HKDF length");
    let mut rk2 = [0u8; 32];
    let mut ck = [0u8; 32];
    rk2.copy_from_slice(&okm[..32]);
    ck.copy_from_slice(&okm[32..]);
    okm.zeroize();
    (rk2, ck)
}

/// Chain KDF: derive the next chain key + a single-use message key from a chain key.
/// `salt = ck`, `ikm = empty`.
pub fn kdf_ck(ck: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(ck), &[]);
    let mut okm = [0u8; 64];
    hk.expand(b"MeshTalk-Ratchet-CK", &mut okm)
        .expect("64 is a valid HKDF length");
    let mut ck2 = [0u8; 32];
    let mut mk = [0u8; 32];
    ck2.copy_from_slice(&okm[..32]);
    mk.copy_from_slice(&okm[32..]);
    okm.zeroize();
    (ck2, mk)
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
