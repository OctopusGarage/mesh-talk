use crate::domain::node_registry::{NodeRegistry, NodeStatus};
use crate::network::tcp::ConnectionManager;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

/// Reconnection event types
#[derive(Debug, Clone)]
pub enum ReconnectionEvent {
    /// Node connection attempt started
    ConnectionAttemptStarted(SocketAddr),
    /// Node connection attempt succeeded
    ConnectionAttemptSucceeded(SocketAddr),
    /// Node connection attempt failed
    ConnectionAttemptFailed(SocketAddr, String),
    /// Node reconnection process completed
    ReconnectionCompleted(SocketAddr),
}

/// Reconnection error types
#[derive(Debug, Clone, PartialEq)]
pub enum ReconnectionError {
    /// Failed to reconnect to node
    ReconnectionFailed(String),
    /// Node not found in registry
    NodeNotFound,
    /// Internal error
    InternalError(String),
}

impl fmt::Display for ReconnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReconnectionError::ReconnectionFailed(msg) => write!(f, "Reconnection failed: {}", msg),
            ReconnectionError::NodeNotFound => write!(f, "Node not found"),
            ReconnectionError::InternalError(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for ReconnectionError {}

/// Reconnection result type
pub type ReconnectionResult<T> = Result<T, ReconnectionError>;

/// Reconnection manager for handling automatic node reconnections
pub struct ReconnectionManager {
    /// Connection manager for establishing connections
    connection_manager: Arc<ConnectionManager>,
    /// Node registry for tracking node status
    node_registry: Arc<Mutex<NodeRegistry>>,
    /// Channels for sending reconnection events
    event_channels: Arc<Mutex<HashMap<String, mpsc::Sender<ReconnectionEvent>>>>,
    /// Flag to indicate if reconnection is enabled
    enabled: Arc<Mutex<bool>>,
}

fn monitoring_interval() -> Duration {
    env::var("MESH_TALK_RECONNECT_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(5))
}

impl ReconnectionManager {
    /// Create a new reconnection manager
    pub fn new(
        connection_manager: Arc<ConnectionManager>,
        node_registry: Arc<Mutex<NodeRegistry>>,
    ) -> Self {
        Self {
            connection_manager,
            node_registry,
            event_channels: Arc::new(Mutex::new(HashMap::new())),
            enabled: Arc::new(Mutex::new(true)),
        }
    }

    /// Enable or disable reconnection
    pub fn set_enabled(&self, enabled: bool) {
        let mut enabled_flag = self.enabled.lock().unwrap();
        *enabled_flag = enabled;
    }

    /// Check if reconnection is enabled
    pub fn is_enabled(&self) -> bool {
        let enabled_flag = self.enabled.lock().unwrap();
        *enabled_flag
    }

    /// Add an event channel for receiving reconnection events
    pub fn add_event_channel(&self, name: String, sender: mpsc::Sender<ReconnectionEvent>) {
        let mut channels = self.event_channels.lock().unwrap();
        channels.insert(name, sender);
    }

    /// Remove an event channel
    pub fn remove_event_channel(&self, name: &str) {
        let mut channels = self.event_channels.lock().unwrap();
        channels.remove(name);
    }

    /// Send a reconnection event to all channels
    fn send_event(&self, event: ReconnectionEvent) {
        let channels = self.event_channels.lock().unwrap();
        for sender in channels.values() {
            let sender = sender.clone();
            let event = event.clone();
            tokio::spawn(async move {
                let _ = sender.send(event).await;
            });
        }
    }

    /// Start the reconnection process for a specific node
    pub async fn reconnect_node<F>(
        &self,
        addr: SocketAddr,
        message_handler: F,
    ) -> ReconnectionResult<()>
    where
        F: Fn(String) + Send + 'static + Clone,
    {
        // Check if reconnection is enabled
        if !self.is_enabled() {
            return Ok(());
        }

        // Send connection attempt started event
        self.send_event(ReconnectionEvent::ConnectionAttemptStarted(addr));

        // Attempt to connect with the connection manager
        match self
            .connection_manager
            .connect_with_retry(addr, message_handler, "unknown".to_string())
            .await
        {
            Ok(()) => {
                // Send connection attempt succeeded event
                self.send_event(ReconnectionEvent::ConnectionAttemptSucceeded(addr));

                // Update node registry status
                {
                    let mut registry = self.node_registry.lock().unwrap();
                    if let Some(node) = registry.get_node_mut(addr) {
                        node.mark_connected();
                    }
                }

                // Send reconnection completed event
                self.send_event(ReconnectionEvent::ReconnectionCompleted(addr));

                Ok(())
            }
            Err(e) => {
                // Send connection attempt failed event
                self.send_event(ReconnectionEvent::ConnectionAttemptFailed(
                    addr,
                    e.to_string(),
                ));

                // Update node registry status
                {
                    let mut registry = self.node_registry.lock().unwrap();
                    if let Some(node) = registry.get_node_mut(addr) {
                        node.mark_connection_failed();
                    }
                }

                Err(ReconnectionError::ReconnectionFailed(e.to_string()))
            }
        }
    }

    /// Start continuous reconnection monitoring
    pub async fn start_monitoring<F>(&self, message_handler: F)
    where
        F: Fn(String) + Send + 'static + Clone,
    {
        // Check if reconnection is enabled
        if !self.is_enabled() {
            return;
        }

        let mut interval = interval(monitoring_interval());
        let registry = Arc::clone(&self.node_registry);
        let connection_manager = Arc::clone(&self.connection_manager);
        let event_channels = Arc::clone(&self.event_channels);
        let enabled = Arc::clone(&self.enabled);

        loop {
            interval.tick().await;

            // Check if reconnection is still enabled
            let should_continue = {
                let enabled_flag = enabled.lock().unwrap();
                *enabled_flag
            };

            if !should_continue {
                break;
            }

            // Get nodes that should be reconnected
            let node_addresses = {
                let registry = registry.lock().unwrap();
                registry
                    .get_nodes_to_reconnect()
                    .iter()
                    .map(|node| node.addr)
                    .collect::<Vec<SocketAddr>>()
            };

            for addr in node_addresses {
                let message_handler_clone = message_handler.clone();
                let connection_manager_clone = Arc::clone(&connection_manager);
                let event_channels_clone = Arc::clone(&event_channels);
                let registry_clone = Arc::clone(&registry);

                tokio::spawn(async move {
                    // Send connection attempt started event
                    {
                        let senders: Vec<mpsc::Sender<ReconnectionEvent>> = {
                            let channels = event_channels_clone.lock().unwrap();
                            channels.values().cloned().collect()
                        };

                        for sender in senders {
                            let _ = sender
                                .send(ReconnectionEvent::ConnectionAttemptStarted(addr))
                                .await;
                        }
                    }

                    // Attempt to connect
                    match connection_manager_clone
                        .connect_with_retry(addr, message_handler_clone, "unknown".to_string())
                        .await
                    {
                        Ok(()) => {
                            // Send connection attempt succeeded event
                            {
                                let senders: Vec<mpsc::Sender<ReconnectionEvent>> = {
                                    let channels = event_channels_clone.lock().unwrap();
                                    channels.values().cloned().collect()
                                };

                                for sender in senders {
                                    let _ = sender
                                        .send(ReconnectionEvent::ConnectionAttemptSucceeded(addr))
                                        .await;
                                }
                            }

                            // Update node registry status
                            {
                                let mut registry = registry_clone.lock().unwrap();
                                if let Some(node) = registry.get_node_mut(addr) {
                                    node.mark_connected();
                                }
                            }

                            // Send reconnection completed event
                            {
                                let senders: Vec<mpsc::Sender<ReconnectionEvent>> = {
                                    let channels = event_channels_clone.lock().unwrap();
                                    channels.values().cloned().collect()
                                };

                                for sender in senders {
                                    let _ = sender
                                        .send(ReconnectionEvent::ReconnectionCompleted(addr))
                                        .await;
                                }
                            }
                        }
                        Err(e) => {
                            // Send connection attempt failed event
                            {
                                let senders: Vec<mpsc::Sender<ReconnectionEvent>> = {
                                    let channels = event_channels_clone.lock().unwrap();
                                    channels.values().cloned().collect()
                                };

                                for sender in senders {
                                    let _ = sender
                                        .send(ReconnectionEvent::ConnectionAttemptFailed(
                                            addr,
                                            e.to_string(),
                                        ))
                                        .await;
                                }
                            }

                            // Update node registry status
                            {
                                let mut registry = registry_clone.lock().unwrap();
                                if let Some(node) = registry.get_node_mut(addr) {
                                    node.mark_connection_failed();
                                }
                            }
                        }
                    }
                });
            }
        }
    }

    /// Force reconnection to all nodes regardless of their current status
    pub async fn force_reconnect_all<F>(&self, message_handler: F) -> ReconnectionResult<usize>
    where
        F: Fn(String) + Send + 'static + Clone,
    {
        // Check if reconnection is enabled
        if !self.is_enabled() {
            return Ok(0);
        }

        // Get all node addresses from the registry
        let node_addresses = {
            let registry = self.node_registry.lock().unwrap();
            registry
                .get_all_nodes()
                .iter()
                .map(|node| node.addr)
                .collect::<Vec<SocketAddr>>()
        };

        let mut reconnection_count = 0;

        // Attempt to reconnect to each node
        for addr in node_addresses {
            match self.reconnect_node(addr, message_handler.clone()).await {
                Ok(()) => {
                    reconnection_count += 1;
                }
                Err(e) => {
                    eprintln!("Failed to reconnect to {}: {}", addr, e);
                }
            }
        }

        Ok(reconnection_count)
    }

    /// Get the current reconnection status for a node
    pub fn get_node_reconnection_status(&self, addr: SocketAddr) -> Option<NodeStatus> {
        let registry = self.node_registry.lock().unwrap();
        registry.get_node(addr).map(|node| node.status.clone())
    }

    /// Get statistics about the reconnection process
    pub fn get_statistics(&self) -> HashMap<String, usize> {
        let registry = self.node_registry.lock().unwrap();
        let all_nodes = registry.get_all_nodes();

        let mut stats = HashMap::new();
        stats.insert("total_nodes".to_string(), all_nodes.len());

        let online_nodes = all_nodes
            .iter()
            .filter(|node| node.status == NodeStatus::Online)
            .count();
        stats.insert("online_nodes".to_string(), online_nodes);

        let offline_nodes = all_nodes
            .iter()
            .filter(|node| node.status == NodeStatus::Offline)
            .count();
        stats.insert("offline_nodes".to_string(), offline_nodes);

        let connecting_nodes = all_nodes
            .iter()
            .filter(|node| node.status == NodeStatus::Connecting)
            .count();
        stats.insert("connecting_nodes".to_string(), connecting_nodes);

        let failed_nodes = all_nodes
            .iter()
            .filter(|node| node.status == NodeStatus::ConnectionFailed)
            .count();
        stats.insert("failed_nodes".to_string(), failed_nodes);

        stats
    }
}

impl Drop for ReconnectionManager {
    fn drop(&mut self) {
        // Disable reconnection when the manager is dropped
        let mut enabled_flag = self.enabled.lock().unwrap();
        *enabled_flag = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn test_reconnection_manager_creation() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager =
            Arc::new(ConnectionManager::new(peers, Arc::clone(&node_registry)));
        let reconnection_manager = ReconnectionManager::new(connection_manager, node_registry);

        assert!(reconnection_manager.is_enabled());
    }

    #[test]
    fn test_reconnection_manager_enable_disable() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager =
            Arc::new(ConnectionManager::new(peers, Arc::clone(&node_registry)));
        let reconnection_manager = ReconnectionManager::new(connection_manager, node_registry);

        assert!(reconnection_manager.is_enabled());

        reconnection_manager.set_enabled(false);
        assert!(!reconnection_manager.is_enabled());

        reconnection_manager.set_enabled(true);
        assert!(reconnection_manager.is_enabled());
    }

    #[tokio::test]
    async fn test_add_remove_event_channel() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager =
            Arc::new(ConnectionManager::new(peers, Arc::clone(&node_registry)));
        let reconnection_manager = ReconnectionManager::new(connection_manager, node_registry);

        let (tx, _rx) = mpsc::channel(32);
        reconnection_manager.add_event_channel("test".to_string(), tx);

        reconnection_manager.remove_event_channel("test");
    }

    #[tokio::test]
    async fn test_get_statistics() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager =
            Arc::new(ConnectionManager::new(peers, Arc::clone(&node_registry)));
        let reconnection_manager =
            ReconnectionManager::new(connection_manager, Arc::clone(&node_registry));

        let stats = reconnection_manager.get_statistics();
        assert_eq!(stats.get("total_nodes"), Some(&0));
        assert_eq!(stats.get("online_nodes"), Some(&0));
        assert_eq!(stats.get("offline_nodes"), Some(&0));
        assert_eq!(stats.get("connecting_nodes"), Some(&0));
        assert_eq!(stats.get("failed_nodes"), Some(&0));
    }

    #[tokio::test]
    async fn test_send_event() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager =
            Arc::new(ConnectionManager::new(peers, Arc::clone(&node_registry)));
        let reconnection_manager =
            ReconnectionManager::new(connection_manager, Arc::clone(&node_registry));

        let (tx, mut rx) = mpsc::channel(32);
        reconnection_manager.add_event_channel("test".to_string(), tx);

        // Send an event
        reconnection_manager.send_event(ReconnectionEvent::ConnectionAttemptStarted(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000),
        ));

        // Check that the event was received
        let event = rx.recv().await.unwrap();
        match event {
            ReconnectionEvent::ConnectionAttemptStarted(addr) => {
                assert_eq!(
                    addr,
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000)
                );
            }
            _ => panic!("Unexpected event type"),
        }
    }

    #[tokio::test]
    async fn test_get_node_reconnection_status() {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager =
            Arc::new(ConnectionManager::new(peers, Arc::clone(&node_registry)));
        let reconnection_manager =
            ReconnectionManager::new(connection_manager, Arc::clone(&node_registry));

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7000);
        let status = reconnection_manager.get_node_reconnection_status(addr);
        assert!(status.is_none());
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(prev) = &self.previous {
                std::env::set_var(self.key, prev);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn test_start_monitoring_runs_until_disabled() {
        let _interval_guard = EnvVarGuard::set("MESH_TALK_RECONNECT_INTERVAL_MS", "10");
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let node_registry = Arc::new(Mutex::new(NodeRegistry::new()));
        let connection_manager =
            Arc::new(ConnectionManager::new(peers, Arc::clone(&node_registry)));
        let reconnection_manager = Arc::new(ReconnectionManager::new(
            Arc::clone(&connection_manager),
            Arc::clone(&node_registry),
        ));

        let completed = Arc::new(AtomicBool::new(false));
        let completed_clone = Arc::clone(&completed);
        let manager_clone = Arc::clone(&reconnection_manager);

        let handle = tokio::spawn(async move {
            manager_clone.start_monitoring(|_| {}).await;
            completed_clone.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !completed.load(Ordering::SeqCst),
            "monitoring should continue while enabled"
        );

        reconnection_manager.set_enabled(false);
        tokio::time::sleep(Duration::from_millis(20)).await;

        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("monitoring task should exit cleanly")
            .expect("monitoring task panicked");

        assert!(
            completed.load(Ordering::SeqCst),
            "monitoring task should mark completion"
        );
    }
}
