use crate::domain::message::{
    Message, MESSAGE_TYPE_DISCOVERY, MESSAGE_TYPE_HEARTBEAT, PROTOCOL_MAGIC, PROTOCOL_VERSION,
};
use crate::domain::node_registry::NodeRegistry;
use crate::error::{MeshTalkError, MeshTalkResult, NetworkErrorKind};
use serde_json;
use socket2::{Domain, Protocol, Socket, Type};
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;
use tokio::time::{interval, Duration};

// Network configuration
const BROADCAST_PORT: u16 = 9999; // default discovery port
const BROADCAST_INTERVAL: Duration = Duration::from_secs(10); // Increased from 5 to 10 seconds to reduce network traffic
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30); // Increased from 10 to 30 seconds to reduce network traffic
const REGISTRY_CLEANUP_INTERVAL: Duration = Duration::from_secs(60); // Clean up registry every minute
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

// Message size limits
const MAX_MESSAGE_SIZE: usize = 65536; // Maximum message size (64KB)
const MAX_JSON_SIZE: usize = 65536; // Maximum JSON data size (64KB)

fn discovery_port() -> u16 {
    env::var("MESH_TALK_DISCOVERY_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .unwrap_or(BROADCAST_PORT)
}

fn duration_from_env(var: &str, default: Duration) -> Duration {
    env::var(var)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

fn discovery_interval() -> Duration {
    duration_from_env("MESH_TALK_DISCOVERY_INTERVAL_MS", BROADCAST_INTERVAL)
}

fn heartbeat_interval() -> Duration {
    duration_from_env("MESH_TALK_HEARTBEAT_INTERVAL_MS", HEARTBEAT_INTERVAL)
}

fn registry_cleanup_interval() -> Duration {
    duration_from_env("MESH_TALK_REGISTRY_CLEANUP_MS", REGISTRY_CLEANUP_INTERVAL)
}

fn reconnect_interval() -> Duration {
    duration_from_env("MESH_TALK_RECONNECT_MS", RECONNECT_INTERVAL)
}

fn bind_udp_socket(addr: SocketAddr) -> MeshTalkResult<UdpSocket> {
    let domain = match addr {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };

    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP)).map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::ConnectionFailed,
            "Failed to create UDP socket",
            Box::new(e),
        )
    })?;

    socket.set_reuse_address(true).map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::ConnectionFailed,
            "Failed to enable SO_REUSEADDR on UDP socket",
            Box::new(e),
        )
    })?;

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos"
    ))]
    socket.set_reuse_port(true).map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::ConnectionFailed,
            "Failed to enable SO_REUSEPORT on UDP socket",
            Box::new(e),
        )
    })?;

    socket.bind(&addr.into()).map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::ConnectionFailed,
            format!("Failed to bind UDP socket on {}", addr),
            Box::new(e),
        )
    })?;

    socket.set_nonblocking(true).map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::ConnectionFailed,
            "Failed to set UDP socket to non-blocking mode",
            Box::new(e),
        )
    })?;

    UdpSocket::from_std(socket.into()).map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::ConnectionFailed,
            "Failed to integrate UDP socket with async runtime",
            Box::new(e),
        )
    })
}

pub async fn start_udp_broadcast(
    name: String,
    username: Option<String>,
    port: u16,
    user_id: Option<u64>, // Add user ID parameter
) -> MeshTalkResult<()> {
    let broadcast_port = discovery_port();
    let socket = bind_udp_socket(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    socket.set_broadcast(true).map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::ConnectionFailed,
            "Failed to set broadcast option on UDP socket",
            Box::new(e),
        )
    })?;

    let broadcast_addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)),
        broadcast_port,
    );

    let discovery_message = Message::Discovery {
        name: name.clone(),
        port,
        username: username.clone(),
        user_id,
    };

    let heartbeat_message = Message::Heartbeat {
        name,
        port,
        username,
        user_id,
    };

    println!("Starting UDP broadcast on port {}...", broadcast_port);

    // Create separate intervals for discovery and heartbeat
    let mut discovery_interval = interval(discovery_interval());
    let mut heartbeat_interval = interval(heartbeat_interval());

    loop {
        tokio::select! {
            // Send discovery message
            _ = discovery_interval.tick() => {
                send_message(&socket, &broadcast_addr, &discovery_message, MESSAGE_TYPE_DISCOVERY).await
                    .map_err(|e| MeshTalkError::network_with_source(
                        NetworkErrorKind::SendFailed,
                        "Failed to send discovery message",
                        Box::new(e)
                    ))?;
            }
            // Send heartbeat message
            _ = heartbeat_interval.tick() => {
                send_message(&socket, &broadcast_addr, &heartbeat_message, MESSAGE_TYPE_HEARTBEAT).await
                    .map_err(|e| MeshTalkError::network_with_source(
                        NetworkErrorKind::SendFailed,
                        "Failed to send heartbeat message",
                        Box::new(e)
                    ))?;
            }
        }
    }
}

/// Create a protocol header for UDP messages
///
/// The header format is:
/// [Magic Number: 4 bytes][Version: 1 byte][Message Type: 1 byte]
fn create_protocol_header(message_type: u8) -> [u8; 6] {
    [
        (PROTOCOL_MAGIC >> 24) as u8,
        (PROTOCOL_MAGIC >> 16) as u8,
        (PROTOCOL_MAGIC >> 8) as u8,
        (PROTOCOL_MAGIC) as u8,
        PROTOCOL_VERSION,
        message_type,
    ]
}

async fn send_message(
    socket: &UdpSocket,
    broadcast_addr: &SocketAddr,
    message: &Message,
    message_type: u8,
) -> MeshTalkResult<()> {
    // Serialize message
    let json = serde_json::to_string(message)
        .map_err(|e| MeshTalkError::message(format!("Failed to serialize message: {}", e)))?;

    // Create protocol header
    let header = create_protocol_header(message_type);

    // Combine header and message
    let mut packet = Vec::new();
    packet.extend_from_slice(&header);
    packet.extend_from_slice(json.as_bytes());

    socket.send_to(&packet, broadcast_addr).await.map_err(|e| {
        MeshTalkError::network_with_source(
            NetworkErrorKind::SendFailed,
            "Failed to send UDP packet",
            Box::new(e),
        )
    })?;
    Ok(())
}

pub async fn start_udp_discovery<F>(connect_callback: F) -> MeshTalkResult<()>
where
    F: Fn(SocketAddr, String, Option<String>, u16) + Send + 'static + Clone,
{
    // Try to bind to the standard broadcast port, but handle conflicts gracefully
    let broadcast_port = discovery_port();
    let socket_result = bind_udp_socket(SocketAddr::from(([0, 0, 0, 0], broadcast_port)));
    let socket = match socket_result {
        Ok(socket) => {
            println!(
                "Starting to listen for UDP discovery messages on port {}...",
                broadcast_port
            );
            socket
        }
        Err(e) => {
            eprintln!(
                "Warning: Failed to bind to UDP broadcast port {} - {}",
                broadcast_port, e
            );
            eprintln!("This may prevent discovery of other nodes on your network.");
            // Even if we can't bind to the standard port, we can still send broadcasts
            // and listen on a different port for testing purposes
            let fallback_socket = bind_udp_socket(SocketAddr::from(([0, 0, 0, 0], 0)))?;
            let local_port = fallback_socket
                .local_addr()
                .map_err(|e| {
                    MeshTalkError::network_with_source(
                        NetworkErrorKind::ConnectionFailed,
                        "Failed to get local address of fallback socket",
                        Box::new(e),
                    )
                })?
                .port();
            println!(
                "Listening for UDP discovery messages on fallback port {}...",
                local_port
            );
            fallback_socket
        }
    };

    // Create node registry for tracking discovered nodes
    let node_registry: Arc<Mutex<NodeRegistry>> = Arc::new(Mutex::new(NodeRegistry::new()));
    let registry_cleanup = node_registry.clone();
    let registry_heartbeat = node_registry.clone();

    // Spawn a task to periodically clean up timed out nodes
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(registry_cleanup_interval());
        loop {
            interval.tick().await;
            let _timed_out = {
                let mut registry = registry_cleanup.lock().unwrap();
                registry.remove_timed_out_nodes()
            };

            // Don't call the callback for timed out nodes since they should be removed from discovery
            // The periodic node discovery emission in events.rs will automatically handle that
            // timed out nodes no longer appear in the discovery list
        }
    });

    // Spawn a task to periodically check for nodes that should be reconnected
    let connect_callback_clone = connect_callback.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(reconnect_interval());
        loop {
            interval.tick().await;
            let registry = registry_heartbeat.lock().unwrap();
            let nodes_to_reconnect = registry.get_nodes_to_reconnect();
            for node in nodes_to_reconnect {
                // Call the connect callback for nodes that should be reconnected
                connect_callback_clone(
                    node.addr,
                    node.name.clone(),
                    node.username.clone(),
                    node.listen_port.unwrap_or(node.addr.port()),
                );
            }
        }
    });

    let mut buf = [0u8; 1024];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, addr)) => {
                // Try to parse the received message
                if let Some(parsed_message) = parse_udp_message(&buf[..n]) {
                    let (magic, version, message_type, json_data) = parsed_message;

                    // Validate protocol
                    if magic == PROTOCOL_MAGIC && version == PROTOCOL_VERSION {
                        match message_type {
                            MESSAGE_TYPE_DISCOVERY => {
                                // Parse the discovery message (including optional user_id)
                                if let Ok(Message::Discovery {
                                    name,
                                    port,
                                    username,
                                    user_id: _, // Extract user_id but don't use it in registry for now
                                }) = serde_json::from_str(json_data)
                                {
                                    // println!("[UDP Discovery] Received discovery message from {}:{}\", name, port);
                                    let peer_addr = SocketAddr::new(addr.ip(), port);

                                    // Add or update node in registry
                                    {
                                        let mut registry = node_registry.lock().unwrap();
                                        registry.add_or_update_node(
                                            peer_addr,
                                            name.clone(),
                                            username.clone(),
                                            Some(port),
                                        );
                                    }

                                    connect_callback(peer_addr, name, username.clone(), port);
                                }
                            }
                            MESSAGE_TYPE_HEARTBEAT => {
                                // Parse the heartbeat message (including optional user_id)
                                if let Ok(Message::Heartbeat {
                                    name,
                                    port,
                                    username,
                                    user_id: _, // Extract user_id but don't use it in registry for now
                                }) = serde_json::from_str(json_data)
                                {
                                    let peer_addr = SocketAddr::new(addr.ip(), port);

                                    // Update heartbeat in registry
                                    {
                                        let mut registry = node_registry.lock().unwrap();
                                        registry.add_or_update_node(
                                            peer_addr,
                                            name.clone(),
                                            username.clone(),
                                            Some(port),
                                        );
                                    }

                                    connect_callback(peer_addr, name, username.clone(), port);
                                }
                            }
                            _ => {
                                // Unknown message type
                                println!(
                                    "Received message with unknown type {} from {}",
                                    message_type, addr
                                );
                            }
                        }
                    } else {
                        // Not our protocol, but log for debugging purposes
                        println!(
                            "Received message with different protocol magic/version from {}",
                            addr
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to receive UDP message: {}", e);
                // Continue listening despite errors
            }
        }
    }
}

/// Parse a UDP message and extract protocol information
///
/// Returns None if the message is too short or invalid
/// Returns Some((magic, version, message_type, json_data)) if successful
fn parse_udp_message(data: &[u8]) -> Option<(u32, u8, u8, &str)> {
    // Check if we have enough bytes for the header (6 bytes)
    if data.len() < 6 {
        return None;
    }

    // Check if we have a reasonable amount of data (prevent extremely large messages)
    if data.len() > MAX_MESSAGE_SIZE * 16 {
        // 1MB limit (16 * 64KB)
        eprintln!("Received UDP message that exceeds maximum size limit");
        return None;
    }

    // Parse protocol header
    let magic = ((data[0] as u32) << 24)
        | ((data[1] as u32) << 16)
        | ((data[2] as u32) << 8)
        | (data[3] as u32);
    let version = data[4];
    let message_type = data[5];

    // Extract the JSON message (everything after the 6-byte header)
    let json_data = match std::str::from_utf8(&data[6..]) {
        Ok(json) => {
            // Validate JSON length (prevent extremely long JSON strings)
            if json.len() > MAX_JSON_SIZE {
                // 64KB limit
                eprintln!("Received UDP message with JSON data that exceeds maximum size limit");
                return None;
            }
            json
        }
        Err(_) => {
            eprintln!("Received UDP message with invalid UTF-8 data");
            return None;
        }
    };

    Some((magic, version, message_type, json_data))
}

/// Handle a complete UDP message and return the parsed Message if successful
///
/// This function parses the protocol header and message content, then deserializes
/// the JSON data into a Message struct based on the message type.
pub fn handle_udp_message(data: &[u8]) -> Option<Message> {
    // Parse the UDP message
    let parsed_message = parse_udp_message(data)?;
    let (magic, version, message_type, json_data) = parsed_message;

    // Validate protocol
    if magic != PROTOCOL_MAGIC || version != PROTOCOL_VERSION {
        return None;
    }

    // Parse message based on type
    match message_type {
        MESSAGE_TYPE_DISCOVERY => match serde_json::from_str::<Message>(json_data) {
            Ok(message @ Message::Discovery { .. }) => Some(message),
            _ => None,
        },
        MESSAGE_TYPE_HEARTBEAT => match serde_json::from_str::<Message>(json_data) {
            Ok(message @ Message::Heartbeat { .. }) => Some(message),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::{
        MESSAGE_TYPE_DISCOVERY, MESSAGE_TYPE_HEARTBEAT, PROTOCOL_MAGIC, PROTOCOL_VERSION,
    };

    #[test]
    fn test_create_protocol_header_discovery() {
        let header = create_protocol_header(MESSAGE_TYPE_DISCOVERY);

        // Check magic number (4 bytes)
        assert_eq!(header[0], (PROTOCOL_MAGIC >> 24) as u8);
        assert_eq!(header[1], (PROTOCOL_MAGIC >> 16) as u8);
        assert_eq!(header[2], (PROTOCOL_MAGIC >> 8) as u8);
        assert_eq!(header[3], PROTOCOL_MAGIC as u8);

        // Check version (1 byte)
        assert_eq!(header[4], PROTOCOL_VERSION);

        // Check message type (1 byte)
        assert_eq!(header[5], MESSAGE_TYPE_DISCOVERY);
    }

    #[test]
    fn test_create_protocol_header_heartbeat() {
        let header = create_protocol_header(MESSAGE_TYPE_HEARTBEAT);

        // Check magic number (4 bytes)
        assert_eq!(header[0], (PROTOCOL_MAGIC >> 24) as u8);
        assert_eq!(header[1], (PROTOCOL_MAGIC >> 16) as u8);
        assert_eq!(header[2], (PROTOCOL_MAGIC >> 8) as u8);
        assert_eq!(header[3], PROTOCOL_MAGIC as u8);

        // Check version (1 byte)
        assert_eq!(header[4], PROTOCOL_VERSION);

        // Check message type (1 byte)
        assert_eq!(header[5], MESSAGE_TYPE_HEARTBEAT);
    }

    #[tokio::test]
    async fn test_start_udp_broadcast() {
        // This test would require mocking the network or using a test network
        // For now, we'll just verify that the function can be called without panicking
        // In a real implementation, we would need to mock the UdpSocket or use a test network
    }

    #[test]
    fn test_parse_udp_message_too_short() {
        let data = [0u8; 5]; // Less than 6 bytes
        let result = parse_udp_message(&data);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_udp_message_valid_discovery() {
        // Create a valid discovery message
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION;
        let message_type = MESSAGE_TYPE_DISCOVERY;
        let json_data = r#"{"Discovery":{"name":"test","port":8000}}"#;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = parse_udp_message(&packet);
        assert!(result.is_some());

        let (parsed_magic, parsed_version, parsed_type, parsed_json) = result.unwrap();
        assert_eq!(parsed_magic, magic);
        assert_eq!(parsed_version, version);
        assert_eq!(parsed_type, message_type);
        assert_eq!(parsed_json, json_data);
    }

    #[test]
    fn test_parse_udp_message_valid_heartbeat() {
        // Create a valid heartbeat message
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION;
        let message_type = MESSAGE_TYPE_HEARTBEAT;
        let json_data = r#"{"Heartbeat":{"name":"test","port":8000}}"#;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = parse_udp_message(&packet);
        assert!(result.is_some());

        let (parsed_magic, parsed_version, parsed_type, parsed_json) = result.unwrap();
        assert_eq!(parsed_magic, magic);
        assert_eq!(parsed_version, version);
        assert_eq!(parsed_type, message_type);
        assert_eq!(parsed_json, json_data);
    }

    #[test]
    fn test_parse_udp_message_invalid_utf8() {
        // Create a message with invalid UTF-8 data
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION;
        let message_type = MESSAGE_TYPE_DISCOVERY;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        // Add invalid UTF-8 bytes
        packet.extend_from_slice(&[0xFF, 0xFE]);

        let result = parse_udp_message(&packet);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_udp_message_valid_discovery() {
        // Create a valid discovery message
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION;
        let message_type = MESSAGE_TYPE_DISCOVERY;
        let json_data = r#"{"Discovery":{"name":"test","port":8000}}"#;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = handle_udp_message(&packet);
        assert!(result.is_some());

        match result.unwrap() {
            Message::Discovery { name, port, .. } => {
                assert_eq!(name, "test");
                assert_eq!(port, 8000);
            }
            _ => panic!("Expected Discovery message"),
        }
    }

    #[test]
    fn test_handle_udp_message_valid_heartbeat() {
        // Create a valid heartbeat message
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION;
        let message_type = MESSAGE_TYPE_HEARTBEAT;
        let json_data = r#"{"Heartbeat":{"name":"test","port":8000}}"#;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = handle_udp_message(&packet);
        assert!(result.is_some());

        match result.unwrap() {
            Message::Heartbeat { name, port, .. } => {
                assert_eq!(name, "test");
                assert_eq!(port, 8000);
            }
            _ => panic!("Expected Heartbeat message"),
        }
    }

    #[test]
    fn test_handle_udp_message_invalid_magic() {
        // Create a message with invalid magic number
        let magic = PROTOCOL_MAGIC + 1; // Invalid magic number
        let version = PROTOCOL_VERSION;
        let message_type = MESSAGE_TYPE_DISCOVERY;
        let json_data = r#"{"Discovery":{"name":"test","port":8000}}"#;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = handle_udp_message(&packet);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_udp_message_invalid_version() {
        // Create a message with invalid version
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION + 1; // Invalid version
        let message_type = MESSAGE_TYPE_DISCOVERY;
        let json_data = r#"{"Discovery":{"name":"test","port":8000}}"#;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = handle_udp_message(&packet);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_udp_message_invalid_message_type() {
        // Create a message with invalid message type
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION;
        let message_type = 99; // Invalid message type
        let json_data = r#"{"Discovery":{"name":"test","port":8000}}"#;

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = handle_udp_message(&packet);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_udp_message_invalid_json() {
        // Create a message with invalid JSON
        let magic = PROTOCOL_MAGIC;
        let version = PROTOCOL_VERSION;
        let message_type = MESSAGE_TYPE_DISCOVERY;
        let json_data = r#"{"Invalid":{"name":"test","port":8000}}"#; // Invalid message type in JSON

        let mut packet = Vec::new();
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(json_data.as_bytes());

        let result = handle_udp_message(&packet);
        assert!(result.is_none());
    }
}
