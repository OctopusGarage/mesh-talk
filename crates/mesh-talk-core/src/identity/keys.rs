use base64::{engine::general_purpose, Engine as _};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand_core::OsRng;
use std::convert::TryInto;

pub struct KeyManager;

impl KeyManager {
    pub fn generate_keypair() -> Result<(VerifyingKey, SigningKey), Box<dyn std::error::Error>> {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        Ok((verifying_key, signing_key))
    }

    pub fn serialize_public_key(public_key: &VerifyingKey) -> String {
        general_purpose::STANDARD.encode(public_key.to_bytes())
    }

    pub fn deserialize_public_key(data: &str) -> Result<VerifyingKey, Box<dyn std::error::Error>> {
        let bytes = general_purpose::STANDARD.decode(data)?;
        let array: [u8; 32] = bytes.try_into().map_err(|_| "Invalid key length")?;
        let public_key = VerifyingKey::from_bytes(&array)?;
        Ok(public_key)
    }

    pub fn serialize_secret_key(secret_key: &SigningKey) -> Vec<u8> {
        secret_key.to_bytes().to_vec()
    }

    pub fn deserialize_secret_key(data: &[u8]) -> Result<SigningKey, Box<dyn std::error::Error>> {
        let array: [u8; 32] = data.try_into().map_err(|_| "Invalid key length")?;
        let secret_key = SigningKey::from_bytes(&array);
        Ok(secret_key)
    }

    pub fn sign_message(
        signing_key: &SigningKey,
        message: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let signature = signing_key.sign(message);
        Ok(signature.to_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    #[test]
    fn keypair_serializes_round_trips_and_signs() {
        let (vk, sk) = KeyManager::generate_keypair().unwrap();

        // Public key round-trips through base64; secret key through raw bytes.
        let vk2 =
            KeyManager::deserialize_public_key(&KeyManager::serialize_public_key(&vk)).unwrap();
        assert_eq!(vk.to_bytes(), vk2.to_bytes());
        let sk2 =
            KeyManager::deserialize_secret_key(&KeyManager::serialize_secret_key(&sk)).unwrap();
        assert_eq!(sk.to_bytes(), sk2.to_bytes());

        // A signature from the secret key verifies under the public key.
        let sig = Signature::from_slice(&KeyManager::sign_message(&sk, b"hello").unwrap()).unwrap();
        assert!(vk.verify(b"hello", &sig).is_ok());
        assert!(vk.verify(b"tampered", &sig).is_err());
    }

    #[test]
    fn deserialize_rejects_bad_input() {
        // Not valid base64, and valid base64 of the wrong length.
        assert!(KeyManager::deserialize_public_key("!!! not base64 !!!").is_err());
        let short = general_purpose::STANDARD.encode([0u8; 10]);
        assert!(KeyManager::deserialize_public_key(&short).is_err());
        assert!(KeyManager::deserialize_secret_key(&[0u8; 10]).is_err());
    }
}
