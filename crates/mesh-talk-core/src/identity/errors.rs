use std::fmt;

#[derive(Debug)]
pub enum IdentityError {
    UserAlreadyExists(String),
    UserNotFound(String),
    InvalidPassword,
    KeyGenerationFailed,
    KeyEncryptionFailed,
    KeyDecryptionFailed,
    SerializationError(String),
    StorageError(String),
    InvalidUsername,
    InvalidUserId,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::UserAlreadyExists(username) => {
                write!(f, "User '{}' already exists", username)
            }
            IdentityError::UserNotFound(username) => write!(f, "User '{}' not found", username),
            IdentityError::InvalidPassword => write!(f, "Invalid password"),
            IdentityError::KeyGenerationFailed => write!(f, "Failed to generate key pair"),
            IdentityError::KeyEncryptionFailed => write!(f, "Failed to encrypt private key"),
            IdentityError::KeyDecryptionFailed => write!(f, "Failed to decrypt private key"),
            IdentityError::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            IdentityError::StorageError(msg) => write!(f, "Storage error: {}", msg),
            IdentityError::InvalidUsername => write!(f, "Invalid username"),
            IdentityError::InvalidUserId => write!(f, "Invalid user ID"),
        }
    }
}

impl std::error::Error for IdentityError {}
