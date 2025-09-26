//! Custom error types for the Mesh-Talk application
use std::fmt;

/// Custom error type for the Mesh-Talk application
#[derive(Debug)]
pub enum MeshTalkError {
    /// Network-related errors
    NetworkError {
        kind: NetworkErrorKind,
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Database-related errors
    DatabaseError {
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Authentication-related errors
    AuthError {
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Message-related errors
    MessageError {
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// General application errors
    ApplicationError {
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

/// Specific kinds of network errors
#[derive(Debug, Clone, PartialEq)]
pub enum NetworkErrorKind {
    ConnectionFailed,
    ConnectionTimeout,
    SendFailed,
    ReceiveFailed,
    InvalidMessage,
    ProtocolViolation,
    DiscoveryFailed,
}

impl MeshTalkError {
    /// Create a new network error
    pub fn network(kind: NetworkErrorKind, message: impl Into<String>) -> Self {
        MeshTalkError::NetworkError {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Create a new network error with a source error
    pub fn network_with_source(
        kind: NetworkErrorKind,
        message: impl Into<String>,
        source: Box<dyn std::error::Error + Send + Sync>,
    ) -> Self {
        MeshTalkError::NetworkError {
            kind,
            message: message.into(),
            source: Some(source),
        }
    }

    /// Create a new database error
    pub fn database(message: impl Into<String>) -> Self {
        MeshTalkError::DatabaseError {
            message: message.into(),
            source: None,
        }
    }

    /// Create a new database error with a source error
    pub fn database_with_source(
        message: impl Into<String>,
        source: Box<dyn std::error::Error + Send + Sync>,
    ) -> Self {
        MeshTalkError::DatabaseError {
            message: message.into(),
            source: Some(source),
        }
    }

    /// Create a new authentication error
    pub fn auth(message: impl Into<String>) -> Self {
        MeshTalkError::AuthError {
            message: message.into(),
            source: None,
        }
    }

    /// Create a new message error
    pub fn message(message: impl Into<String>) -> Self {
        MeshTalkError::MessageError {
            message: message.into(),
            source: None,
        }
    }

    /// Create a new application error
    pub fn application(message: impl Into<String>) -> Self {
        MeshTalkError::ApplicationError {
            message: message.into(),
            source: None,
        }
    }
}

impl fmt::Display for MeshTalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeshTalkError::NetworkError { kind, message, .. } => {
                write!(f, "Network error ({:?}): {}", kind, message)
            }
            MeshTalkError::DatabaseError { message, .. } => {
                write!(f, "Database error: {}", message)
            }
            MeshTalkError::AuthError { message, .. } => {
                write!(f, "Authentication error: {}", message)
            }
            MeshTalkError::MessageError { message, .. } => {
                write!(f, "Message error: {}", message)
            }
            MeshTalkError::ApplicationError { message, .. } => {
                write!(f, "Application error: {}", message)
            }
        }
    }
}

impl std::error::Error for MeshTalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MeshTalkError::NetworkError { source, .. } => source
                .as_ref()
                .map(|e| e.as_ref() as &(dyn std::error::Error + 'static)),
            MeshTalkError::DatabaseError { source, .. } => source
                .as_ref()
                .map(|e| e.as_ref() as &(dyn std::error::Error + 'static)),
            MeshTalkError::AuthError { source, .. } => source
                .as_ref()
                .map(|e| e.as_ref() as &(dyn std::error::Error + 'static)),
            MeshTalkError::MessageError { source, .. } => source
                .as_ref()
                .map(|e| e.as_ref() as &(dyn std::error::Error + 'static)),
            MeshTalkError::ApplicationError { source, .. } => source
                .as_ref()
                .map(|e| e.as_ref() as &(dyn std::error::Error + 'static)),
        }
    }
}

/// Result type alias for MeshTalk operations
pub type MeshTalkResult<T> = Result<T, MeshTalkError>;
