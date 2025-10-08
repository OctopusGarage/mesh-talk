#[cfg(test)]
mod tests {
    use crate::crypto::keys::KeyManager;
    use crate::crypto::session::SessionManager;
    use crate::crypto::signal::SignalContext;
    use crate::crypto::storage::SecureStorageManager;

    // RSA dependencies for the encryption/decryption tests
    use rand::rngs::OsRng;
    use rsa::traits::PublicKeyParts;
    use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};

    #[test]
    fn test_crypto_module_compilation() {
        // This test ensures that the crypto module compiles correctly
        // It doesn't test the actual functionality, just that the code is valid

        // Test that we can create instances of the crypto types
        let _signal_context = SignalContext::new();
        let _key_manager = KeyManager::new();
        let _storage_manager = SecureStorageManager::new();
        let _session_manager = SessionManager::new();
    }

    #[test]
    fn test_rsa_encryption_decryption() {
        // Test RSA public key encryption and private key decryption
        use rand::rngs::OsRng;
        use rsa::traits::PublicKeyParts;
        use rsa::Pkcs1v15Encrypt;
        use rsa::{RsaPrivateKey, RsaPublicKey};

        // Generate a new RSA key pair
        let mut rng = OsRng;
        let bits = 1024; // Use smaller key for faster tests
        let private_key = RsaPrivateKey::new(&mut rng, bits).expect("Failed to generate a key");
        let public_key = RsaPublicKey::from(&private_key);

        // Message to encrypt
        let message = "Hello, this is a secret message!";

        // Encrypt with the public key
        let encrypted_data = public_key
            .encrypt(&mut rng, Pkcs1v15Encrypt, message.as_bytes())
            .expect("Failed to encrypt");

        // Decrypt with the private key
        let decrypted_data = private_key
            .decrypt(Pkcs1v15Encrypt, &encrypted_data)
            .expect("Failed to decrypt");

        let decrypted_message =
            String::from_utf8(decrypted_data).expect("Failed to convert to string");

        // Verify that the original and decrypted messages match
        assert_eq!(message, decrypted_message);
    }

    #[test]
    fn test_multiple_rsa_encryption_decryption_cycles() {
        // Test multiple messages with RSA encryption/decryption
        use rand::rngs::OsRng;
        use rsa::Pkcs1v15Encrypt;
        use rsa::{RsaPrivateKey, RsaPublicKey};

        let mut rng = OsRng;
        let bits = 1024;
        let private_key = RsaPrivateKey::new(&mut rng, bits).expect("Failed to generate a key");
        let public_key = RsaPublicKey::from(&private_key);

        let messages = vec![
            "First test message",
            "Second test message with different content",
            "Third message to verify consistency",
            "Short",
        ];

        for message in messages {
            // Encrypt
            let encrypted_data = public_key
                .encrypt(&mut rng, Pkcs1v15Encrypt, message.as_bytes())
                .expect("Failed to encrypt");

            // Decrypt
            let decrypted_data = private_key
                .decrypt(Pkcs1v15Encrypt, &encrypted_data)
                .expect("Failed to decrypt");

            let decrypted_message =
                String::from_utf8(decrypted_data).expect("Failed to convert to string");

            assert_eq!(message, decrypted_message);
        }
    }
}
