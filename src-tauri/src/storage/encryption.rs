use crate::storage::errors::StorageError;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use sha2::Sha256;

const SALT_SIZE: usize = 16;
const NONCE_SIZE: usize = 12;
const KEY_SIZE: usize = 32;
const PBKDF2_ROUNDS: u32 = 100_000;

pub struct EncryptionKey([u8; KEY_SIZE]);

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
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

pub fn generate_nonce() -> Result<[u8; NONCE_SIZE], StorageError> {
    let mut nonce = [0u8; NONCE_SIZE];
    rand::thread_rng().fill_bytes(&mut nonce);
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
