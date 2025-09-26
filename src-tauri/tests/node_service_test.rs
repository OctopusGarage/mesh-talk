// Integration tests for NodeService
use mesh_talk::services::node_service::NodeService;

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk::launch_network;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokioMutex;

    #[tokio::test]
    async fn test_node_service_creation() {
        // Create a NodeService instance
        let node_service = NodeService::new("test_node".to_string(), 7001);

        // Verify that the NodeService was created correctly
        assert_eq!(node_service.get_name(), "test_node");
        assert_eq!(node_service.get_port(), 7001);
    }

    #[tokio::test]
    async fn test_dynamic_port_allocation() {
        // Create a NodeService with port 0 to represent dynamic allocation
        let node_service = NodeService::new("dynamic_node".to_string(), 0);

        // Verify that the NodeService was created correctly
        assert_eq!(node_service.get_name(), "dynamic_node");
        assert_eq!(node_service.get_port(), 0);
    }

    #[tokio::test]
    async fn test_launch_network_handles_port_conflict() {
        let occupied = TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();

        let node_service = Arc::new(TokioMutex::new(NodeService::new(
            "conflict-node".to_string(),
            occupied_port,
        )));

        let startup = launch_network(Arc::clone(&node_service))
            .await
            .expect("launch network");
        let actual_port = startup.port;
        let runtime = startup.runtime;

        assert_ne!(actual_port, occupied_port);

        let service = node_service.lock().await;
        assert_eq!(service.get_port(), actual_port);

        drop(service);
        runtime.shutdown();
        drop(occupied);
    }
}
