# Technology Stack

This document provides an overview of the technology stack used in the Mesh-Talk project.

## Backend Technologies

### Core Language
- **Rust**: Main programming language for the application backend
  - Edition: 2021
  - Key features: Memory safety, concurrency, performance

### Networking
- **Tokio**: Asynchronous runtime for Rust
  - Used for async I/O, networking, and task management
- **UDP/TCP Sockets**: Standard networking protocols
  - UDP broadcast for local network peer discovery
  - TCP connections for reliable peer-to-peer communication

### Serialization
- **Serde**: Serialization framework for Rust
  - Used for JSON serialization of messages and data structures
- **serde_json**: JSON support for Serde

### Command Line Interface
- **clap**: Command line argument parser
  - Used for parsing command line arguments in the CLI version

### Serialization
- **Serde**: Serialization framework for Rust
  - Used for JSON serialization of messages and data structures
- **serde_json**: JSON support for Serde

### Command Line Interface
- **clap**: Command line argument parser
  - Used for parsing command line arguments in the CLI version

## Frontend Technologies

This project currently implements only a command-line interface.

## Development and Build Tools

### Rust Tooling
- **Cargo**: Rust's package manager and build system
  - Manages dependencies and builds the application
- **rustfmt**: Code formatter for Rust
  - Ensures consistent code formatting
- **Clippy**: Linting tool for Rust
  - Provides additional code quality checks

### Frontend Tooling
- **Vite**: Fast build tool and development server
  - Used for building and serving the frontend application
- **npm**: Package manager for JavaScript dependencies
  - Manages frontend dependencies

### Testing
- **cargo test**: Built-in Rust testing framework
  - Used for unit and integration tests
- **Playwright/Cypress**: End-to-end testing frameworks (planned)
  - Will be used for UI testing

### CI/CD
- **GitHub Actions**: Continuous integration and deployment platform
  - Used for automated testing and building
- **Docker**: Containerization platform (for bootstrap server)
  - Used for packaging and deploying the bootstrap server

## Platform Support

- **macOS**: Full support with system integration
- **Windows**: Full support with system integration
- **Linux**: Full support with system integration

## Security Considerations

- **System Keychains**: Integration with macOS Keychain, Windows DPAPI, and Linux Secret Service for secure key storage
- **Transport Security**: Noise protocol for encrypted communication
- **Application Security**: Tauri's security model for WebView-based applications