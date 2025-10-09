use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

/// Node status enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeStatus {
    /// Node is online and connected
    Online,
    /// Node is offline or unreachable
    Offline,
    /// Node connection is being established
    Connecting,
    /// Node connection failed
    ConnectionFailed,
}

fn heartbeat_timeout_duration() -> Duration {
    env::var("MESH_TALK_HEARTBEAT_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(20))
}

/// Discovered node information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredNode {
    /// Node address
    pub addr: SocketAddr,
    /// Node name
    pub name: String,
    /// Advertised username
    pub username: Option<String>,
    /// Advertised TCP listening IP
    pub remote_ip: Option<String>,
    /// Advertised TCP listening port
    pub listen_port: Option<u16>,
    /// Last time we received a heartbeat from this node
    pub last_heartbeat: SystemTime,
    /// Last time we successfully connected to this node
    pub last_connected: Option<SystemTime>,
    /// Node status
    pub status: NodeStatus,
    /// Number of consecutive connection failures
    pub failure_count: u32,
    /// User ID of the node (if provided in discovery)
    pub user_id: Option<String>,
}

impl DiscoveredNode {
    /// Create a new discovered node
    pub fn new(
        addr: SocketAddr,
        name: String,
        username: Option<String>,
        listen_port: Option<u16>,
    ) -> Self {
        Self {
            addr,
            name,
            username,
            remote_ip: None,
            listen_port,
            last_heartbeat: SystemTime::now(),
            last_connected: None,
            status: NodeStatus::Offline,
            failure_count: 0,
            user_id: None,
        }
    }

    /// Update the heartbeat timestamp
    pub fn update_heartbeat(&mut self) {
        self.last_heartbeat = SystemTime::now();
        self.status = NodeStatus::Online;
    }

    /// Mark node as connected
    pub fn mark_connected(&mut self) {
        self.last_connected = Some(SystemTime::now());
        self.status = NodeStatus::Online;
        self.failure_count = 0;
    }

    /// Mark node as connection failed
    pub fn mark_connection_failed(&mut self) {
        self.status = NodeStatus::ConnectionFailed;
        self.failure_count += 1;
    }

    /// Mark node as connecting
    pub fn mark_connecting(&mut self) {
        self.status = NodeStatus::Connecting;
    }

    /// Check if the node is timed out (no heartbeat for 30 seconds)
    pub fn is_timed_out(&self) -> bool {
        if let Ok(duration) = self.last_heartbeat.elapsed() {
            duration > heartbeat_timeout_duration()
        } else {
            false
        }
    }

    /// Check if the node should be reconnected (offline or connection failed with exponential backoff)
    pub fn should_reconnect(&self) -> bool {
        match self.status {
            NodeStatus::Offline | NodeStatus::ConnectionFailed => {
                // Exponential backoff: wait 2^failure_count seconds before retrying
                let backoff_time = Duration::from_secs(2u64.pow(self.failure_count.min(10)));
                if let Some(last_connected) = self.last_connected {
                    if let Ok(duration) = last_connected.elapsed() {
                        duration > backoff_time
                    } else {
                        true
                    }
                } else {
                    true
                }
            }
            _ => false,
        }
    }
}

/// Node registry for tracking discovered nodes
#[derive(Debug, Clone)]
pub struct NodeRegistry {
    /// Map of discovered nodes (key: node address, value: DiscoveredNode)
    nodes: HashMap<SocketAddr, DiscoveredNode>,
    /// Index mapping user_id to node address for fast lookup (key: user_id, value: node address)
    user_id_index: HashMap<String, SocketAddr>,
}

impl NodeRegistry {
    /// Create a new node registry
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            user_id_index: HashMap::new(),
        }
    }

    /// Add or update a discovered node
    pub fn add_or_update_node(
        &mut self,
        addr: SocketAddr,
        name: String,
        username: Option<String>,
        listen_port: Option<u16>,
        user_id: Option<String>,
    ) {
        let ip = addr.ip();
        let resolved_port = listen_port.unwrap_or_else(|| addr.port());
        let normalized_addr = SocketAddr::new(ip, resolved_port);

        let existing_key = if self.nodes.contains_key(&normalized_addr) {
            Some(normalized_addr)
        } else {
            self.nodes
                .keys()
                .find(|existing| existing.ip() == ip && existing.port() == resolved_port)
                .cloned()
        };

        if let Some(key) = existing_key {
            if key == normalized_addr {
                if let Some(node) = self.nodes.get_mut(&key) {
                    // Remove old user_id from index if it exists and differs from new one
                    if let Some(old_user_id) = &node.user_id {
                        if user_id.as_ref() != Some(old_user_id) {
                            self.user_id_index.remove(old_user_id);
                        }
                    }

                    node.addr = normalized_addr;
                    node.name = name;
                    node.username = username;
                    node.listen_port = Some(resolved_port);
                    if let Some(id) = user_id {
                        println!(
                            "[NODE REGISTRY] Updating node {} with user_id: {}",
                            normalized_addr, id
                        );
                        node.user_id = Some(id.clone());
                        // Add to user_id_index
                        self.user_id_index.insert(id, normalized_addr);
                    } else {
                        println!(
                            "[NODE REGISTRY] Node {} has no user_id to update",
                            normalized_addr
                        );
                    }
                    node.update_heartbeat();
                }
            } else if let Some(mut node) = self.nodes.remove(&key) {
                // Remove old entry from user_id_index if node had a user_id
                if let Some(old_user_id) = &node.user_id {
                    self.user_id_index.remove(old_user_id);
                }

                node.addr = normalized_addr;
                node.name = name;
                node.username = username;
                node.listen_port = Some(resolved_port);
                if let Some(id) = user_id {
                    println!(
                        "[NODE REGISTRY] Normalizing node to {} with user_id: {}",
                        normalized_addr, id
                    );
                    node.user_id = Some(id.clone());
                    // Add new entry to user_id_index
                    self.user_id_index.insert(id, normalized_addr);
                } else {
                    println!(
                        "[NODE REGISTRY] Normalized node {} has no user_id to update",
                        normalized_addr
                    );
                }
                node.update_heartbeat();
                println!("Discovered node normalized to {}", normalized_addr);
                self.nodes.insert(normalized_addr, node);
            }
        } else {
            let mut node = DiscoveredNode::new(
                normalized_addr,
                name.clone(),
                username.clone(),
                Some(resolved_port),
            );
            if let Some(id) = user_id {
                println!(
                    "[NODE REGISTRY] Creating new node {} with user_id: {}",
                    normalized_addr, id
                );
                node.user_id = Some(id.clone());
                // Add to user_id_index
                self.user_id_index.insert(id, normalized_addr);
            } else {
                println!(
                    "[NODE REGISTRY] Creating new node {} without user_id",
                    normalized_addr
                );
            }
            node.update_heartbeat();
            println!(
                "Discovered new node: addr={} name='{}' username='{:#?}' port={}",
                normalized_addr, name, username, resolved_port
            );
            self.nodes.insert(normalized_addr, node);
        }
    }

    /// Return a cloned snapshot of all discovered nodes
    pub fn snapshot(&self) -> Vec<DiscoveredNode> {
        self.nodes.values().cloned().collect()
    }

    /// Update node status
    pub fn update_node_status(&mut self, addr: SocketAddr, status: NodeStatus) {
        if let Some(node) = self.nodes.get_mut(&addr) {
            match status {
                NodeStatus::Online => node.mark_connected(),
                NodeStatus::ConnectionFailed => node.mark_connection_failed(),
                NodeStatus::Connecting => node.mark_connecting(),
                _ => node.status = status,
            }
        }
    }

    /// Get a node by address
    pub fn get_node(&self, addr: SocketAddr) -> Option<&DiscoveredNode> {
        self.nodes.get(&addr)
    }

    /// Get a mutable reference to a node by address
    pub fn get_node_mut(&mut self, addr: SocketAddr) -> Option<&mut DiscoveredNode> {
        self.nodes.get_mut(&addr)
    }

    /// Get all nodes
    pub fn get_all_nodes(&self) -> Vec<&DiscoveredNode> {
        self.nodes.values().collect()
    }

    /// Get online nodes
    pub fn get_online_nodes(&self) -> Vec<&DiscoveredNode> {
        self.nodes
            .values()
            .filter(|node| node.status == NodeStatus::Online)
            .collect()
    }

    /// Get nodes that should be reconnected
    pub fn get_nodes_to_reconnect(&self) -> Vec<&DiscoveredNode> {
        self.nodes
            .values()
            .filter(|node| node.should_reconnect())
            .collect()
    }

    /// Mark timed out nodes as offline and return their addresses.
    pub fn remove_timed_out_nodes(&mut self) -> Vec<SocketAddr> {
        let mut timed_out = Vec::new();

        for (addr, node) in self.nodes.iter_mut() {
            if node.is_timed_out() {
                node.status = NodeStatus::Offline;
                timed_out.push(*addr);
            }
        }

        timed_out
    }

    /// Get a node by user_id
    pub fn get_node_by_user_id(&self, user_id: &str) -> Option<&DiscoveredNode> {
        if let Some(addr) = self.user_id_index.get(user_id) {
            self.nodes.get(addr)
        } else {
            None
        }
    }

    /// Get a mutable reference to a node by user_id
    pub fn get_node_by_user_id_mut(&mut self, user_id: &str) -> Option<&mut DiscoveredNode> {
        if let Some(addr) = self.user_id_index.get(user_id) {
            self.nodes.get_mut(addr)
        } else {
            None
        }
    }

    /// Remove a node by user_id
    pub fn remove_node_by_user_id(&mut self, user_id: &str) -> Option<DiscoveredNode> {
        if let Some(addr) = self.user_id_index.get(user_id) {
            let addr = *addr;
            self.user_id_index.remove(user_id);
            self.nodes.remove(&addr)
        } else {
            None
        }
    }

    /// Get the number of nodes
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    #[test]
    fn test_node_registry_creation() {
        let registry = NodeRegistry::new();
        assert!(registry.is_empty());
    }

    #[test]
    fn test_add_or_update_node() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);

        registry.add_or_update_node(addr, "test-node".to_string(), None, Some(addr.port()), None);
        assert_eq!(registry.len(), 1);

        let node = registry.get_node(addr).unwrap();
        assert_eq!(node.name, "test-node");
        assert_eq!(node.status, NodeStatus::Online);
    }

    #[test]
    fn test_add_or_update_node_with_user_id() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);
        let user_id = "user123".to_string();

        registry.add_or_update_node(
            addr,
            "test-node".to_string(),
            Some("username".to_string()),
            Some(addr.port()),
            Some(user_id.clone()),
        );
        assert_eq!(registry.len(), 1);

        // Test that we can get the node by user_id
        let node = registry.get_node_by_user_id(&user_id).unwrap();
        assert_eq!(node.name, "test-node");
        assert_eq!(node.user_id.as_ref(), Some(&user_id));
        assert_eq!(node.username.as_ref(), Some(&"username".to_string()));
    }

    #[test]
    fn test_get_node_by_user_id_not_found() {
        let registry = NodeRegistry::new();
        assert!(registry.get_node_by_user_id("nonexistent").is_none());
    }

    #[test]
    fn test_remove_node_by_user_id() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);
        let user_id = "user456".to_string();

        registry.add_or_update_node(
            addr,
            "test-node".to_string(),
            Some("username".to_string()),
            Some(addr.port()),
            Some(user_id.clone()),
        );

        // Verify node exists
        assert!(registry.get_node_by_user_id(&user_id).is_some());

        // Remove node by user_id
        let removed_node = registry.remove_node_by_user_id(&user_id);
        assert!(removed_node.is_some());
        assert_eq!(removed_node.unwrap().name, "test-node");

        // Verify node no longer exists
        assert!(registry.get_node_by_user_id(&user_id).is_none());
    }

    #[test]
    fn test_update_node_with_new_user_id() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);
        let old_user_id = "old_user".to_string();
        let new_user_id = "new_user".to_string();

        // Add node with old user_id
        registry.add_or_update_node(
            addr,
            "test-node".to_string(),
            Some("username".to_string()),
            Some(addr.port()),
            Some(old_user_id.clone()),
        );

        // Verify old user_id works
        assert!(registry.get_node_by_user_id(&old_user_id).is_some());
        assert!(registry.get_node_by_user_id(&new_user_id).is_none());

        // Update node with new user_id
        registry.add_or_update_node(
            addr,
            "test-node".to_string(),
            Some("username".to_string()),
            Some(addr.port()),
            Some(new_user_id.clone()),
        );

        // Verify old user_id no longer works, but new one does
        assert!(registry.get_node_by_user_id(&old_user_id).is_none());
        assert!(registry.get_node_by_user_id(&new_user_id).is_some());
    }

    #[test]
    fn test_update_node_status() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);

        registry.add_or_update_node(addr, "test-node".to_string(), None, Some(addr.port()), None);
        registry.update_node_status(addr, NodeStatus::ConnectionFailed);

        let node = registry.get_node(addr).unwrap();
        assert_eq!(node.status, NodeStatus::ConnectionFailed);
        assert_eq!(node.failure_count, 1);
    }

    #[test]
    fn test_should_reconnect() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);

        registry.add_or_update_node(addr, "test-node".to_string(), None, Some(addr.port()), None);
        registry.update_node_status(addr, NodeStatus::ConnectionFailed);

        let node = registry.get_node(addr).unwrap();
        assert!(node.should_reconnect());
    }

    #[test]
    fn test_remove_timed_out_nodes_removes_entries() {
        let mut registry = NodeRegistry::new();
        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7001);

        registry.add_or_update_node(
            addr1,
            "test-node-1".to_string(),
            None,
            Some(addr1.port()),
            None,
        );
        registry.add_or_update_node(
            addr2,
            "test-node-2".to_string(),
            None,
            Some(addr2.port()),
            None,
        );

        // Manually set one node to timeout
        if let Some(node) = registry.get_node_mut(addr1) {
            node.last_heartbeat = SystemTime::now() - Duration::from_secs(60);
        }

        let timed_out = registry.remove_timed_out_nodes();
        assert_eq!(timed_out, vec![addr1]);
        assert_eq!(registry.len(), 2); // Nodes remain but status should change
        let node1 = registry.get_node(addr1).expect("node1 exists");
        assert_eq!(node1.status, NodeStatus::Offline);
        assert!(registry.get_node(addr2).is_some()); // Node 2 should still be there
    }
}
