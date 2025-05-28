# Mesh-Talk
[deepwiki](https://deepwiki.com/Kingson4Wu/mesh-talk)
A peer-to-peer mesh network chat application written in Rust that allows users to communicate in a decentralized manner.

## Features

- Peer-to-peer communication without a central server
- Automatic peer discovery using UDP broadcast
- Real-time messaging between connected peers
- Command-line interface
- Built with async Rust using Tokio

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

## Dependencies

- tokio: Async runtime and networking
- serde: Serialization framework
- serde_json: JSON serialization
- clap: Command line argument parsing

## License

This project is open source and available under the MIT License.
