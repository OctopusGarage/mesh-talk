use crate::domain::node_registry::{NodeRegistry, NodeStatus};
use crate::error::{MeshTalkError, MeshTalkResult, NetworkErrorKind};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

/// Connection pool for managing TCP connections
pub struct ConnectionPool {
    connections: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionPool {
    /// Creates a new connection pool
    pub fn new() -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Gets the connections map wrapped in Arc<Mutex<...>>
    pub fn get_connections(&self) -> Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>> {
        Arc::clone(&self.connections)
    }

    /// Checks if a connection exists for the given address
    pub fn has_connection(&self, addr: &SocketAddr) -> bool {
        let connections = self.connections.lock().unwrap();
        connections.contains_key(addr)
    }

    /// Removes a connection for the given address
    pub fn remove_connection(&self, addr: &SocketAddr) {
        let mut connections = self.connections.lock().unwrap();
        connections.remove(addr);
    }
}

/// Enhanced TCP connection with keep-alive and timeout features
pub struct EnhancedTcpConnection {
    addr: SocketAddr,
    sender: mpsc::Sender<String>,
    last_activity: std::time::Instant,
}

impl EnhancedTcpConnection {
    /// Creates a new enhanced TCP connection
    pub fn new(addr: SocketAddr, sender: mpsc::Sender<String>) -> Self {
        Self {
            addr,
            sender,
            last_activity: std::time::Instant::now(),
        }
    }

    /// Gets the connection address
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Gets the sender for sending messages
    pub fn sender(&self) -> &mpsc::Sender<String> {
        &self.sender
    }

    /// Updates the last activity timestamp
    pub fn update_activity(&mut self) {
        self.last_activity = std::time::Instant::now();
    }

    /// Checks if the connection has timed out (30 seconds of inactivity)
    pub fn is_timed_out(&self) -> bool {
        self.last_activity.elapsed() > Duration::from_secs(30)
    }
}

/// Connects to a peer with enhanced features including timeout and retry logic
pub async fn connect_to_peer<F>(
    addr: SocketAddr,
    peers: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
    node_registry: Arc<Mutex<NodeRegistry>>,
    message_handler: F,
    peer_name: &str,
) -> MeshTalkResult<()>
where
    F: Fn(String) + Send + 'static,
{
    // Check if already connected
    if peers.lock().unwrap().contains_key(&addr) {
        // println!("Already connected to node {}", addr);
        return Ok(());
    }

    // Attempt to connect with a timeout
    let stream = timeout(Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .map_err(|_| {
            MeshTalkError::network(
                NetworkErrorKind::ConnectionTimeout,
                format!("Connection timeout to {}", addr),
            )
        })?
        .map_err(|e| {
            eprintln!("Failed to connect to {}: {}", addr, e);
            MeshTalkError::network_with_source(
                NetworkErrorKind::ConnectionFailed,
                format!("Failed to connect to {}", addr),
                Box::new(e),
            )
        })?;

    // Set TCP keep-alive options
    if let Err(e) = stream.set_nodelay(true) {
        eprintln!("Failed to set TCP_NODELAY: {}", e);
    }

    let (reader, writer) = stream.into_split();
    let registry_for_send = Arc::clone(&node_registry);
    let registry_for_recv = Arc::clone(&node_registry);

    let (tx, mut rx) = mpsc::channel(32);
    peers.lock().unwrap().insert(addr, tx.clone());
    println!("Connected to node '{}' at {}", peer_name, addr);

    // Spawn task for sending messages
    let mut writer = BufWriter::new(writer);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            // println!("Sending message: {}", msg.trim());

            // Add timeout for write operations
            match timeout(Duration::from_secs(5), writer.write_all(msg.as_bytes())).await {
                Ok(Ok(())) => match timeout(Duration::from_secs(5), writer.flush()).await {
                    Ok(Ok(())) => {
                        // println!("Message sent successfully");
                    }
                    Ok(Err(e)) => {
                        eprintln!("Failed to flush buffer: {}", e);
                        break;
                    }
                    Err(_) => {
                        eprintln!("Write flush timeout");
                        break;
                    }
                },
                Ok(Err(e)) => {
                    eprintln!("Failed to send message: {}", e);
                    break;
                }
                Err(_) => {
                    eprintln!("Write timeout");
                    break;
                }
            }
        }
        // println!("Message sending task ended: {}", addr);
        let mut registry = registry_for_send.lock().unwrap();
        if let Some(node) = registry.get_node_mut(addr) {
            node.status = NodeStatus::Offline;
        }
    });

    // Spawn task for receiving messages
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();

            // Add timeout for read operations
            match timeout(Duration::from_secs(60), reader.read_line(&mut line)).await {
                Ok(Ok(0)) => {
                    // println!("Connection closed: {}", addr);
                    break;
                }
                Ok(Ok(_)) => {
                    // #[cfg(debug_assertions)] println!("Received raw message: {}", line);
                    message_handler(line.trim().to_string());
                }
                Ok(Err(e)) => {
                    eprintln!("Failed to read message: {}", e);
                    break;
                }
                Err(_) => {
                    // println!("Read timeout for connection: {}", addr);
                    // Send a keep-alive message or close connection
                    break;
                }
            }
        }

        // println!("Connection disconnected: {}", addr);
        peers.lock().unwrap().remove(&addr);
        {
            let mut registry = registry_for_recv.lock().unwrap();
            if let Some(node) = registry.get_node_mut(addr) {
                node.status = NodeStatus::Offline;
            }
        }
    });

    Ok(())
}

/// Handles incoming connections with enhanced features including timeout and retry logic
pub async fn handle_incoming_connection<F>(
    stream: TcpStream,
    addr: SocketAddr,
    peers: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
    connection_manager: Option<Arc<ConnectionManager>>,
    message_handler: F,
) -> std::io::Result<()>
where
    F: Fn(String) + Send + 'static,
{
    // Set TCP keep-alive options
    if let Err(e) = stream.set_nodelay(true) {
        eprintln!("Failed to set TCP_NODELAY: {}", e);
    }

    let (reader, writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel(32);

    peers.lock().unwrap().insert(addr, tx.clone());
    if let Some(manager) = &connection_manager {
        manager.register_connection_for_addr(addr);
    }
    println!("Accepting new connection from: {}", addr);

    // Spawn task for sending messages
    let mut writer = BufWriter::new(writer);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            // println!("Sending message: {}", msg.trim());

            // Add timeout for write operations
            match timeout(Duration::from_secs(5), writer.write_all(msg.as_bytes())).await {
                Ok(Ok(())) => match timeout(Duration::from_secs(5), writer.flush()).await {
                    Ok(Ok(())) => {
                        // println!("Message sent successfully");
                    }
                    Ok(Err(e)) => {
                        eprintln!("Failed to flush buffer: {}", e);
                        break;
                    }
                    Err(_) => {
                        eprintln!("Write flush timeout");
                        break;
                    }
                },
                Ok(Err(e)) => {
                    eprintln!("Failed to send message: {}", e);
                    break;
                }
                Err(_) => {
                    eprintln!("Write timeout");
                    break;
                }
            }
        }
        // println!("Message sending task ended: {}", addr);
    });

    // Spawn task for receiving messages
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();

            // Add timeout for read operations
            match timeout(Duration::from_secs(60), reader.read_line(&mut line)).await {
                Ok(Ok(0)) => {
                    // println!("Connection closed: {}", addr);
                    break;
                }
                Ok(Ok(_)) => {
                    // #[cfg(debug_assertions)] println!("Received raw message: {}", line);
                    message_handler(line.trim().to_string());
                }
                Ok(Err(e)) => {
                    eprintln!("Failed to read message: {}", e);
                    break;
                }
                Err(_) => {
                    // println!("Read timeout for connection: {}", addr);
                    // Send a keep-alive message or close connection
                    break;
                }
            }
        }

        // println!("Connection disconnected: {}", addr);
        peers.lock().unwrap().remove(&addr);
        if let Some(manager) = &connection_manager {
            manager.unregister_addr(&addr);
        }
    });

    Ok(())
}

/// Sends a message to a peer with retry logic and exponential backoff
pub async fn send_message_with_retry(
    addr: SocketAddr,
    message: String,
    peers: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
    max_retries: usize,
) -> MeshTalkResult<()> {
    let mut retries = 0;

    loop {
        // Get the sender for this peer
        let sender = {
            let peers_guard = peers.lock().unwrap();
            peers_guard.get(&addr).cloned()
        };

        match sender {
            Some(sender) => {
                // Try to send the message
                match sender.send(message.clone()).await {
                    Ok(()) => {
                        // println!("Message sent successfully to {}", addr);
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!("Failed to send message to {}: {}", addr, e);
                        // Remove the broken connection
                        peers.lock().unwrap().remove(&addr);
                    }
                }
            }
            None => {
                eprintln!("No connection found for peer: {}", addr);
            }
        }

        retries += 1;
        if retries >= max_retries {
            return Err(MeshTalkError::network(
                NetworkErrorKind::SendFailed,
                format!(
                    "Failed to send message to {} after {} retries",
                    addr, max_retries
                ),
            ));
        }

        // Wait before retrying with exponential backoff, capped at 5 seconds
        let backoff_ms = (100 * (2_u64.pow(retries as u32))).min(5000);
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
    }
}

/// Enhanced connection manager with retry logic and exponential backoff
pub struct ConnectionManager {
    peers: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
    node_registry: Arc<Mutex<NodeRegistry>>,
    user_connections: Arc<Mutex<HashMap<String, SocketAddr>>>,
    addr_users: Arc<Mutex<HashMap<SocketAddr, HashSet<String>>>>,
}

impl ConnectionManager {
    /// Creates a new connection manager with the provided peers and node registry
    pub fn new(
        peers: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
        node_registry: Arc<Mutex<NodeRegistry>>,
    ) -> Self {
        Self {
            peers,
            node_registry,
            user_connections: Arc::new(Mutex::new(HashMap::new())),
            addr_users: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a mapping from user_id to an active socket address
    pub fn register_user_connection(&self, user_id: impl Into<String>, addr: SocketAddr) {
        let user_id = user_id.into();

        let previous_addr = {
            let mut user_map = self.user_connections.lock().unwrap();
            user_map.insert(user_id.clone(), addr)
        };

        if let Some(existing_addr) = previous_addr {
            if existing_addr != addr {
                let mut addr_map = self.addr_users.lock().unwrap();
                if let Some(users) = addr_map.get_mut(&existing_addr) {
                    users.remove(&user_id);
                    if users.is_empty() {
                        addr_map.remove(&existing_addr);
                    }
                }
            }
        }

        let mut addr_map = self.addr_users.lock().unwrap();
        addr_map
            .entry(addr)
            .or_insert_with(HashSet::new)
            .insert(user_id);
    }

    /// Register connection by inspecting the node registry for the address
    pub fn register_connection_for_addr(&self, addr: SocketAddr) {
        let user_id = {
            let registry = self.node_registry.lock().unwrap();
            registry
                .get_node(addr)
                .and_then(|node| node.user_id.clone())
        };

        if let Some(user_id) = user_id {
            self.register_user_connection(user_id, addr);
        }
    }

    /// Remove all user mappings associated with an address
    pub fn unregister_addr(&self, addr: &SocketAddr) {
        let users = {
            let mut addr_map = self.addr_users.lock().unwrap();
            addr_map.remove(addr)
        };

        if let Some(users) = users {
            let mut user_map = self.user_connections.lock().unwrap();
            for user in users {
                if let Some(mapped_addr) = user_map.get(&user) {
                    if mapped_addr == addr {
                        user_map.remove(&user);
                    }
                }
            }
        }

        {
            let mut registry = self.node_registry.lock().unwrap();
            if let Some(node) = registry.get_node_mut(*addr) {
                node.status = NodeStatus::Offline;
            }
        }
    }

    /// Remove mapping for a specific user_id
    pub fn unregister_user(&self, user_id: &str) {
        if let Some(addr) = {
            let mut user_map = self.user_connections.lock().unwrap();
            user_map.remove(user_id)
        } {
            let mut addr_map = self.addr_users.lock().unwrap();
            if let Some(users) = addr_map.get_mut(&addr) {
                users.remove(user_id);
                if users.is_empty() {
                    addr_map.remove(&addr);
                }
            }
        }
    }

    /// Resolve a connected socket address for the provided user_id
    pub fn address_for_user(&self, user_id: &str) -> Option<SocketAddr> {
        let user_map = self.user_connections.lock().unwrap();
        user_map.get(user_id).copied()
    }

    /// Connects to a peer with retry logic and exponential backoff
    pub async fn connect_with_retry<F>(
        &self,
        addr: SocketAddr,
        message_handler: F,
        peer_name: String,
    ) -> std::io::Result<()>
    where
        F: Fn(String) + Send + 'static + Clone,
    {
        // Check if already connected
        if self.peers.lock().unwrap().contains_key(&addr) {
            // println!("Already connected to node {}", addr);
            return Ok(());
        }

        // Update node registry status
        {
            let mut registry = self.node_registry.lock().unwrap();
            if let Some(node) = registry.get_node_mut(addr) {
                node.mark_connecting();
            }
        }

        // Attempt to connect with exponential backoff
        let mut retries = 0;
        let max_retries = 5;

        loop {
            match connect_to_peer(
                addr,
                Arc::clone(&self.peers),
                Arc::clone(&self.node_registry),
                message_handler.clone(),
                &peer_name,
            )
            .await
            {
                Ok(_) => {
                    // Update node registry status
                    let user_id = {
                        let mut registry = self.node_registry.lock().unwrap();
                        if let Some(node) = registry.get_node_mut(addr) {
                            node.mark_connected();
                            node.user_id.clone()
                        } else {
                            None
                        }
                    };

                    if let Some(uid) = user_id {
                        self.register_user_connection(uid, addr);
                    } else {
                        self.register_connection_for_addr(addr);
                    }
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("Failed to connect to {}: {}", addr, e);

                    // Update node registry status
                    {
                        let mut registry = self.node_registry.lock().unwrap();
                        if let Some(node) = registry.get_node_mut(addr) {
                            node.mark_connection_failed();
                        }
                    }

                    retries += 1;
                    if retries >= max_retries {
                        return Err(std::io::Error::other(format!(
                            "Failed to send message to {} after {} retries",
                            addr, max_retries
                        )));
                    }

                    // Exponential backoff: wait 2^retries seconds before retrying
                    let backoff_time = Duration::from_secs(2u64.pow(retries as u32));
                    // println!("Retrying connection to {} in {:?}...", addr, backoff_time);
                    tokio::time::sleep(backoff_time).await;
                }
            }
        }
    }

    /// Disconnects from a peer and updates the node registry
    pub fn disconnect(&self, addr: &SocketAddr) {
        let mut peers = self.peers.lock().unwrap();
        peers.remove(addr);
        self.unregister_addr(addr);

        // Update node registry status
        let mut registry = self.node_registry.lock().unwrap();
        if let Some(node) = registry.get_node_mut(*addr) {
            node.status = crate::domain::node_registry::NodeStatus::Offline;
        }
    }

    /// Gets connection status for a peer
    pub fn is_connected(&self, addr: &SocketAddr) -> bool {
        let peers = self.peers.lock().unwrap();
        peers.contains_key(addr)
    }

    /// Checks if a peer is disconnected
    pub fn is_disconnected(&self, addr: &SocketAddr) -> bool {
        let peers = self.peers.lock().unwrap();
        !peers.contains_key(addr)
    }

    /// Detects disconnected nodes and updates their status in the registry
    pub fn detect_disconnected_nodes(&self) {
        let peers = self.peers.lock().unwrap();
        let mut registry = self.node_registry.lock().unwrap();

        // Get all node addresses from the registry
        let node_addresses: Vec<SocketAddr> = registry
            .get_all_nodes()
            .iter()
            .map(|node| node.addr)
            .collect();

        // Check each node in the registry
        for addr in node_addresses {
            // If the node is in the registry but not in the peers map, it's disconnected
            if !peers.contains_key(&addr) {
                if let Some(node) = registry.get_node_mut(addr) {
                    // Only update status if it's not already marked as disconnected
                    if node.status != crate::domain::node_registry::NodeStatus::Offline
                        && node.status != crate::domain::node_registry::NodeStatus::ConnectionFailed
                    {
                        node.status = crate::domain::node_registry::NodeStatus::Offline;
                    }
                }
            }
        }
    }

    /// Gets a clone of the node registry wrapped in Arc<Mutex<...>>
    pub fn get_node_registry(&self) -> Arc<Mutex<NodeRegistry>> {
        Arc::clone(&self.node_registry)
    }

    /// Gets a clone of the peers map wrapped in Arc<Mutex<...>>
    pub fn get_peers(&self) -> Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>> {
        Arc::clone(&self.peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_pool() {
        let pool = ConnectionPool::new();
        let connections = pool.get_connections();

        assert!(connections.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_enhanced_tcp_connection() {
        let (tx, _rx) = mpsc::channel(32);
        let addr = "127.0.0.1:8080".parse().unwrap();
        let mut connection = EnhancedTcpConnection::new(addr, tx);

        assert_eq!(connection.addr(), addr);
        assert!(!connection.is_timed_out());

        // Update activity
        connection.update_activity();
        assert!(!connection.is_timed_out());
    }

    #[tokio::test]
    async fn test_send_message_with_retry() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let addr = "127.0.0.1:8080".parse().unwrap();
        let result = send_message_with_retry(addr, "test".to_string(), peers, 1).await;

        // Should fail because there's no connection
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_connection_manager_user_mapping() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let manager = ConnectionManager::new(Arc::clone(&peers), Arc::clone(&registry));

        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        manager.register_user_connection("user123", addr);

        assert_eq!(manager.address_for_user("user123"), Some(addr));

        manager.unregister_addr(&addr);
        assert!(manager.address_for_user("user123").is_none());
    }
}
