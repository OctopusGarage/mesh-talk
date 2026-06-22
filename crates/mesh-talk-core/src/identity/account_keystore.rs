//! Password-encrypted, on-disk keystore for an `Account` secret (the cross-device
//! Ed25519 account key). Separate file from the device keystore: the account key
//! is shared across a user's devices and is transferred at link time, so it has
//! its own lifecycle.
//!
//! Format: salt(16) || nonce(12) || AES-256-GCM ciphertext of the serialized
//! secret. Key derived from the password via PBKDF2 (see `storage::encryption`).

use crate::identity::account::Account;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use crate::storage::errors::StorageError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct StoredAccount {
    ed25519_secret: [u8; 32],
}

/// Encrypt `account` with `password` and write it to `path` (creating parent
/// directories as needed).
pub fn save(path: &Path, password: &str, account: &Account) -> Result<(), StorageError> {
    let stored = StoredAccount {
        ed25519_secret: account.secret_bytes(),
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

/// Read and decrypt the account keystore at `path` with `password`.
pub fn load(path: &Path, password: &str) -> Result<Account, StorageError> {
    let content = std::fs::read(path)?;
    if content.len() < SALT_SIZE + NONCE_SIZE {
        return Err(StorageError::Decryption(
            "account keystore file too short".to_string(),
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
    let stored: StoredAccount = bincode::deserialize(&plaintext)
        .map_err(|e| StorageError::Deserialization(e.to_string()))?;

    Ok(Account::from_secret_bytes(stored.ed25519_secret))
}

/// Load the account if present, otherwise generate a new one and save it.
pub fn load_or_create(path: &Path, password: &str) -> Result<Account, StorageError> {
    if path.exists() {
        load(path, password)
    } else {
        let account = Account::generate();
        save(path, password, &account)?;
        Ok(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_the_account() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");

        let original = Account::generate();
        save(&path, "correct horse battery staple", &original).expect("save");

        let loaded = load(&path, "correct horse battery staple").expect("load");
        assert_eq!(original.account_id(), loaded.account_id());
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "right", &Account::generate()).expect("save");
        assert!(load(&path, "wrong").is_err());
    }

    #[test]
    fn load_or_create_is_stable_across_calls() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");

        let first = load_or_create(&path, "pw").expect("first");
        let second = load_or_create(&path, "pw").expect("second");
        assert_eq!(first.account_id(), second.account_id());
    }

    #[test]
    fn tampered_ciphertext_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "pw", &Account::generate()).expect("save");

        let mut bytes = std::fs::read(&path).expect("read");
        bytes[28] ^= 0xFF; // flip a byte in the ciphertext region (after salt+nonce = 28)
        std::fs::write(&path, &bytes).expect("write tampered");
        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn truncated_file_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        std::fs::write(&path, [0u8; 10]).expect("write short");
        assert!(load(&path, "pw").is_err());
        std::fs::write(&path, [0u8; 28]).expect("write 28-byte");
        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn loaded_account_can_certify() {
        use crate::identity::device::DeviceIdentity;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "pw", &Account::generate()).expect("save");

        let loaded = load(&path, "pw").expect("load");
        let device = DeviceIdentity::generate();
        assert!(loaded.certify(&device.public().ed25519_pub).verify());
    }
}
