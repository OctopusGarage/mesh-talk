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
        .unwrap_or_else(|| Duration::from_secs(60)) // Change from 30 to 60 seconds
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
            listen_port,
            last_heartbeat: SystemTime::now(),
            last_connected: None,
            status: NodeStatus::Offline,
            failure_count: 0,
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
}

impl NodeRegistry {
    /// Create a new node registry
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// Add or update a discovered node
    pub fn add_or_update_node(
        &mut self,
        addr: SocketAddr,
        name: String,
        username: Option<String>,
        listen_port: Option<u16>,
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
                    node.addr = normalized_addr;
                    node.name = name;
                    node.username = username;
                    node.listen_port = Some(resolved_port);
                    node.update_heartbeat();
                }
            } else if let Some(mut node) = self.nodes.remove(&key) {
                node.addr = normalized_addr;
                node.name = name;
                node.username = username;
                node.listen_port = Some(resolved_port);
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
            node.update_heartbeat();
            println!(
                "Discovered new node: addr={} name='{}' username='{:#?}' port={}",
                normalized_addr,
                name,
                username.as_deref().unwrap_or("Unknown"),
                resolved_port
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

    /// Remove timed out nodes from the registry and return their addresses.
    pub fn remove_timed_out_nodes(&mut self) -> Vec<SocketAddr> {
        let mut to_remove = Vec::new();

        // Collect addresses of timed out nodes
        for (addr, node) in &self.nodes {
            if node.is_timed_out() {
                to_remove.push(*addr);
            }
        }

        // Remove timed out nodes and collect their addresses
        let mut removed_addrs = Vec::new();
        for addr in to_remove {
            if self.nodes.remove(&addr).is_some() {
                removed_addrs.push(addr);
            }
        }

        removed_addrs
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

        registry.add_or_update_node(addr, "test-node".to_string(), None, Some(addr.port()));
        assert_eq!(registry.len(), 1);

        let node = registry.get_node(addr).unwrap();
        assert_eq!(node.name, "test-node");
        assert_eq!(node.status, NodeStatus::Online);
    }

    #[test]
    fn test_update_node_status() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);

        registry.add_or_update_node(addr, "test-node".to_string(), None, Some(addr.port()));
        registry.update_node_status(addr, NodeStatus::ConnectionFailed);

        let node = registry.get_node(addr).unwrap();
        assert_eq!(node.status, NodeStatus::ConnectionFailed);
        assert_eq!(node.failure_count, 1);
    }

    #[test]
    fn test_should_reconnect() {
        let mut registry = NodeRegistry::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);

        registry.add_or_update_node(addr, "test-node".to_string(), None, Some(addr.port()));
        registry.update_node_status(addr, NodeStatus::ConnectionFailed);

        let node = registry.get_node(addr).unwrap();
        assert!(node.should_reconnect());
    }

    #[test]
    fn test_remove_timed_out_nodes_removes_entries() {
        let mut registry = NodeRegistry::new();
        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7001);

        registry.add_or_update_node(addr1, "test-node-1".to_string(), None, Some(addr1.port()));
        registry.add_or_update_node(addr2, "test-node-2".to_string(), None, Some(addr2.port()));

        // Manually set one node to timeout
        if let Some(node) = registry.get_node_mut(addr1) {
            node.last_heartbeat = SystemTime::now() - Duration::from_secs(60);
        }

        let timed_out = registry.remove_timed_out_nodes();
        assert_eq!(timed_out, vec![addr1]);
        assert_eq!(registry.len(), 1); // Should be 1 since addr1 was removed
        assert!(registry.get_node(addr1).is_none()); // Node should no longer exist in registry
        assert!(registry.get_node(addr2).is_some()); // Node 2 should still be there
    }
}
