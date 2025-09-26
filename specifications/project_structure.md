# Mesh-Talk Project Structure

This document outlines the project structure and organization for the Mesh-Talk application.

## Overall Directory Structure

```
mesh-talk/
├── src-tauri/              # Rust backend (Tauri + business logic)
│   ├── src/
│   │   ├── main.rs         # Tauri main entry
│   │   ├── lib.rs          # Main library module
│   │   ├── api.rs          # Command-line argument parsing
│   │   ├── domain/         # Domain models
│   │   │   ├── mod.rs
│   │   │   ├── message.rs  # Message enum definition
│   │   │   └── node.rs     # Node struct definition
│   │   ├── services/       # Business logic services
│   │   │   ├── mod.rs
│   │   │   └── node_service.rs # Node service implementation
│   │   ├── network/        # Network layer
│   │   │   ├── mod.rs
│   │   │   ├── udp.rs      # UDP broadcast functionality
│   │   │   └── tcp.rs      # TCP connection handling
│   │   └── utils.rs        # Utility functions
│   ├── Cargo.toml
│   └── tauri.conf.json
├── frontend/               # Vue frontend
│   ├── package.json
│   └── src/                # Frontend source code
├── specifications/         # Project documentation
├── .qwen/                 # AI tool configuration
├── .github/               # GitHub workflows
├── Makefile               # Build and development commands
└── Cargo.toml             # Workspace configuration
```

## Module Descriptions

### Domain Module (`src-tauri/src/domain/`)

Contains the core data models and structures:
- `message.rs`: Message enum with Discovery and Chat variants
- `node.rs`: Node struct definition with peer management

### Services Module (`src-tauri/src/services/`)

Contains business logic implementations:
- `node_service.rs`: Node service with peer management, message broadcasting, and connection handling

### Network Module (`src-tauri/src/network/`)

Handles all network-related functionality:
- `udp.rs`: UDP broadcast for peer discovery
- `tcp.rs`: TCP connections for reliable communication
- Message transmission and reception

### API Module (`src-tauri/src/api.rs`)

Handles command-line argument parsing using clap.

### Main Entry Point (`src-tauri/src/main.rs`)

The application entry point that initializes and runs the node service.

### Utilities (`src-tauri/src/utils.rs`)

Common utility functions used throughout the application.