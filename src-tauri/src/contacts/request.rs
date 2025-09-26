use crate::identity::keys::KeyManager;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

/// Contact request data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactRequest {
    /// Public key of the requester
    pub requester_public_key: String,
    /// Alias or name the requester wants to be known by
    pub requester_alias: String,
    /// Timestamp of the request
    pub timestamp: u64,
    /// Signature to verify authenticity
    pub signature: Vec<u8>,
}

impl ContactRequest {
    /// Create a new contact request
    pub fn new(
        requester_public_key: String,
        requester_alias: String,
        signing_key: &SigningKey,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        let request = ContactRequest {
            requester_public_key,
            requester_alias,
            timestamp,
            signature: Vec::new(), // Will be filled in after signing
        };

        // Sign the request
        let serialized = serde_json::to_string(&request)?;
        let signature = KeyManager::sign_message(signing_key, serialized.as_bytes())?;

        Ok(ContactRequest {
            requester_public_key: request.requester_public_key,
            requester_alias: request.requester_alias,
            timestamp: request.timestamp,
            signature,
        })
    }

    /// Verify the signature of the contact request
    pub fn verify_signature(&self) -> Result<bool, Box<dyn std::error::Error>> {
        // Deserialize the public key
        let public_key = KeyManager::deserialize_public_key(&self.requester_public_key)?;

        // Create a copy without the signature for verification
        let request_without_signature = ContactRequest {
            requester_public_key: self.requester_public_key.clone(),
            requester_alias: self.requester_alias.clone(),
            timestamp: self.timestamp,
            signature: Vec::new(),
        };

        // Serialize for verification
        let serialized = serde_json::to_string(&request_without_signature)?;

        // Verify the signature
        KeyManager::verify_signature(&public_key, serialized.as_bytes(), &self.signature)
    }

    /// Serialize the contact request to JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize the contact request from JSON
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Contact request response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactResponse {
    /// Public key of the responder
    pub responder_public_key: String,
    /// Whether the request was approved
    pub approved: bool,
    /// Alias or name the responder wants to be known by
    pub responder_alias: String,
    /// Timestamp of the response
    pub timestamp: u64,
    /// Signature to verify authenticity
    pub signature: Vec<u8>,
}

impl ContactResponse {
    /// Create a new contact response
    pub fn new(
        responder_public_key: String,
        approved: bool,
        responder_alias: String,
        signing_key: &SigningKey,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        let response = ContactResponse {
            responder_public_key,
            approved,
            responder_alias,
            timestamp,
            signature: Vec::new(), // Will be filled in after signing
        };

        // Sign the response
        let serialized = serde_json::to_string(&response)?;
        let signature = KeyManager::sign_message(signing_key, serialized.as_bytes())?;

        Ok(ContactResponse {
            responder_public_key: response.responder_public_key,
            approved: response.approved,
            responder_alias: response.responder_alias,
            timestamp: response.timestamp,
            signature,
        })
    }

    /// Verify the signature of the contact response
    pub fn verify_signature(&self) -> Result<bool, Box<dyn std::error::Error>> {
        // Deserialize the public key
        let public_key = KeyManager::deserialize_public_key(&self.responder_public_key)?;

        // Create a copy without the signature for verification
        let response_without_signature = ContactResponse {
            responder_public_key: self.responder_public_key.clone(),
            approved: self.approved,
            responder_alias: self.responder_alias.clone(),
            timestamp: self.timestamp,
            signature: Vec::new(),
        };

        // Serialize for verification
        let serialized = serde_json::to_string(&response_without_signature)?;

        // Verify the signature
        KeyManager::verify_signature(&public_key, serialized.as_bytes(), &self.signature)
    }

    /// Serialize the contact response to JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize the contact response from JSON
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contact_request_creation_and_verification() {
        let (public_key, signing_key) = KeyManager::generate_keypair().unwrap();
        let public_key_str = KeyManager::serialize_public_key(&public_key);

        let request =
            ContactRequest::new(public_key_str.clone(), "Alice".to_string(), &signing_key).unwrap();

        assert_eq!(request.requester_public_key, public_key_str);
        assert_eq!(request.requester_alias, "Alice");
        assert!(request.timestamp > 0);
        assert!(!request.signature.is_empty());

        // Verify the signature
        assert!(request.verify_signature().unwrap());
    }

    #[test]
    fn test_contact_response_creation_and_verification() {
        let (public_key, signing_key) = KeyManager::generate_keypair().unwrap();
        let public_key_str = KeyManager::serialize_public_key(&public_key);

        let response = ContactResponse::new(
            public_key_str.clone(),
            true,
            "Bob".to_string(),
            &signing_key,
        )
        .unwrap();

        assert_eq!(response.responder_public_key, public_key_str);
        assert_eq!(response.responder_alias, "Bob");
        assert!(response.timestamp > 0);
        assert!(!response.signature.is_empty());
        assert!(response.approved);

        // Verify the signature
        assert!(response.verify_signature().unwrap());
    }

    #[test]
    fn test_contact_request_serialization() {
        let (public_key, signing_key) = KeyManager::generate_keypair().unwrap();
        let public_key_str = KeyManager::serialize_public_key(&public_key);

        let request =
            ContactRequest::new(public_key_str.clone(), "Alice".to_string(), &signing_key).unwrap();

        let json = request.to_json().unwrap();
        let deserialized = ContactRequest::from_json(&json).unwrap();

        assert_eq!(
            request.requester_public_key,
            deserialized.requester_public_key
        );
        assert_eq!(request.requester_alias, deserialized.requester_alias);
        assert_eq!(request.timestamp, deserialized.timestamp);
        assert_eq!(request.signature, deserialized.signature);
    }

    #[test]
    fn test_contact_response_serialization() {
        let (public_key, signing_key) = KeyManager::generate_keypair().unwrap();
        let public_key_str = KeyManager::serialize_public_key(&public_key);

        let response = ContactResponse::new(
            public_key_str.clone(),
            true,
            "Bob".to_string(),
            &signing_key,
        )
        .unwrap();

        let json = response.to_json().unwrap();
        let deserialized = ContactResponse::from_json(&json).unwrap();

        assert_eq!(
            response.responder_public_key,
            deserialized.responder_public_key
        );
        assert_eq!(response.responder_alias, deserialized.responder_alias);
        assert_eq!(response.timestamp, deserialized.timestamp);
        assert_eq!(response.signature, deserialized.signature);
        assert_eq!(response.approved, deserialized.approved);
    }
}
