use crate::commands::ChatMessageInfo;
use crate::domain::node_registry::NodeStatus;
use crate::perf_monitor;
use crate::services::node_service::{MessageEvent, NodeService};
use crate::state::AppState;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager, Runtime};
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{error, info};

#[derive(Clone, Debug)]
struct ContactStatusChange {
    address: SocketAddr,
    node_name: String,
    username: Option<String>,
    listen_port: Option<u16>,
    is_online: bool,
}

// Event names
pub const EVENT_MESSAGE_RECEIVED: &str = "message-received";
pub const EVENT_CONTACT_STATUS_CHANGED: &str = "contact-status-changed";
pub const EVENT_CONTACT_REQUEST_RECEIVED: &str = "contact-request-received";
pub const EVENT_CONTACT_RESPONSE_RECEIVED: &str = "contact-response-received";
pub const EVENT_CONTACT_ADDED: &str = "contact-added";
pub const EVENT_NETWORK_STATUS_CHANGED: &str = "network-status-changed";
pub const EVENT_NODE_PORT_CHANGED: &str = "node-port-changed";
pub const EVENT_NODES_DISCOVERED: &str = "nodes-discovered";

static NODE_EVENT_APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

pub fn set_node_event_app_handle(app_handle: &tauri::AppHandle) {
    let _ = NODE_EVENT_APP_HANDLE.set(app_handle.clone());
}

pub fn with_node_event_app_handle<F>(f: F)
where
    F: FnOnce(&tauri::AppHandle),
{
    if let Some(handle) = NODE_EVENT_APP_HANDLE.get() {
        f(handle);
    }
}

// Event data structures
#[derive(serde::Serialize, Clone)]
pub struct MessageReceivedEvent {
    pub message: ChatMessageInfo,
    pub sender_name: String,
    pub sender_address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<u64>,
}

#[derive(serde::Serialize, Clone)]
pub struct ContactStatusChangedEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<u64>,
    pub address: String,
    pub ip: String,
    pub status: String, // online, offline, away, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_label: Option<String>,
}

#[derive(serde::Serialize, Clone)]
pub struct ContactRequestReceivedEvent {
    pub requester_public_key: String,
    pub requester_alias: String,
    pub timestamp: u64,
    pub request_json: String,
}

#[derive(serde::Serialize, Clone)]
pub struct ContactResponseReceivedEvent {
    pub responder_public_key: String,
    pub responder_alias: String,
    pub approved: bool,
    pub timestamp: u64,
}

#[derive(serde::Serialize, Clone)]
pub struct ContactAddedEvent {
    pub public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

#[derive(serde::Serialize, Clone)]
pub struct NetworkStatusChangedEvent {
    pub status: String, // connected, disconnected, connecting, etc.
    pub peer_count: usize,
}

#[derive(serde::Serialize, Clone)]
pub struct NodePortChangedEvent {
    pub port: u16,
    pub ip: String,
}

#[derive(serde::Serialize, Clone)]
pub struct NodesDiscoveredEvent {
    pub nodes: Vec<DiscoveredNodeInfo>,
}

#[derive(serde::Serialize, Clone)]
pub struct DiscoveredNodeInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_port: Option<u16>,
    pub ip: String,
    pub port: u16,
    pub display_label: String,
    pub is_connected: bool,
}

// Function to emit a message received event
async fn emit_message_received<R: Runtime>(app_handle: tauri::AppHandle<R>, event: MessageEvent) {
    let payload = build_message_event_payload(&app_handle, &event);

    if let Err(e) = app_handle.emit(EVENT_MESSAGE_RECEIVED, payload) {
        error!("Failed to emit message received event: {}", e);
    }
}

// Function to emit a contact status changed event
fn emit_contact_status_changed<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    change: ContactStatusChange,
) {
    let address = change.address.to_string();
    let status = if change.is_online {
        "online".to_string()
    } else {
        "offline".to_string()
    };

    let (contact_id, name_override) = {
        let app_state: tauri::State<AppState> = app_handle.state();
        if let Some(session) = app_state.session().get() {
            app_state
                .contact_service()
                .find_contact_by_address(session.user.id, &address)
                .map(|contact| (Some(contact.id), Some(contact.name)))
                .unwrap_or((None, None))
        } else {
            (None, None)
        }
    };

    let node_name = name_override.unwrap_or(change.node_name.clone());
    let listen_port = change.listen_port.or(Some(change.address.port()));
    let display_label = format!(
        "{} • {} • {}:{}",
        node_name,
        change
            .username
            .clone()
            .unwrap_or_else(|| "Unknown".to_string()),
        change.address.ip(),
        listen_port.unwrap_or(change.address.port())
    );

    let event = ContactStatusChangedEvent {
        contact_id,
        address,
        ip: change.address.ip().to_string(),
        status,
        node_name: Some(node_name),
        username: change.username.clone(),
        listen_port,
        display_label: Some(display_label),
    };

    if let Err(e) = app_handle.emit(EVENT_CONTACT_STATUS_CHANGED, event) {
        error!("Failed to emit contact status changed event: {}", e);
    }
}

// Function to emit a network status changed event
pub fn emit_network_status_changed<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    status: String,
    peer_count: usize,
) {
    let event = NetworkStatusChangedEvent { status, peer_count };

    if let Err(e) = app_handle.emit(EVENT_NETWORK_STATUS_CHANGED, event) {
        error!("Failed to emit network status changed event: {}", e);
    }
}

pub fn emit_node_port_changed<R: Runtime>(app_handle: &tauri::AppHandle<R>, port: u16, ip: String) {
    let event = NodePortChangedEvent { port, ip };
    if let Err(e) = app_handle.emit(EVENT_NODE_PORT_CHANGED, event) {
        error!("Failed to emit node port changed event: {}", e);
    }
}

pub fn emit_nodes_discovered<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    nodes: Vec<DiscoveredNodeInfo>,
) {
    let event = NodesDiscoveredEvent { nodes };
    if let Err(e) = app_handle.emit(EVENT_NODES_DISCOVERED, event) {
        error!("Failed to emit nodes discovered event: {}", e);
    }
}

pub fn emit_contact_request_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    requester_public_key: String,
    requester_alias: String,
    request_json: String,
) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::from_secs(0))
        .as_secs();

    let event = ContactRequestReceivedEvent {
        requester_public_key,
        requester_alias,
        timestamp,
        request_json,
    };

    if let Err(e) = app_handle.emit(EVENT_CONTACT_REQUEST_RECEIVED, event) {
        error!("Failed to emit contact request received event: {}", e);
    }
}

pub fn emit_contact_response_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    responder_public_key: String,
    responder_alias: String,
    approved: bool,
    timestamp: u64,
) {
    let event = ContactResponseReceivedEvent {
        responder_public_key,
        responder_alias,
        approved,
        timestamp,
    };

    if let Err(e) = app_handle.emit(EVENT_CONTACT_RESPONSE_RECEIVED, event) {
        error!("Failed to emit contact response received event: {}", e);
    }
}

pub fn emit_contact_added<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    public_key: String,
    alias: Option<String>,
    address: Option<String>,
) {
    let event = ContactAddedEvent {
        public_key,
        alias,
        address,
    };

    if let Err(e) = app_handle.emit(EVENT_CONTACT_ADDED, event) {
        error!("Failed to emit contact added event: {}", e);
    }
}

// Function to set up event listeners for the NodeService
pub async fn setup_node_service_events(
    node_service: Arc<Mutex<NodeService>>,
    app_handle: tauri::AppHandle,
) {
    let _timer = perf_monitor!("setup_node_service_events");
    set_node_event_app_handle(&app_handle);

    // Clone the app handle for use in the async block
    let app_handle_clone = app_handle.clone();

    // Register a message handler with the NodeService
    {
        let service = node_service.lock().await;
        let app_handle = app_handle_clone.clone();
        service
            .register_message_handler(move |event| {
                let app_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    emit_message_received(app_handle, event).await;
                });
            })
            .await;
    }

    let contact_handle = app_handle.clone();
    let network_handle = app_handle.clone();
    let _nodes_handle = app_handle.clone();
    let node_for_status = Arc::clone(&node_service);
    tauri::async_runtime::spawn(async move {
        let mut previous_status: HashMap<SocketAddr, bool> = HashMap::new();
        let mut last_network_online: Option<bool> = None;

        loop {
            let (changes, new_status_map, connected_count) = {
                let service = node_for_status.lock().await;
                let peers_arc = service.get_peers();
                let connected: HashSet<SocketAddr> =
                    peers_arc.lock().unwrap().keys().copied().collect();

                let mut peer_info_map = service.node.peer_info.lock().unwrap();
                let (changes, statuses) =
                    compute_peer_status_deltas(&mut peer_info_map, &previous_status, &connected);
                (changes, statuses, connected.len())
            };

            for change in changes {
                emit_contact_status_changed(&contact_handle, change);
            }

            let network_online = connected_count > 0 || {
                // Check if we have any peers at all, even if not connected yet
                let service = node_for_status.lock().await;
                let peers_arc = service.get_peers();
                let peer_count = peers_arc.lock().unwrap().len();
                peer_count > 0
            };

            if last_network_online != Some(network_online) {
                emit_network_status_changed(
                    &network_handle,
                    if network_online {
                        "connected".to_string()
                    } else {
                        "disconnected".to_string()
                    },
                    connected_count,
                );
                last_network_online = Some(network_online);
            }

            previous_status = new_status_map;

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // Add a task to periodically emit discovered nodes
    let nodes_discovery_handle = app_handle.clone();
    let node_for_discovery = Arc::clone(&node_service);
    tauri::async_runtime::spawn(async move {
        loop {
            // Get discovered nodes and emit them
            let nodes = {
                let service = node_for_discovery.lock().await;
                let registry = service.node_registry.lock().unwrap();

                registry
                    .snapshot()
                    .into_iter()
                    .map(|entry| {
                        let ip = entry.addr.ip().to_string();
                        let port = entry.listen_port.unwrap_or(entry.addr.port());
                        let username_clone = entry.username.clone();
                        let display_label = format!(
                            "{} • {} • {}:{}",
                            entry.name,
                            username_clone
                                .clone()
                                .unwrap_or_else(|| "Unknown".to_string()),
                            ip,
                            port
                        );

                        DiscoveredNodeInfo {
                            name: entry.name,
                            username: username_clone,
                            address: entry.addr.to_string(),
                            listen_port: entry.listen_port,
                            ip,
                            port,
                            display_label,
                            is_connected: entry.status == NodeStatus::Online,
                        }
                    })
                    .collect::<Vec<DiscoveredNodeInfo>>()
            };

            emit_nodes_discovered(&nodes_discovery_handle, nodes);

            // Wait for 2 seconds before next update
            tokio::time::sleep(Duration::from_millis(2000)).await;
        }
    });

    // Add a task to periodically monitor IP changes and emit updates
    let ip_monitor_handle = app_handle.clone();
    let node_for_ip_monitor = Arc::clone(&node_service);
    tauri::async_runtime::spawn(async move {
        let mut previous_ip: Option<String> = None;

        loop {
            // Get current IP address
            let current_ip = get_local_ip_from_socket().unwrap_or_else(|| "127.0.0.1".to_string());

            // Check if IP has changed
            if let Some(prev_ip) = &previous_ip {
                if prev_ip != &current_ip {
                    // IP has changed, emit the event
                    let service = node_for_ip_monitor.lock().await;
                    let port = service.get_port();
                    drop(service); // Release the lock before emitting the event

                    emit_node_port_changed(&ip_monitor_handle, port, current_ip.clone());
                    info!("IP address changed from {} to {}", prev_ip, current_ip);
                }
            }

            // Update previous IP for next comparison
            previous_ip = Some(current_ip);

            // Check for IP changes every 10 seconds
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

/// Helper function to get the local IP address
fn get_local_ip_from_socket() -> Option<String> {
    // Try to infer the outward-facing interface without performing a full TCP handshake.
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(local_addr) = socket.local_addr() {
                let ip = local_addr.ip();
                if !ip.is_unspecified() {
                    return Some(ip.to_string());
                }
            }
        }
    }

    // Fallback to localhost when we cannot determine a better address.
    Some("127.0.0.1".to_string())
}

fn build_message_event_payload<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    event: &MessageEvent,
) -> MessageReceivedEvent {
    if let Some(app_state) = app_handle.try_state::<AppState>() {
        build_message_event_payload_with_state(event, Some(app_state.inner()))
    } else {
        build_message_event_payload_with_state(event, None)
    }
}

fn build_message_event_payload_with_state(
    event: &MessageEvent,
    app_state: Option<&AppState>,
) -> MessageReceivedEvent {
    let fallback = || fallback_message_event(event);

    let Some(app_state) = app_state else {
        return fallback();
    };

    let Some(session) = app_state.session().get() else {
        return fallback();
    };

    let sender_address = if event.from.trim().is_empty() {
        "unknown".to_owned()
    } else {
        event.from.clone()
    };

    let message_service = app_state.message_service();

    let persisted_message = message_service
        .create_message(
            0,
            sender_address.clone(),
            Some(session.user.id),
            Some(session.user.address.clone()),
            event.content.clone(),
        )
        .map_err(|err| {
            error!("Failed to persist incoming message: {:?}", err);
        })
        .ok()
        .and_then(|message| {
            if let Err(err) = message_service.mark_delivered(message.id) {
                error!("Failed to mark incoming message as delivered: {:?}", err);
            }

            message_service
                .get_message(message.id)
                .map_err(|err| {
                    error!("Failed to reload persisted message: {:?}", err);
                })
                .ok()
                .or(Some(message))
        });

    let contact_service = app_state.contact_service();
    let contact = contact_service.find_contact_by_address(session.user.id, &sender_address);
    let sender_name = contact
        .as_ref()
        .map(|contact| contact.name.clone())
        .unwrap_or_else(|| event.from.clone());
    let contact_id = contact.as_ref().map(|contact| contact.id);

    if let Some(message) = persisted_message {
        return MessageReceivedEvent {
            message: ChatMessageInfo::from(message),
            sender_name,
            sender_address,
            contact_id,
        };
    }

    fallback()
}

fn fallback_message_event(event: &MessageEvent) -> MessageReceivedEvent {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();

    MessageReceivedEvent {
        message: ChatMessageInfo {
            id: 0,
            from_user_id: 0,
            from_address: event.from.clone(),
            to_user_id: None,
            to_address: None,
            content: event.content.clone(),
            sent_at: timestamp,
            delivered_at: None,
            read_at: None,
            status: 0,
        },
        sender_name: event.from.clone(),
        sender_address: event.from.clone(),
        contact_id: None,
    }
}

fn compute_peer_status_deltas(
    peer_info_map: &mut HashMap<SocketAddr, crate::domain::models::PeerInfo>,
    previous_status: &HashMap<SocketAddr, bool>,
    connected: &HashSet<SocketAddr>,
) -> (Vec<ContactStatusChange>, HashMap<SocketAddr, bool>) {
    let mut new_status_map = HashMap::new();
    let mut changes = Vec::new();

    for addr in connected {
        peer_info_map.entry(*addr).or_insert_with(|| {
            let mut info = crate::domain::models::PeerInfo::new(
                *addr,
                format!("Unknown-{}", addr.port()),
                None,
                Some(addr.port()),
            );
            info.mark_connected();
            info
        });
    }

    for (addr, info) in peer_info_map.iter_mut() {
        let is_now_connected = connected.contains(addr);
        if is_now_connected && !info.is_connected {
            info.mark_connected();
        } else if !is_now_connected && info.is_connected {
            info.mark_disconnected();
        }

        let prev = previous_status.get(addr).copied().unwrap_or(false);
        if prev != is_now_connected {
            changes.push(ContactStatusChange {
                address: *addr,
                node_name: info.node_name.clone(),
                username: info.username.clone(),
                listen_port: info.listen_port,
                is_online: is_now_connected,
            });
        }

        new_status_map.insert(*addr, is_now_connected);
    }

    (changes, new_status_map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::PeerInfo;
    use crate::domain::models::User;
    use crate::services::auth_service::AuthService;
    use crate::services::contact_service::ContactService;
    use crate::services::message_service::MessageService;
    use crate::state::AppState;

    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn compute_peer_status_detects_online_change() {
        let mut peer_info = HashMap::new();
        let address = addr(7001);
        peer_info.insert(
            address,
            PeerInfo::new(address, "peer".into(), None, Some(address.port())),
        );

        let previous = HashMap::new();
        let connected = HashSet::from([address]);

        let (changes, statuses) = compute_peer_status_deltas(&mut peer_info, &previous, &connected);

        assert_eq!(changes.len(), 1);
        assert!(changes[0].is_online);
        assert_eq!(statuses.get(&address), Some(&true));
    }

    #[test]
    fn compute_peer_status_detects_offline_change() {
        let mut peer_info = HashMap::new();
        let address = addr(7002);
        let mut info = PeerInfo::new(address, "peer".into(), None, Some(address.port()));
        info.mark_connected();
        peer_info.insert(address, info);

        let previous = HashMap::from([(address, true)]);
        let connected = HashSet::new();

        let (changes, statuses) = compute_peer_status_deltas(&mut peer_info, &previous, &connected);

        assert_eq!(changes.len(), 1);
        assert!(!changes[0].is_online);
        assert_eq!(statuses.get(&address), Some(&false));
    }

    #[test]
    fn build_payload_persists_message_and_uses_contact_name() {
        // Create a temporary directory for test data
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory");
        let data_path = temp_dir.path().to_str().unwrap().to_string();

        let file_manager = crate::storage::file_manager::FileManager::new(data_path.into());

        // Initialize services with file manager
        let auth_service = AuthService::new(Arc::new(
            crate::identity::manager::IdentityManager::new(file_manager.clone()),
        ));
        let contact_manager = Arc::new(crate::contacts::manager::ContactManager::new(
            file_manager.clone(),
        ));
        let contact_service = ContactService::new(contact_manager);
        let message_service = MessageService::new(Arc::new(file_manager));

        let app_state = AppState::new(
            auth_service.clone(),
            contact_service.clone(),
            message_service.clone(),
            Arc::new(crate::api::AppConfig::default()),
        );

        let local_user = User::new(1, "local-user".into(), "127.0.0.1:7000".into());
        app_state.session().set("token".into(), local_user.clone());

        let event = MessageEvent {
            from: "peer-one".into(),
            content: "Hello from the network".into(),
        };

        let payload = build_message_event_payload_with_state(&event, Some(&app_state));

        assert!(payload.message.id > 0);
        assert_eq!(payload.message.content, "Hello from the network");
        assert_eq!(payload.message.to_user_id, Some(local_user.id));
        assert_eq!(payload.sender_name, "peer-one");
        assert_eq!(payload.sender_address, "peer-one");
        assert!(payload.contact_id.is_none());
        assert_eq!(payload.message.status, 1); // Delivered

        let persisted = app_state
            .message_service()
            .get_message(payload.message.id)
            .expect("message persisted");
        assert_eq!(persisted.content, "Hello from the network");
    }
}
