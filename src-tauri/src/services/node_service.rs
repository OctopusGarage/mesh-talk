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
use crate::state::AppState;
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
    pub from: String,
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
    /// User ID for the authenticated user (0 if not authenticated)
    pub user_id: Arc<AtomicU64>,
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
            user_id: Arc::new(AtomicU64::new(0)), // Initialize with 0 (not authenticated)
        }
    }

    /// Sets the authenticated user ID for this node service
    pub fn set_user_id(&self, user_id: u64) {
        self.user_id.store(user_id, Ordering::SeqCst);
    }

    /// Gets the authenticated user ID for this node service
    pub fn get_user_id(&self) -> u64 {
        self.user_id.load(Ordering::SeqCst)
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

        handle_incoming_connection(stream, addr, Arc::clone(&self.node.peers), move |line| {
            let name_clone = name.clone();
            let message_handlers_clone = Arc::clone(&message_handlers);
            tokio::spawn(async move {
                if let Err(e) =
                    Self::handle_message_static(line, name_clone, message_handlers_clone).await
                {
                    eprintln!("Failed to process message: {}", e);
                }
            });
        })
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
        if !self.connection_manager.is_connected(&addr) {
            self.connect_to_peer(addr).await.map_err(|e| {
                MeshTalkError::network_with_source(
                    NetworkErrorKind::ConnectionFailed,
                    format!("Failed to connect to {addr}"),
                    Box::new(e),
                )
            })?;
        }

        let json_line = format!("{}\n", message_json);
        send_message_with_retry(addr, json_line, Arc::clone(&self.node.peers), 3).await
    }

    /// Handles a received message by parsing it and performing appropriate actions based on message type
    pub async fn handle_message_static(
        line: String,
        _node_name: String,
        message_handlers: Arc<RwLock<Vec<MessageEventHandler>>>,
    ) -> Result<(), serde_json::Error> {
        // println!("Trying to parse message: {}", line);
        match serde_json::from_str(&line) {
            Ok(Message::Chat { from, content }) => {
                println!(
                    "
Received message from {}: {}
",
                    from, content
                );

                // Notify message handlers
                let handlers = message_handlers.read().await;
                let event = MessageEvent {
                    from: from.clone(),
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
                Ok(())
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
                Ok(())
            }
            Ok(Message::ContactRequest {
                requester_public_key,
                requester_alias,
                timestamp,
                signature,
                node_name,
                username,
                user_id: _, // Extract user_id but don't use it currently
                ip,
                port,
            }) => {
                println!(
                    "Received contact request from '{}' (alias: '{}') at {} (signature bytes: {})",
                    requester_public_key,
                    requester_alias,
                    timestamp,
                    signature.len()
                );

                let requester_public_key_event = requester_public_key.clone();
                let peer_username = username.clone().unwrap_or_else(|| "Unknown".to_string());
                let peer_node_name = node_name.clone().unwrap_or_else(|| "Unknown".to_string());
                let peer_ip = ip.unwrap_or_else(|| "unknown".to_string());
                let peer_port = port.unwrap_or_default();
                let composed_alias = if peer_port == 0 {
                    format!("{} • {} • {}", peer_username, peer_node_name, peer_ip)
                } else {
                    format!(
                        "{} • {} • {}:{}",
                        peer_username, peer_node_name, peer_ip, peer_port
                    )
                };
                let requester_alias_event = composed_alias.clone();
                let request_json = match serde_json::to_string(&Message::ContactRequest {
                    requester_public_key: requester_public_key.clone(),
                    requester_alias: composed_alias,
                    timestamp,
                    signature: signature.clone(),
                    node_name,
                    username,
                    user_id: None, // Add user_id as None for now since it's from an external request
                    ip: Some(peer_ip.clone()),
                    port: Some(peer_port),
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
                ..
            }) => {
                println!(
                    "Received contact response from '{}' (alias: '{}', approved: {}) at {} (signature bytes: {})",
                    responder_public_key,
                    responder_alias,
                    approved,
                    timestamp,
                    signature.len()
                );

                let response_json = match serde_json::to_string(&Message::ContactResponse {
                    responder_public_key: responder_public_key.clone(),
                    approved,
                    responder_alias: responder_alias.clone(),
                    timestamp,
                    signature: signature.clone(),
                    user_id: None, // Add user_id as None for now since it's from an external response
                }) {
                    Ok(json) => json,
                    Err(err) => {
                        eprintln!(
                            "Failed to serialize contact response for processing: {}",
                            err
                        );
                        String::new()
                    }
                };

                let responder_public_key_event = responder_public_key.clone();
                let responder_alias_event = responder_alias.clone();

                with_node_event_app_handle(move |app_handle| {
                    let app_handle = app_handle.clone();
                    let response_json = response_json.clone();
                    tauri::async_runtime::spawn(async move {
                        let app_state: tauri::State<AppState> = app_handle.state();
                        if let Some(session) = app_state.session().get() {
                            let service = app_state.contact_request_service();
                            let contact_service = app_state.contact_service();
                            if let Err(err) = service
                                .handle_contact_response(&session.user.name, "", &response_json)
                                .await
                            {
                                eprintln!(
                                    "Failed to persist contact response from {}: {}",
                                    responder_public_key_event, err
                                );
                            } else {
                                if let Err(err) = contact_service.add_contact(
                                    session.user.id,
                                    responder_alias_event.clone(),
                                    responder_public_key_event.clone(),
                                    None,
                                ) {
                                    eprintln!(
                                        "Warning: failed to record contact '{}' for user '{}': {:?}",
                                        responder_public_key_event, session.user.name, err
                                    );
                                }

                                emit_contact_added(
                                    &app_handle,
                                    responder_public_key_event.clone(),
                                    Some(responder_alias_event.clone()),
                                    Some(responder_public_key_event.clone()),
                                );
                                emit_contact_response_received(
                                    &app_handle,
                                    responder_public_key_event.clone(),
                                    responder_alias_event.clone(),
                                    approved,
                                    timestamp,
                                );
                            }
                        } else {
                            eprintln!(
                                "No active session; ignoring contact response from {}",
                                responder_public_key_event
                            );
                        }
                    });
                });
                Ok(())
            }
            Err(e) => {
                eprintln!(
                    "Failed to parse message: {} (raw message: {})
",
                    e, line
                );
                Err(e)
            }
        }
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
