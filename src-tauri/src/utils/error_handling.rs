//! Common error handling utilities for Mesh-Talk
//!
//! This module provides shared utilities for consistent error handling
//! and conversion patterns across the application.

use crate::commands::CommandError;
use crate::error::MeshTalkError;

/// Converts a MeshTalkError to a CommandError with appropriate categorization
pub fn map_mesh_talk_error_to_command_error(error: MeshTalkError) -> CommandError {
    match error {
        MeshTalkError::NetworkError { kind, message, .. } => {
            CommandError::Network(format!("Network error ({:?}): {}", kind, message))
        }
        MeshTalkError::DatabaseError { message, .. } => {
            CommandError::Service(format!("Database error: {}", message))
        }
        MeshTalkError::AuthError { message, .. } => {
            CommandError::Authentication(format!("Authentication error: {}", message))
        }
        MeshTalkError::MessageError { message, .. } => {
            CommandError::Service(format!("Message error: {}", message))
        }
        MeshTalkError::ApplicationError { message, .. } => {
            CommandError::Service(format!("Application error: {}", message))
        }
    }
}

/// Converts a std::io::Error to a CommandError
pub fn map_io_error_to_command_error(error: std::io::Error) -> CommandError {
    CommandError::Network(format!("IO error: {}", error))
}

/// Converts a serde_json::Error to a CommandError
pub fn map_json_error_to_command_error(error: serde_json::Error) -> CommandError {
    CommandError::Validation(format!("JSON error: {}", error))
}

/// Creates a standardized validation error
pub fn validation_error(message: impl Into<String>) -> CommandError {
    CommandError::Validation(message.into())
}

/// Creates a standardized authentication error
pub fn authentication_error(message: impl Into<String>) -> CommandError {
    CommandError::Authentication(message.into())
}

/// Creates a standardized service error
pub fn service_error(message: impl Into<String>) -> CommandError {
    CommandError::Service(message.into())
}

/// Creates a standardized network error
pub fn network_error(message: impl Into<String>) -> CommandError {
    CommandError::Network(message.into())
}

/// Creates a standardized authorization error
pub fn authorization_error(message: impl Into<String>) -> CommandError {
    CommandError::Authorization(message.into())
}

/// Wraps a result with a mapping function for error conversion
pub fn map_result<T, E, F, M>(
    result: Result<T, E>,
    success_mapper: F,
    error_mapper: M,
) -> Result<T, CommandError>
where
    F: FnOnce(T) -> T,
    M: FnOnce(E) -> CommandError,
{
    match result {
        Ok(value) => Ok(success_mapper(value)),
        Err(error) => Err(error_mapper(error)),
    }
}

/// Extension trait for Result types to provide convenient error mapping
pub trait ResultExt<T, E> {
    /// Maps errors using a provided closure
    fn map_err_to_command(self, mapper: impl FnOnce(E) -> CommandError) -> Result<T, CommandError>;

    /// Maps IO errors to command errors
    fn map_io_err(self) -> Result<T, CommandError>
    where
        E: Into<std::io::Error>;

    /// Maps MeshTalk errors to command errors
    fn map_mesh_err(self) -> Result<T, CommandError>
    where
        E: Into<MeshTalkError>;
}

impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn map_err_to_command(self, mapper: impl FnOnce(E) -> CommandError) -> Result<T, CommandError> {
        self.map_err(mapper)
    }

    fn map_io_err(self) -> Result<T, CommandError>
    where
        E: Into<std::io::Error>,
    {
        self.map_err(|e| map_io_error_to_command_error(e.into()))
    }

    fn map_mesh_err(self) -> Result<T, CommandError>
    where
        E: Into<MeshTalkError>,
    {
        self.map_err(|e| map_mesh_talk_error_to_command_error(e.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NetworkErrorKind;

    #[test]
    fn test_map_mesh_talk_network_error() {
        let error =
            MeshTalkError::network(NetworkErrorKind::ConnectionFailed, "Connection refused");

        let command_error = map_mesh_talk_error_to_command_error(error);
        let error_string = format!("{:?}", command_error);
        assert!(error_string.contains("Network error"));
        assert!(error_string.contains("Connection refused"));
    }

    #[test]
    fn test_map_io_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::Other, "Test IO error");
        let command_error = map_io_error_to_command_error(io_error);
        let error_string = format!("{:?}", command_error);
        assert!(error_string.contains("IO error"));
        assert!(error_string.contains("Test IO error"));
    }

    #[test]
    fn test_standard_error_creators() {
        let validation = validation_error("Invalid input");
        assert!(matches!(validation, CommandError::Validation(_)));

        let auth = authentication_error("Not logged in");
        assert!(matches!(auth, CommandError::Authentication(_)));

        let service = service_error("Service unavailable");
        assert!(matches!(service, CommandError::Service(_)));

        let network = network_error("Connection failed");
        assert!(matches!(network, CommandError::Network(_)));

        let authz = authorization_error("Insufficient permissions");
        assert!(matches!(authz, CommandError::Authorization(_)));
    }
}
