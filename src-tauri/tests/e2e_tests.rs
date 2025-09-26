//! End-to-end tests for the CLI version of Mesh-Talk

// Note: End-to-end tests for CLI applications are complex because they require
// running the actual application and interacting with it. For now, we'll create
// tests that verify the CLI can be invoked and basic functionality works.

#[cfg(test)]
mod tests {
    use std::process::Command;

    #[test]
    fn test_cli_help_output() {
        // Test that the CLI application can be invoked with --help
        let output = Command::new("cargo")
            .args(&["run", "--bin", "mesh-talk-cli", "--", "--help"])
            .output()
            .expect("Failed to execute command");

        // Verify that the command executed successfully
        assert!(output.status.success());

        // Verify that the help text contains expected content
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage:"));
        assert!(stdout.contains("Options:"));
        assert!(stdout.contains("-n, --name"));
        assert!(stdout.contains("-p, --port"));
    }

    #[test]
    fn test_cli_version_output() {
        // Test that the CLI application can be invoked with --version
        let output = Command::new("cargo")
            .args(&["run", "--bin", "mesh-talk-cli", "--", "--version"])
            .output()
            .expect("Failed to execute command");

        // Verify that the command executed successfully
        assert!(output.status.success());

        // Verify that the version text contains expected content
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("mesh-talk"));
    }

    #[test]
    fn test_cli_invalid_arguments() {
        // Test that the CLI application handles invalid arguments gracefully
        let output = Command::new("cargo")
            .args(&["run", "--bin", "mesh-talk-cli", "--", "--invalid-arg"])
            .output()
            .expect("Failed to execute command");

        // The application should fail with invalid arguments
        assert!(!output.status.success());
    }
}
