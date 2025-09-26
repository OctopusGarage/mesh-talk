//! User-friendly error handling for the Mesh-Talk application
use crate::error::{MeshTalkError, NetworkErrorKind};

/// Convert a MeshTalkError into a user-friendly error message
pub fn format_user_friendly_error(error: &MeshTalkError) -> String {
    match error {
        MeshTalkError::NetworkError {
            kind,
            message,
            source,
        } => {
            let base_message = match kind {
                NetworkErrorKind::ConnectionFailed => {
                    "Unable to establish connection. Please check your network connection and try again."
                }
                NetworkErrorKind::ConnectionTimeout => {
                    "Connection timed out. The remote peer may be offline or unreachable."
                }
                NetworkErrorKind::SendFailed => {
                    "Failed to send message. Please check your network connection and try again."
                }
                NetworkErrorKind::ReceiveFailed => {
                    "Failed to receive message. Please check your network connection and try again."
                }
                NetworkErrorKind::InvalidMessage => {
                    "Received an invalid message format. This may indicate a compatibility issue."
                }
                NetworkErrorKind::ProtocolViolation => {
                    "Protocol violation detected. This may indicate a compatibility issue or network problem."
                }
                NetworkErrorKind::DiscoveryFailed => {
                    "Peer discovery failed. Please ensure you're on the same network as other users."
                }
            };

            if let Some(source_error) = source {
                format!("{}\nDetails: {} - {}", base_message, message, source_error)
            } else {
                format!("{}\nDetails: {}", base_message, message)
            }
        }

        MeshTalkError::DatabaseError { message, source } => {
            let base_message =
                "A database error occurred. Your data may not have been saved properly.";
            if let Some(source_error) = source {
                format!("{}\nDetails: {} - {}", base_message, message, source_error)
            } else {
                format!("{}\nDetails: {}", base_message, message)
            }
        }

        MeshTalkError::AuthError { message, source } => {
            let base_message =
                "Authentication failed. Please check your credentials and try again.";
            if let Some(source_error) = source {
                format!("{}\nDetails: {} - {}", base_message, message, source_error)
            } else {
                format!("{}\nDetails: {}", base_message, message)
            }
        }

        MeshTalkError::MessageError { message, source } => {
            let base_message =
                "A message error occurred. Your message may not have been sent properly.";
            if let Some(source_error) = source {
                format!("{}\nDetails: {} - {}", base_message, message, source_error)
            } else {
                format!("{}\nDetails: {}", base_message, message)
            }
        }

        MeshTalkError::ApplicationError { message, source } => {
            let base_message =
                "An application error occurred. Please try again or restart the application.";
            if let Some(source_error) = source {
                format!("{}\nDetails: {} - {}", base_message, message, source_error)
            } else {
                format!("{}\nDetails: {}", base_message, message)
            }
        }
    }
}

/// Convert any error into a user-friendly error message
pub fn format_any_error<E: std::error::Error>(error: &E) -> String {
    format_user_friendly_error(&MeshTalkError::ApplicationError {
        message: error.to_string(),
        source: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NetworkErrorKind;

    #[test]
    fn test_network_connection_failed() {
        let error = MeshTalkError::network(
            NetworkErrorKind::ConnectionFailed,
            "Could not connect to 192.168.1.100:7000",
        );

        let friendly_message = format_user_friendly_error(&error);
        assert!(friendly_message.contains("Unable to establish connection"));
        assert!(friendly_message.contains("Could not connect to 192.168.1.100:7000"));
    }

    #[test]
    fn test_database_error() {
        let error = MeshTalkError::database("Failed to write to database file");

        let friendly_message = format_user_friendly_error(&error);
        assert!(friendly_message.contains("A database error occurred"));
        assert!(friendly_message.contains("Failed to write to database file"));
    }

    #[test]
    fn test_auth_error() {
        let error = MeshTalkError::auth("Invalid username or password");

        let friendly_message = format_user_friendly_error(&error);
        assert!(friendly_message.contains("Authentication failed"));
        assert!(friendly_message.contains("Invalid username or password"));
    }

    #[test]
    fn test_format_any_error() {
        let error = std::io::Error::new(std::io::ErrorKind::NotFound, "File not found");
        let friendly_message = format_any_error(&error);
        assert!(friendly_message.contains("An application error occurred"));
        assert!(friendly_message.contains("File not found"));
    }
}
