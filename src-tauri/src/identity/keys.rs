use base64::{engine::general_purpose, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
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

    pub fn verify_signature(
        public_key: &VerifyingKey,
        message: &[u8],
        signature: &[u8],
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let array: [u8; 64] = signature
            .try_into()
            .map_err(|_| "Invalid signature length")?;
        let signature = Signature::from_bytes(&array);
        let result = public_key.verify(message, &signature);
        Ok(result.is_ok())
    }
}
