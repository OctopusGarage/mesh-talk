//! Utility functions for the Mesh-Talk application
//!
//! Contains general utility functions used throughout the application.
use rand::{distributions::Alphanumeric, thread_rng, Rng};

/// Validates that a name meets the application's requirements.
///
/// Names must be non-empty and contain only alphanumeric characters, spaces, hyphens, or underscores.
pub fn is_valid_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    name.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_')
}

/// Validates that a port number is within the valid range.
///
/// Valid ports are between 1024 and 65535 inclusive.
pub fn is_valid_port(port: u16) -> bool {
    port >= 1024 && port <= 65535
}

/// Generates a random string of the specified length using alphanumeric characters.
pub fn generate_random_string(length: usize) -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

/// Re-export error handling utilities
pub mod error_handling;
