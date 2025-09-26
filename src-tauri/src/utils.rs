//! Utility functions for the Mesh-Talk application

/// Validates that a port number is in the valid range (1-65535)
pub fn is_valid_port(port: u16) -> bool {
    port > 0
}

/// Checks if a name is valid (non-empty and not too long)
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 50
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_port() {
        assert!(is_valid_port(8000));
        assert!(is_valid_port(1));
        assert!(is_valid_port(65535));
        assert!(!is_valid_port(0));
    }

    #[test]
    fn test_is_valid_name() {
        assert!(is_valid_name("Alice"));
        assert!(is_valid_name("Bob"));
        assert!(!is_valid_name(""));
        assert!(is_valid_name("A"));
        // Test with a 50-character name
        assert!(is_valid_name(&"A".repeat(50)));
        // Test with a 51-character name
        assert!(!is_valid_name(&"A".repeat(51)));
    }
}
