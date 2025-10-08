use crate::domain::message::Message;
use crate::domain::models::PeerInfo;
use crate::domain::node::Node;
use crate::domain::node_registry::NodeRegistry;
use crate::error::{MeshTalkError, MeshTalkResult, NetworkErrorKind};
use crate::events::{
    emit_contact_added, emit_contact_request_received, emit_contact_response_received,
    with_node_event_app_handle,
};
use crate::network::reconnection::ReconnectionManager;
use crate::network::tcp::{handle_incoming_connection, send_message_with_retry, ConnectionManager};
use crate::services::file_transfer::FileTransferManager;
use crate::state::AppState;
use log::{error, info, warn};
use serde_json;
use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio::sync::{mpsc, RwLock};

/// Message event type for notifying about received messages
#[derive(Debug, Clone)]
pub struct MessageEvent {
    pub from_address: String,
    pub sender_name: Option<String>,
    pub from_user_id: Option<String>,
    pub to_user_id: Option<String>,
    pub to_address: Option<String>,
    pub content: String,
}

/// Type alias for message event handlers
pub type MessageEventHandler = Arc<dyn Fn(MessageEvent) + Send + Sync>;

/// Trait for handling message events
pub trait MessageEventListener: Send + Sync {
    fn on_message_received(&self, event: MessageEvent);
}

// Static notification service instance
lazy_static::lazy_static! {
    pub static ref NOTIFICATION_SERVICE: crate::services::notification_service::NotificationService = crate::services::notification_service::NotificationService::new();
}

use std::sync::atomic::{AtomicU64, Ordering};

/// NodeService manages the network node and peer connections
#[derive(Clone)]
pub struct NodeService {
    /// The core node information and peer connections
    pub node: Node,
    /// Registry of discovered nodes
    pub node_registry: Arc<Mutex<NodeRegistry>>,
    /// Connection manager for handling TCP connections
    pub connection_manager: Arc<ConnectionManager>,
    /// Reconnection manager for handling connection retries
    pub reconnection_manager: Arc<ReconnectionManager>,
    /// Message event handlers for notifying about received messages
    pub message_handlers: Arc<RwLock<Vec<MessageEventHandler>>>,
    /// User ID for the authenticated user (empty if not authenticated)
    pub user_id: Arc<Mutex<String>>,
}

/// Result returned when ensuring a TCP connection to a peer identified by user_id
#[derive(Debug, Clone)]
pub struct ConnectionStatus {
    pub addr: SocketAddr,
    pub reused: bool,
}

impl NodeService {
    /// Creates a new NodeService instance with the provided name and port
    pub fn new(name: String, port: u16) -> Self {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let peer_info_map = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager = Arc::new(ConnectionManager::new(
            Arc::clone(&peers),
            Arc::clone(&node_registry),
        ));
        let reconnection_manager = Arc::new(ReconnectionManager::new(
            Arc::clone(&connection_manager),
            Arc::clone(&node_registry),
        ));
        let message_handlers = Arc::new(RwLock::new(Vec::<MessageEventHandler>::new()));

        Self {
            node: Node {
                name: name.clone(),
                port,
                peers,
                peer_info: Arc::clone(&peer_info_map),
            },
            node_registry,
            connection_manager,
            reconnection_manager,
            message_handlers,
            user_id: Arc::new(Mutex::new(String::new())), // Initialize with empty string (not authenticated)
        }
    }

    /// Sets the authenticated user ID for this node service
    pub fn set_user_id(&self, user_id: String) {
        let mut id_guard = self.user_id.lock().unwrap();
        *id_guard = user_id;
    }

    /// Gets the authenticated user ID for this node service
    pub fn get_user_id(&self) -> String {
        let id_guard = self.user_id.lock().unwrap();
        id_guard.clone()
    }

    /// Handles a received message by parsing it and performing appropriate actions based on message type
    /// This is an instance method that has access to the node registry and other instance data
    pub async fn handle_message_with_registry(
        &self,
        line: String,
        source_addr: SocketAddr, // The source address of the message
        _node_name: String,
        message_handlers: Arc<RwLock<Vec<MessageEventHandler>>>,
        node_registry: Arc<Mutex<NodeRegistry>>,
    ) -> Result<(), serde_json::Error> {
        // println!("Trying to parse message: {}", line);
        match serde_json::from_str(&line) {
            Ok(Message::Chat {
                from,
                content,
                from_user_id,
                from_address,
                to_user_id,
                to_address,
            }) => {
                println!("\nReceived message from {}: {}\n", from, content);

                if let Some(ref uid) = from_user_id {
                    self.connection_manager
                        .register_user_connection(uid.clone(), source_addr);
                }

                // Notify message handlers
                let handlers = message_handlers.read().await;
                let event = MessageEvent {
                    from_address: from_address
                        .clone()
                        .unwrap_or_else(|| source_addr.to_string()),
                    sender_name: Some(from.clone()),
                    from_user_id,
                    to_user_id,
                    to_address,
                    content: content.clone(),
                };
                for handler in handlers.iter() {
                    handler(event.clone());
                }

                // Send notification for new message
                let _ = NOTIFICATION_SERVICE.send_new_message_notification(&from, &content);

                print!("> ");
                std::io::stdout().flush().unwrap();
                Ok(())
            }
            Ok(Message::FileOffer(payload)) => {
                let payload_clone = payload.clone();

                if let Some(ref uid) = payload_clone.sender_user_id {
                    self.connection_manager
                        .register_user_connection(uid.clone(), source_addr);
                } else {
                    self.connection_manager
                        .register_connection_for_addr(source_addr);
                }

                if let Err(err) = FileTransferManager::global()
                    .handle_file_offer(payload, Some(source_addr.to_string()))
                    .await
                {
                    error!(
                        "[NODE SERVICE] Failed to handle file offer from {}: {:?}",
                        source_addr, err
                    );
                }

                let content_json = serde_json::json!({
                    "type": "file",
                    "transferId": payload_clone.transfer_id,
                    "fileName": payload_clone.file_name,
                    "fileSize": payload_clone.file_size,
                    "checksum": payload_clone.checksum,
                    "status": "incoming",
                    "direction": "incoming",
                    "resumeOffset": payload_clone.resume_offset.unwrap_or(0)
                });

                let event = MessageEvent {
                    from_address: payload_clone
                        .sender_address
                        .clone()
                        .unwrap_or_else(|| source_addr.to_string()),
                    sender_name: payload_clone.sender_name.clone(),
                    from_user_id: payload_clone.sender_user_id.clone(),
                    to_user_id: {
                        let uid = self.get_user_id();
                        if uid.is_empty() {
                            None
                        } else {
                            Some(uid)
                        }
                    },
                    to_address: None,
                    content: content_json.to_string(),
                };

                let handlers = message_handlers.read().await;
                for handler in handlers.iter() {
                    handler(event.clone());
                }
                Ok(())
            }
            Ok(Message::FileChunk(payload)) => {
                let transfer_id = payload.transfer_id.clone();
                if let Err(err) = FileTransferManager::global()
                    .handle_file_chunk(payload)
                    .await
                {
                    error!(
                        "[NODE SERVICE] Failed to process file chunk for {}: {:?}",
                        transfer_id, err
                    );
                }
                Ok(())
            }
            Ok(Message::FileAck(payload)) => {
                let transfer_id = payload.transfer_id.clone();
                if let Err(err) = FileTransferManager::global().handle_file_ack(payload).await {
                    error!(
                        "[NODE SERVICE] Failed to process file ack for {}: {:?}",
                        transfer_id, err
                    );
                }
                Ok(())
            }
            Ok(Message::FileComplete(payload)) => {
                let transfer_id = payload.transfer_id.clone();
                if let Err(err) = FileTransferManager::global()
                    .handle_file_complete(payload)
                    .await
                {
                    error!(
                        "[NODE SERVICE] Failed to finalize file transfer {}: {:?}",
                        transfer_id, err
                    );
                }
                Ok(())
            }
            Ok(Message::Discovery {
                name,
                port,
                username,
                user_id,
                ..
            }) => {
                println!(
                    "Received Discovery message from {} (user: {:?}), port: {}\n",
                    name, username, port
                );

                if let Some(uid) = user_id {
                    self.connection_manager
                        .register_user_connection(uid, source_addr);
                }
                Ok(())
            }
            Ok(Message::Heartbeat {
                name,
                port,
                username,
                user_id,
                ..
            }) => {
                println!(
                    "Received Heartbeat message from {} (user: {:?}), port: {}\n",
                    name, username, port
                );

                if let Some(uid) = user_id {
                    self.connection_manager
                        .register_user_connection(uid, source_addr);
                }

                // Update peer info with heartbeat
                // Note: We don't have access to peer_info here, but in a real implementation
                // we would update the heartbeat timestamp for this peer
                Ok(())
            }
            Ok(Message::ContactRequest {
                requester_public_key,
                requester_alias,
                timestamp,
                signature,
                node_name,
                username,
                user_id,
                ip,   // IP from the message data
                port, // Port from the message data
            }) => {
                println!(
                    "Received contact request from '{}' (alias: '{}') at {} (signature bytes: {}) with user_id: {:?}",
                    requester_public_key,
                    requester_alias,
                    timestamp,
                    signature.len(),
                    user_id
                );

                let requester_public_key_event = requester_public_key.clone();

                // Get source IP from the actual TCP connection, which is the most accurate
                let source_ip_str = source_addr.ip().to_string();

                // Try to get more accurate information from the node registry if we have a user_id
                // Use source IP from connection but use listen_port from registry if available
                let (peer_username, peer_node_name, peer_ip, peer_port) =
                    if let Some(ref uid) = user_id {
                        let registry = node_registry.lock().unwrap();
                        if let Some(node) = registry.get_node_by_user_id(uid) {
                            (
                                node.username.clone().unwrap_or_else(|| {
                                    username.clone().unwrap_or_else(|| "Unknown".to_string())
                                }),
                                node.name.clone(),
                                // Use the source IP from the actual connection as the most accurate
                                source_ip_str.clone(),
                                // Use the listen_port from the registry (not the source port of the TCP connection)
                                node.listen_port.unwrap_or(source_addr.port()),
                            )
                        } else {
                            // Fallback to provided values if not found in registry, but still use source IP
                            (
                                username.clone().unwrap_or_else(|| "Unknown".to_string()),
                                node_name.clone().unwrap_or_else(|| "Unknown".to_string()),
                                source_ip_str.clone(), // Use actual connection IP
                                source_addr.port(),    // Use source port as fallback
                            )
                        }
                    } else {
                        // If no user_id provided, use the values from the message but still use source IP
                        (
                            username.clone().unwrap_or_else(|| "Unknown".to_string()),
                            node_name.clone().unwrap_or_else(|| "Unknown".to_string()),
                            source_ip_str.clone(), // Use actual connection IP
                            source_addr.port(),    // Use source port as fallback
                        )
                    };

                let composed_alias = if peer_port == 0 {
                    format!("{} • {} • {}", peer_username, peer_node_name, peer_ip)
                } else {
                    format!(
                        "{} • {} • {}:{}",
                        peer_username, peer_node_name, peer_ip, peer_port
                    )
                };

                if let Some(uid) = user_id.clone() {
                    self.connection_manager
                        .register_user_connection(uid, source_addr);
                }

                let requester_alias_event = composed_alias.clone();
                let request_json = match serde_json::to_string(&Message::ContactRequest {
                    requester_public_key: requester_public_key.clone(),
                    requester_alias: composed_alias,
                    timestamp,
                    signature: signature.clone(),
                    node_name: Some(peer_node_name.clone()),
                    username: Some(peer_username.clone()),
                    user_id,
                    ip: Some(peer_ip.clone()), // Use the actual connection IP
                    port: Some(peer_port),     // Use the actual connection port
                }) {
                    Ok(json) => json,
                    Err(err) => {
                        eprintln!(
                            "Failed to serialize contact request for event emission: {}",
                            err
                        );
                        String::new()
                    }
                };
                with_node_event_app_handle(move |app_handle| {
                    emit_contact_request_received(
                        app_handle,
                        requester_public_key_event.clone(),
                        requester_alias_event,
                        request_json.clone(),
                    );
                });
                Ok(())
            }
            Ok(Message::ContactResponse {
                responder_public_key,
                approved,
                responder_alias,
                timestamp,
                signature,
                user_id,
                ..
            }) => {
                println!(
                    "Received contact response from '{}' (alias: '{}', approved: {}) at {}",
                    responder_public_key, responder_alias, approved, timestamp
                );

                if let Some(uid) = user_id {
                    self.connection_manager
                        .register_user_connection(uid, source_addr);
                }

                // For now, we'll just log this. In a full implementation, we would process
                // the contact response appropriately.
                println!(
                    "Contact response processed - from: {}, approved: {}, alias: {}",
                    responder_public_key, approved, responder_alias
                );

                // Persist contact on this side as well (mirror of inviter flow)
                if approved {
                    let response_json = line.clone();
                    let responder_public_key_for_persist = responder_public_key.clone();
                    with_node_event_app_handle(|app_handle| {
                        let app_state: tauri::State<AppState> = app_handle.state();
                        if let Some(session) = app_state.session().get() {
                            let username = session.user.name.clone();
                            let contact_request_service = app_state.contact_request_service();
                            let responder_public_key_cloned =
                                responder_public_key_for_persist.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(err) = contact_request_service
                                    .handle_contact_response(&username, "", &response_json)
                                    .await
                                {
                                    warn!(
                                        "Failed to persist contact after response for user {}: {:?}",
                                        username,
                                        err
                                    );
                                } else {
                                    info!(
                                        "Contact response from {} persisted for user {}",
                                        responder_public_key_cloned, username
                                    );
                                }
                            });
                        } else {
                            warn!(
                                "Skipping contact response persistence; no active session available"
                            );
                        }
                    });
                }

                // Emit contact response received event
                with_node_event_app_handle(|app_handle| {
                    emit_contact_response_received(
                        app_handle,
                        responder_public_key,
                        responder_alias,
                        approved,
                        timestamp,
                    );
                });
                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to parse message: {}", e);
                eprintln!("Message content: {}", line);
                return Err(e);
            }
        }
    }

    /// Connects to a peer at the specified socket address
    pub async fn connect_to_peer(&self, addr: SocketAddr) -> std::io::Result<()> {
        let peer_info = Arc::clone(&self.node.peer_info);
        let name = self.node.name.clone();
        let message_handlers = Arc::clone(&self.message_handlers);

        let peer_display = {
            let mut peer_info_map = peer_info.lock().unwrap();
            if let Some(info) = peer_info_map.get_mut(&addr) {
                info.mark_connected();
                info.display_label()
            } else {
                let node_alias = format!("Unknown-{}", addr.port());
                let mut info = PeerInfo::new(addr, node_alias, None, Some(addr.port()));
                info.mark_connected();
                let label = info.display_label();
                peer_info_map.insert(addr, info);
                label
            }
        };

        let message_handlers_clone = Arc::clone(&message_handlers);
        // Use connection manager with retry logic
        self.connection_manager
            .connect_with_retry(
                addr,
                move |line| {
                    let name_clone = name.clone();
                    let message_handlers_clone2 = Arc::clone(&message_handlers_clone);
                    tokio::spawn(async move {
                        if let Err(e) =
                            Self::handle_message_static(line, name_clone, message_handlers_clone2)
                                .await
                        {
                            eprintln!("Failed to process message: {}", e);
                        }
                    });
                },
                peer_display,
            )
            .await
    }

    /// Handles an incoming TCP connection from a peer
    pub async fn handle_incoming_connection(
        &self,
        stream: tokio::net::TcpStream,
        addr: SocketAddr,
    ) -> std::io::Result<()> {
        let peer_info = Arc::clone(&self.node.peer_info);
        let name = self.node.name.clone();
        let message_handlers = Arc::clone(&self.message_handlers);

        let _peer_display = {
            let mut peer_info_map = peer_info.lock().unwrap();
            if let Some(info) = peer_info_map.get_mut(&addr) {
                info.mark_connected();
                info.display_label()
            } else {
                let node_alias = format!("Unknown-{}", addr.port());
                let mut info = PeerInfo::new(addr, node_alias, None, Some(addr.port()));
                info.mark_connected();
                let label = info.display_label();
                peer_info_map.insert(addr, info);
                label
            }
        };

        let node_service_clone = self.clone();
        let node_registry_clone = Arc::clone(&self.node_registry);
        handle_incoming_connection(
            stream,
            addr,
            Arc::clone(&self.node.peers),
            Some(Arc::clone(&self.connection_manager)),
            move |line| {
                let node_service_clone = node_service_clone.clone();
                let name_clone = name.clone();
                let message_handlers_clone = Arc::clone(&message_handlers);
                let node_registry_clone = Arc::clone(&node_registry_clone);
                let source_addr = addr; // The source address is the `addr` parameter
                tokio::spawn(async move {
                    if let Err(e) = node_service_clone
                        .handle_message_with_registry(
                            line,
                            source_addr, // Pass the actual source address
                            name_clone,
                            message_handlers_clone,
                            node_registry_clone,
                        )
                        .await
                    {
                        eprintln!("Failed to process message: {}", e);
                    }
                });
            },
        )
        .await
    }

    /// Broadcasts a message to all connected peers
    pub async fn broadcast_message(&self, content: String) -> std::io::Result<()> {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        let message = Message::Chat {
            from: self.node.name.clone(),
            content: trimmed.to_string(),
            from_user_id: Some(self.get_user_id()),
            from_address: None,
            to_user_id: None,
            to_address: None,
        };
        let json = serde_json::to_string(&message).unwrap();

        let json_line = format!("{}\n", json);
        // println!("Preparing to send JSON message: {}", json);

        let peers = {
            let guard = self.node.peers.lock().unwrap();
            guard.clone()
        };

        if peers.is_empty() {
            println!("No connected nodes currently\n");
            return Ok(());
        }

        // println!("Sending message to {} nodes\n", peers.len());

        // Use enhanced send with retry logic
        let mut failed_addrs = Vec::new();
        for (addr, _sender) in peers.iter() {
            match send_message_with_retry(*addr, json_line.clone(), Arc::clone(&self.node.peers), 3)
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Failed to send message to {}: {}\n", addr, e);
                    failed_addrs.push(*addr);
                }
            }
        }

        // Remove failed connections
        if !failed_addrs.is_empty() {
            let mut guard = self.node.peers.lock().unwrap();
            for addr in failed_addrs {
                guard.remove(&addr);
                self.connection_manager.unregister_addr(&addr);
            }
        }

        Ok(())
    }

    /// Send a raw JSON message to a specific peer, opening a connection if necessary
    pub async fn send_json_message_to_peer(
        &self,
        addr: SocketAddr,
        message_json: String,
    ) -> MeshTalkResult<()> {
        self.connection_manager.register_connection_for_addr(addr);
        if !self.connection_manager.is_connected(&addr) {
            self.connect_to_peer(addr).await.map_err(|e| {
                MeshTalkError::network_with_source(
                    NetworkErrorKind::ConnectionFailed,
                    format!("Failed to connect to {addr}"),
                    Box::new(e),
                )
            })?;
        }

        info!("[NODE SERVICE] Sending message via TCP to {}", addr);
        let json_line = format!("{}\n", message_json);
        send_message_with_retry(addr, json_line, Arc::clone(&self.node.peers), 3).await
    }

    /// Send a message to a peer identified by user_id, reusing existing TCP connections where possible
    pub async fn send_json_message_to_user(
        &self,
        user_id: &str,
        message_json: String,
    ) -> MeshTalkResult<()> {
        let status = self.ensure_connection_for_user(user_id).await?;

        info!(
            "[NODE SERVICE] Sending message to user_id {} via {} (reused={})",
            user_id, status.addr, status.reused
        );

        let json_line = format!("{}\n", message_json);
        send_message_with_retry(status.addr, json_line, Arc::clone(&self.node.peers), 3).await
    }

    /// Ensure there is an active TCP connection for the provided user_id, establishing one if required
    pub async fn ensure_connection_for_user(
        &self,
        user_id: &str,
    ) -> MeshTalkResult<ConnectionStatus> {
        let trimmed = user_id.trim();
        if trimmed.is_empty() {
            return Err(MeshTalkError::network(
                NetworkErrorKind::ConnectionFailed,
                "user_id is required to establish a connection",
            ));
        }

        if let Some(addr) = self.connection_manager.address_for_user(trimmed) {
            if self.connection_manager.is_connected(&addr) {
                info!(
                    "[NODE SERVICE] Reusing existing TCP channel for user_id {} at {}",
                    trimmed, addr
                );
                return Ok(ConnectionStatus { addr, reused: true });
            }

            info!(
                "[NODE SERVICE] Found stale TCP mapping for user_id {} at {}. Removing mapping before reconnecting.",
                trimmed,
                addr
            );
            self.connection_manager.unregister_addr(&addr);
        }

        let addr = {
            let registry = self.node_registry.lock().unwrap();
            registry.get_node_by_user_id(trimmed).map(|node| node.addr)
        }
        .ok_or_else(|| {
            MeshTalkError::network(
                NetworkErrorKind::ConnectionFailed,
                format!("No known address for user_id {trimmed}"),
            )
        })?;

        info!(
            "[NODE SERVICE] Establishing new TCP connection to {} for user_id {}",
            addr, trimmed
        );
        self.connect_to_peer(addr).await.map_err(|e| {
            MeshTalkError::network_with_source(
                NetworkErrorKind::ConnectionFailed,
                format!("Failed to connect to {} for user_id {}", addr, trimmed),
                Box::new(e),
            )
        })?;

        self.connection_manager
            .register_user_connection(trimmed.to_string(), addr);

        Ok(ConnectionStatus {
            addr,
            reused: false,
        })
    }

    /// Handles a received message by parsing it and making basic responses
    pub async fn handle_message_static(
        line: String,
        _node_name: String,
        message_handlers: Arc<RwLock<Vec<MessageEventHandler>>>,
    ) -> Result<(), serde_json::Error> {
        // println!("Trying to parse message: {}", line);
        match serde_json::from_str(&line) {
            Ok(Message::Chat {
                from,
                content,
                from_user_id,
                from_address,
                to_user_id,
                to_address,
            }) => {
                println!(
                    "
Received message from {}: {}
",
                    from, content
                );

                // Notify message handlers
                let handlers = message_handlers.read().await;
                let event = MessageEvent {
                    from_address: from_address.unwrap_or_default(),
                    sender_name: Some(from.clone()),
                    from_user_id,
                    to_user_id,
                    to_address,
                    content: content.clone(),
                };
                for handler in handlers.iter() {
                    handler(event.clone());
                }

                // Send notification for new message
                let _ = NOTIFICATION_SERVICE.send_new_message_notification(&from, &content);

                print!("> ");
                std::io::stdout().flush().unwrap();
            }
            Ok(Message::Discovery {
                name,
                port,
                username,
                ..
            }) => {
                println!(
                    "Received Discovery message from {} (user: {:?}), port: {}
",
                    name, username, port
                );
            }
            Ok(Message::Heartbeat {
                name,
                port,
                username,
                ..
            }) => {
                println!(
                    "Received Heartbeat message from {} (user: {:?}), port: {}
",
                    name, username, port
                );

                // Update peer info with heartbeat
                // Note: We don't have access to peer_info here, but in a real implementation
                // we would update the heartbeat timestamp for this peer
            }
            Ok(Message::ContactRequest { .. }) => {
                // Contact requests should be handled separately in the instance method
                // For now, just log it in the static method
                println!("Received contact request in static handler - should be handled by instance method");
            }
            Ok(Message::ContactResponse { .. }) => {
                // Contact responses should be handled separately in the instance method
                // For now, just log it in the static method
                println!("Received contact response in static handler - should be handled by instance method");
            }
            Ok(Message::FileOffer(_))
            | Ok(Message::FileChunk(_))
            | Ok(Message::FileAck(_))
            | Ok(Message::FileComplete(_)) => {
                // File transfers are handled through the instance-specific logic.
            }
            Err(e) => {
                eprintln!("Failed to parse message: {} (raw message: {})\n", e, line);
                return Err(e);
            }
        }

        Ok(())
    }

    /// Returns a clone of the peers HashMap wrapped in Arc<Mutex<...>>
    pub fn get_peers(&self) -> Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>> {
        Arc::clone(&self.node.peers)
    }

    /// Returns the name of this node
    pub fn get_name(&self) -> String {
        self.node.name.clone()
    }

    /// Returns the port this node is listening on
    pub fn get_port(&self) -> u16 {
        self.node.port
    }

    /// Updates the port this node is listening on
    pub fn update_port(&mut self, new_port: u16) {
        self.node.port = new_port;
    }

    /// Updates the name of this node
    pub fn update_name(&mut self, new_name: String) {
        self.node.name = new_name;
    }

    /// Returns a clone of the node registry wrapped in Arc<Mutex<...>>
    pub fn get_node_registry(&self) -> Arc<Mutex<NodeRegistry>> {
        Arc::clone(&self.node_registry)
    }

    /// Returns a clone of the connection manager
    pub fn get_connection_manager(&self) -> Arc<ConnectionManager> {
        Arc::clone(&self.connection_manager)
    }

    /// Returns a clone of the reconnection manager
    pub fn get_reconnection_manager(&self) -> Arc<ReconnectionManager> {
        Arc::clone(&self.reconnection_manager)
    }

    /// Register a message event handler
    pub async fn register_message_handler<F>(&self, handler: F)
    where
        F: Fn(MessageEvent) + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        self.message_handlers.write().await.push(handler);
    }

    /// Detect disconnected nodes and update their status
    pub fn detect_disconnected_nodes(&self) {
        self.connection_manager.detect_disconnected_nodes();
    }
}
