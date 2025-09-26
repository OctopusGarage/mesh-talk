use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Encryption(String),
    Serialization(String),
    Decryption(String),
    Deserialization(String),
    InvalidPath(PathBuf),
    FileNotFound(PathBuf),
    DirectoryCreationFailed(PathBuf),
    InvalidPassword,
    KeyDerivationFailed,
    NonceGenerationFailed,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io(err) => write!(f, "IO error: {}", err),
            StorageError::Encryption(msg) => write!(f, "Encryption error: {}", msg),
            StorageError::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            StorageError::Decryption(msg) => write!(f, "Decryption error: {}", msg),
            StorageError::Deserialization(msg) => write!(f, "Deserialization error: {}", msg),
            StorageError::InvalidPath(path) => write!(f, "Invalid path: {:?}", path),
            StorageError::FileNotFound(path) => write!(f, "File not found: {:?}", path),
            StorageError::DirectoryCreationFailed(path) => {
                write!(f, "Failed to create directory: {:?}", path)
            }
            StorageError::InvalidPassword => write!(f, "Invalid password"),
            StorageError::KeyDerivationFailed => write!(f, "Key derivation failed"),
            StorageError::NonceGenerationFailed => write!(f, "Nonce generation failed"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        StorageError::Io(error)
    }
}
