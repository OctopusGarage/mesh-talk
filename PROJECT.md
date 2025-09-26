# Project Context for `mesh-talk`

This document serves as the main project specification and context file for AI tools. For detailed information about various aspects of the project, please refer to the documentation files in the `specifications/` directory.

## Project Overview

Mesh-Talk is a local network chat tool written in Rust that enables users to communicate directly with others on the same network using UDP broadcast and TCP connections.

### Current Implementation

#### Technology Stack

- **Language**: Rust 2021
- **Async Runtime**: Tokio
- **Serialization**: serde, serde_json
- **Command-line Parsing**: clap

#### Core Features

1. Local network communication without a central server
2. Automatic peer discovery using UDP broadcast
3. Real-time messaging between connected peers via TCP
4. Command-line interface

#### Network Implementation

- UDP broadcast (port 8888) for local network peer discovery
- TCP connections for reliable peer-to-peer communication
- JSON encoding for messages
- Thread-safe peer management using Arc and Mutex

#### Message Types

```rust
#[derive(Debug, Serialize, Deserialize)]
enum Message {
    Discovery {
        name: String,
        port: u16,
    },
    Chat {
        from: String,
        content: String,
    },
}
```

#### Main Components

- `Node`: Core node structure managing node name, port, and connected peers
- `start_udp_broadcast`: Starts UDP broadcast, periodically broadcasting node information
- `start_udp_discovery`: Listens for UDP broadcasts to discover new nodes
- `connect_to_peer`: Connects to newly discovered nodes
- `handle_incoming_connection`: Handles incoming TCP connections
- `broadcast_message`: Broadcasts messages to all connected nodes
- `handle_message`: Processes received messages

## AI Tool Compatibility

This project specification is designed to be compatible with various AI coding assistants, including:

- Qwen Code
- Gemini CLI
- Claude Code
- GitHub Copilot
- Other AI-powered development tools

The structured organization and comprehensive documentation make it easy for AI tools to understand the project context and provide relevant assistance.

## Development Conventions and Coding Standards

For development conventions, coding standards, and best practices, see [@specifications/development_conventions.md](specifications/development_conventions.md).

## Project Structure and Organization

For detailed information about the project structure, see [@specifications/project_structure.md](specifications/project_structure.md).

## Implementation Tasks and Progress

For a consolidated list of implementation tasks and their completion status, see [@specifications/TODO.md](specifications/TODO.md).

This file provides:
- Overview of completed project setup tasks
- Checklist of implementation tasks organized by module
- Progress tracking for all implementation tasks

To view the current status of all tasks and track progress, please refer to [@specifications/TODO.md](specifications/TODO.md).

## Git Commit Standards

For Git commit standards and conventions, see [@specifications/git_standards.md](specifications/git_standards.md).

## Backend Architecture (Tauri/Rust)

The backend is built using Rust with Tauri, providing a secure desktop application framework that communicates with the frontend via a well-defined command interface.

### Directory Structure

```
src-tauri/
├── src/
│   ├── bin/                    # Binary executables
│   │   └── mesh-talk-cli.rs    # CLI application entry point
│   ├── api.rs                  # Configuration management and CLI arguments
│   ├── commands.rs             # Tauri command handlers (frontend API endpoints)
│   ├── domain/                 # Core domain models and business logic
│   │   ├── message.rs          # Message protocol and structure
│   │   ├── models.rs           # Core data models (User, Contact, ChatMessage, etc.)
│   │   ├── node.rs             # Node representation
│   │   └── node_registry.rs    # Node registry for discovered nodes
│   ├── services/               # Business logic services
│   │   ├── auth_service.rs     # Authentication service
│   │   ├── contact_service.rs  # Contact management service
│   │   ├── message_service.rs  # Message handling service
│   │   ├── node_service.rs     # Core network node service
│   │   ├── notification_service.rs # Notification service
│   │   └── contact_request_service.rs # Contact request handling
│   ├── network/                # Network communication layer
│   │   ├── tcp.rs              # TCP connection management
│   │   ├── udp.rs              # UDP broadcast and discovery
│   │   ├── reconnection.rs     # Connection reconnection logic
│   │   └── runtime.rs          # Network runtime management
│   ├── identity/               # User authentication and identity
│   │   ├── auth.rs             # Authentication logic
│   │   ├── keys.rs             # Key pair generation and management
│   │   ├── user.rs             # User model
│   │   ├── manager.rs          # Identity management
│   │   └── errors.rs           # Identity-related errors
│   ├── contacts/               # Contact management
│   │   ├── contact.rs          # Contact model
│   │   ├── manager.rs          # Contact list management
│   │   ├── service.rs          # Contact service
│   │   ├── request.rs          # Contact request handling
│   │   ├── discovery.rs        # Contact discovery service
│   │   └── integration.rs      # Contact discovery integration
│   ├── crypto/                 # Cryptography and encryption
│   │   ├── keys.rs             # Key management
│   │   ├── session.rs          # Session management
│   │   ├── signal.rs           # Signal Protocol integration
│   │   └── storage.rs          # Secure key storage
│   ├── storage/                # Data persistence
│   │   ├── file_manager.rs     # File system operations
│   │   ├── encryption.rs       # Data encryption
│   │   ├── serialization.rs    # Data serialization
│   │   └── errors.rs           # Storage-related errors
│   ├── notifications/          # Notification system
│   │   ├── desktop.rs          # Desktop notification manager
│   │   ├── settings.rs         # Notification settings
│   │   └── tray.rs             # System tray integration
│   ├── platform/               # Platform-specific implementations
│   │   ├── linux.rs            # Linux-specific functionality
│   │   ├── macos.rs            # macOS-specific functionality
│   │   └── windows.rs          # Windows-specific functionality
│   ├── state.rs                # Shared application state management
│   ├── events.rs               # Event emission and handling
│   ├── tray.rs                 # System tray menu and interactions
│   ├── error.rs                # Custom error types
│   ├── user_friendly_errors.rs # User-friendly error messages
│   └── utils.rs                # Utility functions
├── Cargo.toml                  # Rust dependencies and build configuration
├── tauri.conf.json             # Tauri configuration
├── build.rs                    # Build script
└── capabilities/               # Tauri capabilities and permissions
    └── default.json
```

### Architecture Layers

1. **API Layer**: Defines Tauri commands that the frontend can call (in `commands.rs`)
2. **Service Layer**: Business logic and coordination (in `services/`)
3. **Domain Layer**: Core data models and pure business logic (in `domain/`)
4. **Infrastructure Layer**: Network, storage, identity, notifications (in respective modules)
5. **Platform Layer**: OS-specific functionality (in `platform/`)

### Key Components

- `NodeService`: Core network node functionality managing TCP/UDP connections
- `AuthService`: User authentication and session management  
- `ContactService`: Contact list and relationship management
- `MessageService`: Message persistence and retrieval
- `Network Runtime`: Manages TCP listener, UDP discovery/broadcast, and reconnection
- `AppState`: Shared state between Tauri commands
- `Events`: Event emission system for real-time updates to frontend

### Protocol Implementation

The application implements a custom protocol with the following features:
- Protocol magic number: 0x4D455348 ("MESH")
- Protocol version: 1
- UDP broadcast for node discovery
- TCP for reliable message delivery
- Heartbeat mechanism for connection health

## Frontend Architecture

The frontend is built using Vue.js 3 with the following organized structure:

### Directory Structure

```
frontend/src/
├── components/           # Reusable UI components organized by feature
│   ├── chat/            # Chat-specific components (ChatWindow.vue, MessageInput.vue)
│   ├── common/          # Shared components (AppFeedback.vue)
│   └── contacts/        # Contact/network components (ContactList.vue, DiscoveryList.vue)
├── composables/         # Business logic hooks organized by feature
│   ├── auth/            # Authentication logic (useAuth.js)
│   └── chat/            # Chat real-time logic (useRealTimeMessages.js)
├── services/            # API communication layer (new)
│   └── api.js           # Centralized API functions with organized namespaces
├── stores/              # Global state management (Pinia stores)
│   ├── appStore.js      # Main application store
│   └── feedbackStore.js # User feedback and notifications store
├── views/               # Page-level components
│   ├── auth/            # Authentication views (LoginView.vue)
│   └── chat/            # Main chat view (ChatView.vue)
├── router/              # Route definitions
├── styles/              # CSS styles (global.css)
└── main.js              # Application entry point
```

### Architecture Layers

1. **Components Layer**: Reusable UI elements that render HTML and handle user interactions
2. **Composables Layer**: Business logic hooks that can be shared across components
3. **Services Layer**: API communication layer that centralizes all external calls
4. **Stores Layer**: Global state management using Pinia for application state

### API Service Organization

The `services/api.js` file provides organized namespaces:

- `authAPI` - Authentication functions (login, register, logout)
- `nodeAPI` - Node-related functions (getNodeInfo, connectToNode)
- `contactsAPI` - Contact management (getContacts, updateContact, etc.)
- `messagesAPI` - Message functions (getMessages, sendMessage, etc.)
- `networkAPI` - Network discovery functions (getDiscoveredNodes, etc.)

### Page Layout Structure

#### Login Page (LoginView.vue)
- **Route**: `/login`
- **Layout**: Centered authentication panel with tabs for sign-in and registration
- **Components**: 
  - Username/password inputs with validation
  - Switchable tabs for login/register
  - Form validation and error handling
  - Responsive design with centered panel

#### Main Chat Page (ChatView.vue) 
- **Route**: `/` (requires authentication)
- **Layout**: Two-column responsive grid layout
- **Left Column**:
  - ChatWindow component for message display
  - MessageInput component for sending messages
  - Full height flex layout
- **Right Column**: Stacked panels
  - **Contacts Panel**: List of connected contacts
  - **Discovery Panel**: List of discovered nodes on network
  - **Node Panel**: Current node information (status, peers, unread count, logout button)

### Routing and Authentication Flow
- Authentication guard redirects unauthenticated users from `/` to `/login`
- After successful login, users are redirected from `/login` to `/` (chat page)
- When logged out, users are redirected from `/` to `/login`
- Protected routes use `meta: { requiresAuth: true }` configuration

This architecture ensures separation of concerns, reduces code redundancy, and improves maintainability.

## Key Technology Stack

For information about the technology stack, see [@specifications/tech_stack.md](specifications/tech_stack.md).

## Task Completion Criteria

For task completion criteria, see [@specifications/task_completion_criteria.md](specifications/task_completion_criteria.md).

## Future Plans (Based on TODO.md)

### Technology Selection and Initialization

1. Integrate Tauri + Vue.js GUI framework
2. Integrate Signal Protocol encryption library
3. Integrate SQLite database

## Development Recommendations

Based on the detailed analysis in TODO.md, it is recommended to proceed with development in a modular way:

1. `feature/port-handling`: Handle UDP/TCP port conflicts and dynamic allocation
2. `feature/cli-enhancements`: Improve CLI functionality and user experience
3. `feature/ui`: Add GUI interface using Tauri/Vue.js
4. `feature/net`: Enhance network capabilities
5. `feature/notifications`: Add system notifications

## Important Considerations

1. Signal Implementation and Compatibility: Initially implement as "signal-compatible bidirectional ratchet implementation"
2. NAT Traversal Uncertainty: Must support relays, and user experience should be clear when direct connection is not possible
3. Key Backup: Need to provide secure backup (encrypted export) mechanism
4. Offline Messages: P2P + no central storage means offline messages depend on relays or the recipient coming online to resend/poll
5. Dependency Vulnerabilities: Regularly use `cargo-audit` and set up dependency update processes

## Plan Adjustment Guidance

If any implementation plan proves unsuitable during development, the following approach should be taken:

1. **Assess the Issue**: Identify why the current plan is not working and document the specific problems encountered
2. **Adjust the Plan**: Modify the implementation approach based on new information or challenges discovered
3. **Update Documentation**: Revise relevant specification documents in `specifications/task_specs/` to reflect the adjusted plan
4. **Update Task Tracking**: Modify the TODO.md file to reflect any changes in task scope or approach
5. **Continue Implementation**: Proceed with the updated plan, ensuring all team members are aware of the changes
6. **Document Lessons Learned**: Record any insights gained from the plan adjustment for future reference

This iterative approach ensures that development can continue smoothly even when initial plans need to be modified, while maintaining proper documentation and task tracking.