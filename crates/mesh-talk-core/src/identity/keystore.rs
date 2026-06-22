//! Password-encrypted, on-disk keystore for a `DeviceIdentity`.
//!
//! Format: salt(16) || nonce(12) || AES-256-GCM ciphertext of the serialized
//! secret keys. Key derived from the password via PBKDF2 (see
//! `storage::encryption`).

use crate::identity::device::DeviceIdentity;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use crate::storage::errors::StorageError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct StoredKeys {
    ed25519_secret: [u8; 32],
    x25519_secret: [u8; 32],
}

/// Encrypt `identity` with `password` and write it to `path` (creating parent
/// directories as needed).
pub fn save(path: &Path, password: &str, identity: &DeviceIdentity) -> Result<(), StorageError> {
    let (ed, dh) = identity.secret_bytes();
    let stored = StoredKeys {
        ed25519_secret: ed,
        x25519_secret: dh,
    };
    let plaintext =
        bincode::serialize(&stored).map_err(|e| StorageError::Serialization(e.to_string()))?;

    let salt = generate_salt();
    let key = EncryptionKey::from_password(password, &salt)?;
    let (ciphertext, nonce) = encrypt_data(&plaintext, &key)?;

    let mut out = Vec::with_capacity(SALT_SIZE + NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| StorageError::DirectoryCreationFailed(parent.to_path_buf()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &out)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read and decrypt the keystore at `path` with `password`.
pub fn load(path: &Path, password: &str) -> Result<DeviceIdentity, StorageError> {
    let content = std::fs::read(path)?;
    if content.len() < SALT_SIZE + NONCE_SIZE {
        return Err(StorageError::Decryption(
            "keystore file too short".to_string(),
        ));
    }
    let salt: [u8; SALT_SIZE] = content[0..SALT_SIZE]
        .try_into()
        .expect("length checked above");
    let nonce: [u8; NONCE_SIZE] = content[SALT_SIZE..SALT_SIZE + NONCE_SIZE]
        .try_into()
        .expect("length checked above");
    let ciphertext = &content[SALT_SIZE + NONCE_SIZE..];

    let key = EncryptionKey::from_password(password, &salt)?;
    let plaintext = decrypt_data(ciphertext, &nonce, &key)?;
    let stored: StoredKeys = bincode::deserialize(&plaintext)
        .map_err(|e| StorageError::Deserialization(e.to_string()))?;

    Ok(DeviceIdentity::from_secret_bytes(
        stored.ed25519_secret,
        stored.x25519_secret,
    ))
}

/// Load the keystore if present, otherwise generate a new identity and save it.
pub fn load_or_create(path: &Path, password: &str) -> Result<DeviceIdentity, StorageError> {
    if path.exists() {
        load(path, password)
    } else {
        let identity = DeviceIdentity::generate();
        save(path, password, &identity)?;
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_the_identity() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");

        let original = DeviceIdentity::generate();
        save(&path, "correct horse battery staple", &original).expect("save");

        let loaded = load(&path, "correct horse battery staple").expect("load");
        assert_eq!(original.user_id(), loaded.user_id());
        assert_eq!(original.public(), loaded.public());
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");
        let id = DeviceIdentity::generate();
        save(&path, "right", &id).expect("save");

        assert!(load(&path, "wrong").is_err());
    }

    #[test]
    fn load_or_create_is_stable_across_calls() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");

        let first = load_or_create(&path, "pw").expect("first");
        let second = load_or_create(&path, "pw").expect("second");
        assert_eq!(first.user_id(), second.user_id());
    }

    #[test]
    fn tampered_ciphertext_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");
        let id = DeviceIdentity::generate();
        save(&path, "pw", &id).expect("save");

        let mut bytes = std::fs::read(&path).expect("read");
        // Flip a byte in the ciphertext region (after salt+nonce = 28 bytes).
        bytes[28] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("write tampered");

        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn truncated_file_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");

        // Fewer than 28 bytes — must return Err, not panic.
        std::fs::write(&path, [0u8; 10]).expect("write short");
        assert!(load(&path, "pw").is_err());

        // Exactly 28 bytes — empty ciphertext — must also return Err.
        std::fs::write(&path, [0u8; 28]).expect("write 28-byte");
        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn loaded_identity_can_sign_and_verify() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");
        let original = DeviceIdentity::generate();
        save(&path, "pw", &original).expect("save");

        let loaded = load(&path, "pw").expect("load");
        let msg = b"roundtrip msg";
        let sig = loaded.sign(msg);
        assert!(DeviceIdentity::verify(
            &loaded.public().ed25519_pub,
            msg,
            &sig
        ));
    }
}
