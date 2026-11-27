//! Local credential codecs over SDK-owned cryptographic signing material.
//! Local users and their identifiers/lifecycle remain application-owned.

use base64::{Engine as _, engine::general_purpose};
use ed25519_dalek::VerifyingKey;
use mesh_identity::account::Account as SigningMaterial;
use std::convert::TryInto;

// SDK signing material owns secrets; local User identifiers and codecs remain app-owned.

pub struct KeyManager;

impl KeyManager {
    pub fn generate_keypair() -> Result<(VerifyingKey, SigningMaterial), Box<dyn std::error::Error>>
    {
        let signing_key = SigningMaterial::generate();
        let verifying_key = VerifyingKey::from_bytes(&signing_key.public().ed25519_pub)?;
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

    pub fn serialize_secret_key(secret_key: &SigningMaterial) -> Vec<u8> {
        secret_key.secret_bytes().to_vec()
    }

    pub fn deserialize_secret_key(
        data: &[u8],
    ) -> Result<SigningMaterial, Box<dyn std::error::Error>> {
        let array: [u8; 32] = data.try_into().map_err(|_| "Invalid key length")?;
        let secret_key = SigningMaterial::from_secret_bytes(array);
        Ok(secret_key)
    }

    pub fn sign_message(
        signing_key: &SigningMaterial,
        message: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let signature = signing_key.sign(message);
        Ok(signature.to_vec())
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
        assert_eq!(sk.secret_bytes(), sk2.secret_bytes());

        // A signature from the secret key verifies under the public key.
        let sig = Signature::from_slice(&KeyManager::sign_message(&sk, b"hello").unwrap()).unwrap();
        assert!(vk.verify(b"hello", &sig).is_ok());
        assert!(vk.verify(b"tampered", &sig).is_err());
    }

    #[test]
    fn sdk_material_restores_rfc8032_vector() {
        // RFC 8032 section 7.1, test 1: independent frozen key/signature fixture.
        let secret =
            hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
                .unwrap();
        let material: mesh_identity::account::Account =
            KeyManager::deserialize_secret_key(&secret).unwrap();
        assert_eq!(KeyManager::serialize_secret_key(&material), secret);
        let public =
            KeyManager::deserialize_public_key("11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=")
                .unwrap();
        assert_eq!(public.to_bytes(), material.public().ed25519_pub);
        assert_eq!(
            hex::encode(KeyManager::sign_message(&material, b"").unwrap()),
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        );
        let (_, generated): (VerifyingKey, mesh_identity::account::Account) =
            KeyManager::generate_keypair().unwrap();
        assert_eq!(KeyManager::serialize_secret_key(&generated).len(), 32);
    }

    #[test]
    fn deserialize_rejects_bad_input() {
        // Not valid base64, and valid base64 of the wrong length.
        assert!(KeyManager::deserialize_public_key("!!! not base64 !!!").is_err());
        assert!(KeyManager::deserialize_secret_key(&[0u8; 33]).is_err());
        let short = general_purpose::STANDARD.encode([0u8; 10]);
        assert!(KeyManager::deserialize_public_key(&short).is_err());
        assert!(KeyManager::deserialize_secret_key(&[0u8; 10]).is_err());
    }
}
