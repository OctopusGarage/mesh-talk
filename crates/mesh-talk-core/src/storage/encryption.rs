use crate::storage::errors::StorageError;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use pbkdf2::pbkdf2_hmac;
use rand_core::RngCore;
use sha2::Sha256;
use zeroize::Zeroize;

pub const SALT_SIZE: usize = 16;
pub const NONCE_SIZE: usize = 12;
const KEY_SIZE: usize = 32;
// OWASP 2023 minimum for PBKDF2-HMAC-SHA256 protecting long-term keys.
//
// PBKDF2 dominates the debug test suite (~12s per derivation), so it is collapsed to a trivial
// cost in two TEST-ONLY situations: (a) `cfg(test)` — true only when compiling THIS crate as a
// test target; (b) the `fast-test-kdf` feature — enabled exclusively from a downstream crate's
// `[dev-dependencies]` (src-tauri), which Cargo does NOT activate for normal `cargo build`/
// release builds. So release builds and the shipped app always use the strong production value;
// there is no way to enable the cheap path in a real build.
#[cfg(not(any(test, feature = "fast-test-kdf")))]
const PBKDF2_ROUNDS: u32 = 600_000;
#[cfg(any(test, feature = "fast-test-kdf"))]
const PBKDF2_ROUNDS: u32 = 1;

pub struct EncryptionKey([u8; KEY_SIZE]);

/// Zeroize the derived key on drop so it does not linger in freed memory (mirrors
/// `GroupKey`/`RatchetState`). This is at-rest key material, never serialized, so this
/// changes no wire/on-disk format.
impl Drop for EncryptionKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl EncryptionKey {
    pub fn from_password(password: &str, salt: &[u8; SALT_SIZE]) -> Result<Self, StorageError> {
        let mut key = [0u8; KEY_SIZE];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, PBKDF2_ROUNDS, &mut key);
        Ok(EncryptionKey(key))
    }

    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }
}

pub fn generate_salt() -> [u8; SALT_SIZE] {
    let mut salt = [0u8; SALT_SIZE];
    rand_core::OsRng.fill_bytes(&mut salt);
    salt
}

pub fn generate_nonce() -> Result<[u8; NONCE_SIZE], StorageError> {
    let mut nonce = [0u8; NONCE_SIZE];
    rand_core::OsRng.fill_bytes(&mut nonce);
    Ok(nonce)
}

pub fn encrypt_data(
    data: &[u8],
    key: &EncryptionKey,
) -> Result<(Vec<u8>, [u8; NONCE_SIZE]), StorageError> {
    let nonce_bytes = generate_nonce()?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let cipher = Aes256Gcm::new(key.as_bytes().into());

    let ciphertext = cipher
        .encrypt(nonce, data)
        .map_err(|e| StorageError::Encryption(e.to_string()))?;

    Ok((ciphertext, nonce_bytes))
}

pub fn decrypt_data(
    ciphertext: &[u8],
    nonce: &[u8; NONCE_SIZE],
    key: &EncryptionKey,
) -> Result<Vec<u8>, StorageError> {
    let nonce = Nonce::from_slice(nonce);
    let cipher = Aes256Gcm::new(key.as_bytes().into());

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| StorageError::Decryption(e.to_string()))?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pw: &str, salt: &[u8; SALT_SIZE]) -> EncryptionKey {
        EncryptionKey::from_password(pw, salt).unwrap()
    }

    #[test]
    fn round_trips() {
        let salt = generate_salt();
        let k = key("correct horse", &salt);
        let (ct, nonce) = encrypt_data(b"top secret", &k).unwrap();
        assert_ne!(ct, b"top secret"); // actually encrypted
        assert_eq!(decrypt_data(&ct, &nonce, &k).unwrap(), b"top secret");
    }

    #[test]
    fn wrong_password_fails() {
        let salt = generate_salt();
        let (ct, nonce) = encrypt_data(b"data", &key("right", &salt)).unwrap();
        assert!(decrypt_data(&ct, &nonce, &key("wrong", &salt)).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let salt = generate_salt();
        let k = key("pw", &salt);
        let (mut ct, nonce) = encrypt_data(b"data", &k).unwrap();
        ct[0] ^= 0x01; // flip a bit anywhere (body or tag) — AEAD must reject
        assert!(decrypt_data(&ct, &nonce, &k).is_err());
    }

    #[test]
    fn wrong_nonce_fails() {
        let salt = generate_salt();
        let k = key("pw", &salt);
        let (ct, mut nonce) = encrypt_data(b"data", &k).unwrap();
        nonce[0] ^= 0x01;
        assert!(decrypt_data(&ct, &nonce, &k).is_err());
    }

    #[test]
    fn empty_and_short_ciphertext_fail_cleanly() {
        let salt = generate_salt();
        let k = key("pw", &salt);
        let nonce = generate_nonce().unwrap();
        // No panic on attacker-supplied empty/too-short input — just an error.
        assert!(decrypt_data(&[], &nonce, &k).is_err());
        assert!(decrypt_data(&[0u8; 4], &nonce, &k).is_err());
    }

    #[test]
    fn key_derivation_is_deterministic_and_salt_separated() {
        let salt = generate_salt();
        // Same password + salt => same key.
        assert_eq!(key("pw", &salt).as_bytes(), key("pw", &salt).as_bytes());
        // Different salt => different key (so two stores never share a key).
        let salt2 = generate_salt();
        assert_ne!(key("pw", &salt).as_bytes(), key("pw", &salt2).as_bytes());
        // Different password => different key.
        assert_ne!(key("pw", &salt).as_bytes(), key("pw2", &salt).as_bytes());
    }

    #[test]
    fn fresh_nonces_differ() {
        // Nonce reuse under a fixed AES-GCM key is catastrophic; each encrypt must draw
        // a fresh one.
        let salt = generate_salt();
        let k = key("pw", &salt);
        let (_, n1) = encrypt_data(b"x", &k).unwrap();
        let (_, n2) = encrypt_data(b"x", &k).unwrap();
        assert_ne!(n1, n2);
    }
}
