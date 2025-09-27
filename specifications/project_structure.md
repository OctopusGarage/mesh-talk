# Mesh-Talk Project Structure

This document outlines the project structure and organization for the Mesh-Talk application.

## Overall Directory Structure

```
mesh-talk/
├── src-tauri/              # Rust backend (Tauri + business logic)
│   ├── src/
│   │   ├── main.rs         # Tauri main entry
│   │   ├── lib.rs          # Main library module
│   │   ├── api.rs          # Configuration management and CLI arguments
│   │   ├── commands.rs      # Tauri command handlers (frontend API endpoints)
│   │   ├── domain/         # Core domain models and business logic
│   │   │   ├── mod.rs
│   │   │   ├── message.rs  # Message protocol and structure
│   │   │   ├── models.rs   # Core data models (User, Contact, ChatMessage, etc.)
│   │   │   ├── node.rs     # Node representation
│   │   │   └── node_registry.rs # Node registry for discovered nodes
│   │   ├── services/        # Business logic services
│   │   │   ├── mod.rs
│   │   │   ├── auth_service.rs     # Authentication service
│   │   │   ├── contact_service.rs  # Contact management service
│   │   │   ├── message_service.rs  # Message handling service
│   │   │   ├── node_service.rs     # Core network node service
│   │   │   ├── notification_service.rs # Notification service
│   │   │   ├── contact_request_service.rs # Contact request handling
│   │   │   └── common.rs           # Common service utilities
│   │   ├── network/        # Network communication layer
│   │   │   ├── mod.rs
│   │   │   ├── tcp.rs      # TCP connection management
│   │   │   ├── udp.rs      # UDP broadcast and discovery
│   │   │   ├── reconnection.rs # Connection reconnection logic
│   │   │   ├── runtime.rs   # Network runtime management
│   │   │   └── utils.rs    # Network utilities (retry logic, timeouts)
│   │   ├── identity/       # User authentication and identity
│   │   │   ├── mod.rs
│   │   │   ├── auth.rs     # Authentication logic
│   │   │   ├── keys.rs     # Key pair generation and management
│   │   │   ├── user.rs     # User model
│   │   │   ├── manager.rs  # Identity management
│   │   │   └── errors.rs   # Identity-related errors
│   │   ├── contacts/        # Contact management
│   │   │   ├── mod.rs
│   │   │   ├── contact.rs   # Contact model
│   │   │   ├── manager.rs  # Contact list management
│   │   │   ├── service.rs  # Contact service
│   │   │   ├── request.rs  # Contact request handling
│   │   │   ├── discovery.rs # Contact discovery service
│   │   │   └── integration.rs # Contact discovery integration
│   │   ├── crypto/         # Cryptography and encryption
│   │   │   ├── mod.rs
│   │   │   ├── keys.rs     # Key management
│   │   │   ├── session.rs  # Session management
│   │   │   ├── signal.rs   # Signal Protocol integration
│   │   │   └── storage.rs # Secure key storage
│   │   ├── storage/        # Data persistence
│   │   │   ├── mod.rs
│   │   │   ├── file_manager.rs # File system operations
│   │   │   ├── encryption.rs # Data encryption
│   │   │   ├── serialization.rs # Data serialization
│   │   │   └── errors.rs   # Storage-related errors
│   │   ├── notifications/  # Notification system
│   │   │   ├── mod.rs
│   │   │   ├── desktop.rs  # Desktop notification manager
│   │   │   ├── settings.rs # Notification settings
│   │   │   └── tray.rs     # System tray integration
│   │   ├── platform/        # Platform-specific implementations
│   │   │   ├── mod.rs
│   │   │   ├── linux.rs    # Linux-specific functionality
│   │   │   ├── macos.rs    # macOS-specific functionality
│   │   │   └── windows.rs  # Windows-specific functionality
│   │   ├── state.rs        # Shared application state management
│   │   ├── events.rs       # Event emission and handling
│   │   ├── tray.rs         # System tray menu and interactions
│   │   ├── error.rs        # Custom error types
│   │   ├── user_friendly_errors.rs # User-friendly error messages
│   │   └── utils/          # Utility functions
│   │       ├── mod.rs
│   │       └── error_handling.rs # Error handling utilities
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
- `message.rs`: Message protocol definition with different message types
- `models.rs`: Core data models (User, Contact, ChatMessage, etc.)
- `node.rs`: Node representation with connection management
- `node_registry.rs`: Registry for tracking discovered nodes

### Services Module (`src-tauri/src/services/`)

Contains business logic implementations with clear separation of concerns:
- `auth_service.rs`: Authentication and user management
- `contact_service.rs`: Contact list management and operations
- `message_service.rs`: Message persistence and retrieval
- `node_service.rs`: Core network node functionality
- `notification_service.rs`: Desktop notifications and system tray integration
- `contact_request_service.rs`: Contact request handling and responses
- `common.rs`: Shared service utilities and interfaces

### Network Module (`src-tauri/src/network/`)

Handles all network-related functionality with robust error handling:
- `tcp.rs`: TCP connection management with automatic reconnection
- `udp.rs`: UDP broadcast for peer discovery and heartbeat
- `reconnection.rs`: Automatic reconnection logic for dropped connections
- `runtime.rs`: Network runtime management with graceful shutdown
- `utils.rs`: Network utilities including retry logic and timeout handling

### Identity Module (`src-tauri/src/identity/`)

Manages user authentication and cryptographic identity:
- `auth.rs`: Authentication logic with secure credential handling
- `keys.rs`: Key pair generation and management
- `user.rs`: User model with persistent storage
- `manager.rs`: Identity management with secure storage
- `errors.rs`: Identity-related errors with proper categorization

### Contacts Module (`src-tauri/src/contacts/`)

Handles contact management and discovery:
- `contact.rs`: Contact model with persistent storage
- `manager.rs`: Contact list management
- `service.rs`: Contact service with CRUD operations
- `request.rs`: Contact request handling with digital signatures
- `discovery.rs`: Contact discovery service
- `integration.rs`: Contact discovery integration

### Crypto Module (`src-tauri/src/crypto/`)

Provides cryptographic functionality using the Signal Protocol:
- `keys.rs`: Key management with secure storage
- `session.rs`: Session management for encrypted communications
- `signal.rs`: Signal Protocol integration with libsignal-rust
- `storage.rs`: Secure key storage with platform-specific keychains

### Storage Module (`src-tauri/src/storage/`)

Handles data persistence with encryption:
- `file_manager.rs`: File system operations with error handling
- `encryption.rs`: Data encryption with AES-GCM
- `serialization.rs`: Data serialization with versioning
- `errors.rs`: Storage-related errors

### Notifications Module (`src-tauri/src/notifications/`)

Manages desktop notifications and system tray interactions:
- `desktop.rs`: Desktop notification manager
- `settings.rs`: Notification settings
- `tray.rs`: System tray integration

### Platform Module (`src-tauri/src/platform/`)

Provides platform-specific implementations:
- `linux.rs`: Linux-specific functionality
- `macos.rs`: macOS-specific functionality
- `windows.rs`: Windows-specific functionality

### API Module (`src-tauri/src/api.rs`)

Handles configuration management and CLI arguments using clap.

### Commands Module (`src-tauri/src/commands.rs`)

Defines Tauri commands that the frontend can call, serving as the API interface:
- Authentication commands (login, logout, register)
- Network commands (getNodeInfo, connectToNode)
- Contact commands (getContacts, sendContactRequest)
- Message commands (getMessages, sendMessage)
- Discovery commands (getDiscoveredNodes)

### Main Entry Points (`src-tauri/src/main.rs` and `src-tauri/src/bin/mesh-talk-cli.rs`)

The application entry points that initialize and run the services:
- `main.rs`: Tauri desktop application entry point
- `bin/mesh-talk-cli.rs`: CLI application entry point

### Utilities (`src-tauri/src/utils/`)

Common utility functions used throughout the application:
- `error_handling.rs`: Common error handling utilities
- `utils.rs`: General utility functions

### State Management (`src-tauri/src/state.rs`)

Shared application state management with thread-safe access:
- `AppState`: Global application state with session management
- `SessionInfo`: User session information

### Event Handling (`src-tauri/src/events.rs`)

Event emission system for real-time updates to frontend:
- Event listeners and emitters
- Integration with Tauri event system

### Error Handling (`src-tauri/src/error.rs`)

Custom error types with proper categorization and error chains:
- `MeshTalkError`: Main error type with variants for different error categories
- Error conversion traits for seamless interoperability

### User-Friendly Errors (`src-tauri/src/user_friendly_errors.rs`)

Human-readable error messages for better user experience:
- Error formatting with context-aware messages
- Localization-ready message templates