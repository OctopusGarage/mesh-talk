use crate::identity::manager::IdentityManager;
use crate::storage::errors::StorageError;
use crate::storage::serialization::{deserialize_data, serialize_data};
use base64::{engine::general_purpose, Engine as _};
use rsa::pkcs1v15::Pkcs1v15Encrypt;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use std::sync::Arc;

/// A specialized file manager that uses public key encryption instead of password-based encryption
#[derive(Clone)]
pub struct PublicKeyFileManager {
    base_path: PathBuf,
    identity_manager: Arc<IdentityManager>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyEncryptedFile {
    pub public_key_encrypted_data: String, // Base64 encoded encrypted data
}

impl PublicKeyFileManager {
    pub fn new(base_path: PathBuf, identity_manager: IdentityManager) -> Self {
        PublicKeyFileManager {
            base_path,
            identity_manager: Arc::new(identity_manager),
        }
    }

    pub fn create_user_directory(&self, username: &str) -> Result<(), StorageError> {
        let user_dir = self.user_data_path(username);

        if !user_dir.exists() {
            fs::create_dir_all(&user_dir)
                .map_err(|_| StorageError::DirectoryCreationFailed(user_dir.clone()))?;
        }

        Ok(())
    }

    pub fn user_data_path(&self, username: &str) -> PathBuf {
        self.base_path.join("users").join(username)
    }

    /// Write a file encrypted with a public key
    pub fn write_encrypted_file<T>(
        &self,
        username: &str,
        password: &str,
        filepath: &str,
        data: &T,
    ) -> Result<(), StorageError>
    where
        T: Serialize,
    {
        log::info!("=== START ENCRYPTING AND SAVING FILE ===");
        log::info!(
            "Encrypting and saving file for user '{}', path: users/{}/{}",
            username,
            username,
            filepath
        );

        // Create user directory if it doesn't exist
        self.create_user_directory(username)?;

        // Serialize the data
        let serialized_data = serialize_data(data)?;
        log::info!(
            "Serialized data size: {} bytes for user '{}', file: {}",
            serialized_data.len(),
            username,
            filepath
        );

        // Get the user's RSA encryption key pair
        let (rsa_public_key, _) = self.get_or_create_rsa_key_pair(username, password)?;
        log::debug!(
            "Retrieved RSA key pair for encryption for user '{}'",
            username
        );

        // Generate a random symmetric key (32 bytes for AES-256)
        use aes_gcm::{
            aead::{Aead, KeyInit},
            Aes256Gcm, Key, Nonce,
        };
        use rand::RngCore;

        let mut symmetric_key_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut symmetric_key_bytes);

        // Use this as the AES key
        let key = Key::<Aes256Gcm>::from_slice(&symmetric_key_bytes);
        let cipher = Aes256Gcm::new(key);

        // Generate a random nonce
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt the actual data with the symmetric key
        let ciphertext = cipher
            .encrypt(nonce, &serialized_data[..])
            .map_err(|e| StorageError::Encryption(e.to_string()))?;
        log::debug!(
            "Encrypted data with symmetric key, ciphertext size: {} bytes",
            ciphertext.len()
        );

        // Encrypt the symmetric key with the RSA public key
        let mut rng = rand::rngs::OsRng;
        let encrypted_symmetric_key = rsa_public_key
            .encrypt(&mut rng, Pkcs1v15Encrypt, &symmetric_key_bytes)
            .map_err(|e| StorageError::Encryption(e.to_string()))?;
        log::debug!(
            "Encrypted symmetric key with RSA public key, encrypted key size: {} bytes",
            encrypted_symmetric_key.len()
        );

        // Combine the encrypted symmetric key, nonce, and ciphertext
        let mut encrypted_content = Vec::new();
        encrypted_content.extend_from_slice(&(encrypted_symmetric_key.len() as u32).to_be_bytes());
        encrypted_content.extend_from_slice(&encrypted_symmetric_key);
        encrypted_content.extend_from_slice(&nonce_bytes);
        encrypted_content.extend_from_slice(&ciphertext);

        // Log the raw encrypted content before base64 encoding
        log::debug!(
            "Raw encrypted content size: {} bytes",
            encrypted_content.len()
        );
        if encrypted_content.len() <= 100 {
            log::debug!(
                "Raw encrypted content (hex): {:?}",
                hex::encode(&encrypted_content)
            );
        } else {
            log::debug!(
                "Raw encrypted content (first 100 bytes hex): {:?}",
                hex::encode(&encrypted_content[..100])
            );
        }

        // Convert to base64 for storage
        let encrypted_b64 = general_purpose::STANDARD.encode(&encrypted_content);
        log::debug!(
            "Final encrypted content base64 length: {} characters",
            encrypted_b64.len()
        );

        // Log a sample of the base64 content (but not too much)
        if encrypted_b64.len() <= 200 {
            log::debug!("Base64 encrypted content: {}", encrypted_b64);
        } else {
            log::debug!(
                "Base64 encrypted content (first 200 chars): {}",
                &encrypted_b64[..200]
            );
        }

        // Prepare the full file path
        let full_path = self.user_data_path(username).join(filepath);
        log::info!("Final file path: {}", full_path.display());

        // Create parent directories if they don't exist
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| StorageError::DirectoryCreationFailed(parent.to_path_buf()))?;
        }

        // Write the encrypted data to file
        let mut file = File::create(&full_path)?;
        file.write_all(encrypted_b64.as_bytes())?;

        log::info!("=== SUCCESSFULLY ENCRYPTED AND SAVED FILE ===");
        log::info!(
            "Successfully encrypted and saved file for user '{}' at path: {}",
            username,
            full_path.display()
        );

        Ok(())
    }

    /// Read a file encrypted with a public key (and decrypt with private key)
    pub fn read_encrypted_file<T>(
        &self,
        username: &str,
        password: &str,
        filepath: &str,
    ) -> Result<T, StorageError>
    where
        T: for<'de> Deserialize<'de>,
    {
        log::info!("=== START READING AND DECRYPTING FILE ===");
        log::info!(
            "Reading and decrypting file for user '{}', path: users/{}/{}",
            username,
            username,
            filepath
        );

        // Prepare the full file path
        let full_path = self.user_data_path(username).join(filepath);
        log::info!("Reading from file path: {}", full_path.display());

        // Check if file exists
        if !full_path.exists() {
            log::warn!("File does not exist: {}", full_path.display());
            return Err(StorageError::FileNotFound(full_path));
        }

        // Read the file as base64 string
        let mut file = File::open(&full_path)?;
        let mut encrypted_b64 = String::new();
        file.read_to_string(&mut encrypted_b64)?;
        log::debug!(
            "Read encrypted file, base64 length: {} characters",
            encrypted_b64.len()
        );

        // Decode the base64 content
        let encrypted_content = general_purpose::STANDARD
            .decode(&encrypted_b64)
            .map_err(|e| StorageError::Decryption(format!("Failed to decode base64: {}", e)))?;
        log::debug!(
            "Decoded base64 content, size: {} bytes",
            encrypted_content.len()
        );

        // Log a sample of the raw encrypted content
        if encrypted_content.len() <= 100 {
            log::debug!(
                "Raw encrypted content (hex): {:?}",
                hex::encode(&encrypted_content)
            );
        } else {
            log::debug!(
                "Raw encrypted content (first 100 bytes hex): {:?}",
                hex::encode(&encrypted_content[..100])
            );
        }

        if encrypted_content.len() < 4 {
            log::error!("Encrypted file too short (< 4 bytes)");
            return Err(StorageError::Decryption("File too short".to_string()));
        }

        // Parse the format: [4 bytes length][encrypted symmetric key][12 bytes nonce][ciphertext]
        let encrypted_key_len = u32::from_be_bytes([
            encrypted_content[0],
            encrypted_content[1],
            encrypted_content[2],
            encrypted_content[3],
        ]) as usize;
        log::debug!(
            "Encrypted symmetric key length: {} bytes",
            encrypted_key_len
        );

        if encrypted_content.len() < 4 + encrypted_key_len + 12 {
            log::error!("Encrypted file format invalid - not enough data");
            return Err(StorageError::Decryption("File format invalid".to_string()));
        }

        let start_encrypted_key = 4;
        let end_encrypted_key = start_encrypted_key + encrypted_key_len;
        let start_nonce = end_encrypted_key;
        let end_nonce = start_nonce + 12;
        let start_ciphertext = end_nonce;

        let encrypted_symmetric_key = &encrypted_content[start_encrypted_key..end_encrypted_key];
        let nonce_bytes = [
            encrypted_content[start_nonce],
            encrypted_content[start_nonce + 1],
            encrypted_content[start_nonce + 2],
            encrypted_content[start_nonce + 3],
            encrypted_content[start_nonce + 4],
            encrypted_content[start_nonce + 5],
            encrypted_content[start_nonce + 6],
            encrypted_content[start_nonce + 7],
            encrypted_content[start_nonce + 8],
            encrypted_content[start_nonce + 9],
            encrypted_content[start_nonce + 10],
            encrypted_content[start_nonce + 11],
        ];
        let ciphertext = &encrypted_content[start_ciphertext..];
        log::debug!("Ciphertext size: {} bytes", ciphertext.len());

        // Get the user's RSA private key for decryption
        let (_, rsa_private_key) = self.get_or_create_rsa_key_pair(username, password)?;
        log::debug!(
            "Retrieved RSA private key for decryption for user '{}'",
            username
        );

        // Decrypt the symmetric key using the RSA private key
        let decrypted_symmetric_key = rsa_private_key
            .decrypt(Pkcs1v15Encrypt, encrypted_symmetric_key)
            .map_err(|e| StorageError::Decryption(e.to_string()))?;
        log::debug!("Successfully decrypted symmetric key");

        // Use the decrypted symmetric key to decrypt the actual data
        use aes_gcm::{
            aead::{Aead, KeyInit},
            Aes256Gcm, Key, Nonce,
        };

        let key = Key::<Aes256Gcm>::from_slice(&decrypted_symmetric_key);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let decrypted_data = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| StorageError::Decryption(e.to_string()))?;
        log::debug!(
            "Successfully decrypted data, final size: {} bytes",
            decrypted_data.len()
        );

        // Deserialize the data
        let deserialized_data = deserialize_data(&decrypted_data)?;
        log::info!("=== SUCCESSFULLY READ AND DECRYPTED FILE ===");
        log::info!(
            "Successfully read and decrypted file for user '{}' from path: {}",
            username,
            full_path.display()
        );

        Ok(deserialized_data)
    }

    /// Get or create RSA key pair for the user for encryption purposes
    fn get_or_create_rsa_key_pair(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(RsaPublicKey, RsaPrivateKey), StorageError> {
        // Try to load existing RSA keys
        if let Ok(rsa_private_key_der_b64) = self.load_rsa_private_key(username, password) {
            let rsa_private_key_der = general_purpose::STANDARD
                .decode(&rsa_private_key_der_b64)
                .map_err(|e| StorageError::Decryption(e.to_string()))?;

            let rsa_private_key = rsa::RsaPrivateKey::from_pkcs8_der(&rsa_private_key_der)
                .map_err(|e| StorageError::Decryption(e.to_string()))?;

            let rsa_public_key = rsa_private_key.to_public_key();
            return Ok((rsa_public_key, rsa_private_key));
        }

        // If no existing keys, generate a new pair
        let mut rng = rand::rngs::OsRng;
        let rsa_bits = 2048;
        let rsa_private_key = RsaPrivateKey::new(&mut rng, rsa_bits)
            .map_err(|e| StorageError::Encryption(e.to_string()))?;
        let rsa_public_key = RsaPublicKey::from(&rsa_private_key);

        // Store the private key encrypted with the user's password
        let rsa_private_key_der = rsa_private_key
            .to_pkcs8_der()
            .map_err(|e| StorageError::Encryption(e.to_string()))?;
        let rsa_private_key_der_b64 =
            general_purpose::STANDARD.encode(rsa_private_key_der.as_bytes());

        self.save_rsa_private_key(username, password, &rsa_private_key_der_b64)
            .map_err(|e| StorageError::Encryption(e.to_string()))?;

        Ok((rsa_public_key, rsa_private_key))
    }

    /// Load RSA private key for the user
    fn load_rsa_private_key(
        &self,
        username: &str,
        _password: &str,
    ) -> Result<String, StorageError> {
        let full_path = self
            .user_data_path(username)
            .join("meta/rsa_private_key.enc");

        if !full_path.exists() {
            return Err(StorageError::FileNotFound(full_path));
        }

        let content = std::fs::read_to_string(&full_path)
            .map_err(|e| StorageError::Decryption(format!("Failed to read RSA key file: {}", e)))?;

        Ok(content)
    }

    /// Save RSA private key for the user
    fn save_rsa_private_key(
        &self,
        username: &str,
        _password: &str, // This parameter is kept for compatibility but not used
        key_b64: &str,
    ) -> Result<(), std::io::Error> {
        let full_path = self
            .user_data_path(username)
            .join("meta/rsa_private_key.enc");

        // Create parent directory if needed
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }

        std::fs::write(&full_path, key_b64.as_bytes())?;

        Ok(())
    }
}
