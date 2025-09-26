[deepwiki](https://deepwiki.com/Kingson4Wu/mesh-talk)

# Mesh-Talk
A local network chat tool written in Rust that enables users to communicate directly with others on the same network using UDP broadcast and TCP connections.

## Features

- Local network communication without a central server
- Automatic peer discovery using UDP broadcast
- Real-time messaging between connected peers via TCP
- Command-line interface
- Built with async Rust using Tokio
- Modular architecture following professional Rust project structure

## Project Structure

```
mesh-talk/
├── src-tauri/              # Rust backend (Tauri + business logic)
│   ├── src/
│   │   ├── main.rs         # Tauri main entry
│   │   ├── lib.rs          # Main library module
│   │   ├── api.rs          # Command-line argument parsing
│   │   ├── domain/         # Domain models
│   │   ├── services/       # Business logic services
│   │   ├── network/        # Network layer
│   │   └── utils.rs        # Utility functions
│   ├── Cargo.toml
│   └── tauri.conf.json
├── frontend/               # Vue frontend
├── specifications/         # Project documentation
├── Makefile               # Build and development commands
└── Cargo.toml             # Workspace configuration
```

## Prerequisites

- Rust 2021 edition or later
- Cargo package manager

## Installation

Clone the repository and build the project:

```bash
git clone https://github.com/yourusername/mesh-talk.git
cd mesh-talk
cargo build --release
```

## Usage

Run the application with your desired name and port:

```bash
cargo run -- --name YourName --port 8000
```

### Command Line Arguments

- `--name` or `-n`: Your display name in the chat
- `--port` or `-p`: The TCP port to listen on for incoming connections

## How It Works

1. The application creates a mesh network where each node can communicate directly with other nodes
2. UDP broadcast is used for peer discovery on port 8888
3. TCP connections are established between peers for reliable message delivery
4. Messages are broadcast to all connected peers in the network

## Technical Details

- Uses Tokio for async runtime and networking
- Implements UDP broadcast for peer discovery
- TCP for reliable peer-to-peer communication
- JSON serialization for message encoding
- Thread-safe peer management using Arc and Mutex
- Modular design with clear separation of concerns

## Dependencies

- tokio: Async runtime and networking
- serde: Serialization framework
- serde_json: JSON serialization
- clap: Command line argument parsing

## Development

This project follows a professional Rust project structure with:
- Domain models in `src-tauri/src/domain/`
- Business logic in `src-tauri/src/services/`
- Network handling in `src-tauri/src/network/`
- Command-line interface in `src-tauri/src/api.rs`
- Utility functions in `src-tauri/src/utils.rs`

Use the provided Makefile for common development tasks:
```bash
make dev        # Run in development mode
make build      # Build for release
make test       # Run tests
```

## License

This project is open source and available under the MIT License.
