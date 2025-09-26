//! Crypto module for Mesh-Talk
//!
//! This module provides end-to-end encryption for all messages using the Signal Protocol.

pub mod keys;
pub mod session;
pub mod signal;
pub mod storage;

#[cfg(test)]
mod tests;

/// Crypto module error types
#[derive(Debug, Clone, PartialEq)]
pub enum CryptoError {
    /// Failed to initialize Signal protocol
    SignalInitializationError(String),
    /// Failed to generate or manage keys
    KeyManagementError(String),
    /// Failed to establish or maintain session
    SessionError(String),
    /// Failed to encrypt or decrypt message
    EncryptionError(String),
    /// Failed to access secure storage
    StorageError(String),
    /// Internal error
    InternalError(String),
}

/// Crypto result type
pub type CryptoResult<T> = Result<T, CryptoError>;
