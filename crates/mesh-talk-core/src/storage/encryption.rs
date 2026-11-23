//! Compatibility adapter for Mesh Talk's at-rest record encryption API.

use super::errors::StorageError;

pub use mesh_storage::{NONCE_SIZE, SALT_SIZE, generate_salt};

pub struct EncryptionKey(mesh_storage::EncryptionKey);

impl EncryptionKey {
    pub fn from_password(password: &str, salt: &[u8; SALT_SIZE]) -> Result<Self, StorageError> {
        Ok(Self(derive_record_key(password, salt)?))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

pub(super) fn derive_record_key(
    password: &str,
    salt: &[u8; SALT_SIZE],
) -> Result<mesh_storage::EncryptionKey, mesh_storage::StorageError> {
    #[cfg(not(any(test, feature = "fast-test-kdf")))]
    let key = mesh_storage::EncryptionKey::from_password(password, salt)?;
    #[cfg(any(test, feature = "fast-test-kdf"))]
    let key = {
        let mut bytes = zeroize::Zeroizing::new([0; 32]);
        ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            std::num::NonZeroU32::MIN,
            salt,
            password.as_bytes(),
            &mut *bytes,
        );
        mesh_storage::EncryptionKey::from_derived_key(*bytes)
    };
    Ok(key)
}

impl From<mesh_storage::StorageError> for StorageError {
    fn from(error: mesh_storage::StorageError) -> Self {
        match error {
            mesh_storage::StorageError::Encryption(message) => Self::Encryption(message),
            mesh_storage::StorageError::Decryption(message) => Self::Decryption(message),
        }
    }
}

pub fn generate_nonce() -> Result<[u8; NONCE_SIZE], StorageError> {
    Ok(mesh_storage::generate_nonce()?)
}

pub fn encrypt_data(
    data: &[u8],
    key: &EncryptionKey,
) -> Result<(Vec<u8>, [u8; NONCE_SIZE]), StorageError> {
    Ok(mesh_storage::encrypt_data(data, &key.0)?)
}

pub fn decrypt_data(
    ciphertext: &[u8],
    nonce: &[u8; NONCE_SIZE],
    key: &EncryptionKey,
) -> Result<Vec<u8>, StorageError> {
    Ok(mesh_storage::decrypt_data(ciphertext, nonce, &key.0)?)
}
