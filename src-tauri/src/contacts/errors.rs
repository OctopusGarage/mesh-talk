use std::fmt;

#[derive(Debug)]
pub enum ContactError {
    ContactAlreadyExists(String),
    ContactNotFound(String),
    InvalidPublicKey,
    StorageError(String),
    IdentityError(String),
    NetworkError(String),
    SerializationError(String),
    InvalidAlias,
}

impl fmt::Display for ContactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContactError::ContactAlreadyExists(public_key) => {
                write!(f, "Contact with public key '{}' already exists", public_key)
            }
            ContactError::ContactNotFound(public_key) => {
                write!(f, "Contact with public key '{}' not found", public_key)
            }
            ContactError::InvalidPublicKey => write!(f, "Invalid public key"),
            ContactError::StorageError(msg) => write!(f, "Storage error: {}", msg),
            ContactError::IdentityError(msg) => write!(f, "Identity error: {}", msg),
            ContactError::NetworkError(msg) => write!(f, "Network error: {}", msg),
            ContactError::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            ContactError::InvalidAlias => write!(f, "Invalid alias"),
        }
    }
}

impl std::error::Error for ContactError {}
