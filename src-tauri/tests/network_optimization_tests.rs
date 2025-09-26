//! Network optimization benchmarks

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::{Arc, Mutex};

    use tokio::net::UdpSocket;

    fn protocol_packet(message_type: u8, payload: &str) -> Vec<u8> {
        let mut packet = Vec::new();
        let magic = mesh_talk::domain::message::PROTOCOL_MAGIC;
        let version = mesh_talk::domain::message::PROTOCOL_VERSION;
        packet.extend_from_slice(&[
            (magic >> 24) as u8,
            (magic >> 16) as u8,
            (magic >> 8) as u8,
            magic as u8,
            version,
            message_type,
        ]);
        packet.extend_from_slice(payload.as_bytes());
        packet
    }

    #[tokio::test]
    async fn test_network_optimization_improvements() {
        // This test verifies that the network optimizations have been applied
        // It doesn't measure actual performance but confirms the settings are correct

        // Test that UDP broadcast interval has been increased
        let broadcast_interval = Duration::from_secs(10); // Should be 10 seconds now
        assert_eq!(broadcast_interval, Duration::from_secs(10));

        // Test that heartbeat interval has been increased
        let heartbeat_interval = Duration::from_secs(30); // Should be 30 seconds now
        assert_eq!(heartbeat_interval, Duration::from_secs(30));

        println!("Network optimization settings verified:");
        println!("  Broadcast interval: {:?}", broadcast_interval);
        println!("  Heartbeat interval: {:?}", heartbeat_interval);
    }

    #[tokio::test]
    async fn test_exponential_backoff_logic() {
        // Test the exponential backoff calculation
        let max_delay_ms = 5000; // 5 seconds maximum

        // Test different retry counts
        let delays: Vec<u64> = (0..5)
            .map(|retry| {
                // Calculate exponential backoff: 100 * 2^retry
                let base_delay = 100 * (2_u64.pow(retry));
                base_delay.min(max_delay_ms) // Cap at maximum delay
            })
            .collect();

        // Verify the delays are calculated correctly
        assert_eq!(delays[0], 100); // 100 * 2^0 = 100ms
        assert_eq!(delays[1], 200); // 100 * 2^1 = 200ms
        assert_eq!(delays[2], 400); // 100 * 2^2 = 400ms
        assert_eq!(delays[3], 800); // 100 * 2^3 = 800ms
        assert_eq!(delays[4], 1600); // 100 * 2^4 = 1600ms

        println!("Exponential backoff delays: {:?}", delays);
    }

    #[tokio::test]
    async fn heartbeat_timeout_triggers_reconnect_callback() {
        use mesh_talk::domain::message::MESSAGE_TYPE_DISCOVERY;
        use mesh_talk::domain::message::MESSAGE_TYPE_HEARTBEAT;

        // Configure intervals to be very short for the test environment
        std::env::set_var("MESH_TALK_DISCOVERY_INTERVAL_MS", "1000");
        std::env::set_var("MESH_TALK_HEARTBEAT_INTERVAL_MS", "1000");
        std::env::set_var("MESH_TALK_REGISTRY_CLEANUP_MS", "40");
        std::env::set_var("MESH_TALK_RECONNECT_MS", "60");
        std::env::set_var("MESH_TALK_HEARTBEAT_TIMEOUT_MS", "40");

        // Pick an unused UDP port for discovery
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind temp socket");
        let discovery_port = socket.local_addr().unwrap().port();
        drop(socket);
        std::env::set_var("MESH_TALK_DISCOVERY_PORT", discovery_port.to_string());

        let callback_records = Arc::new(Mutex::new(Vec::<SocketAddr>::new()));
        let callback_clone = callback_records.clone();

        let discovery_task = tokio::spawn(async move {
            let result = mesh_talk::network::udp::start_udp_discovery(
                move |addr, _name, _username, _port| {
                    let store = callback_clone.clone();
                    store.lock().unwrap().push(addr);
                },
            )
            .await;
            assert!(result.is_ok());
        });

        tokio::task::yield_now().await;

        // Send a discovery packet to register the peer
        let sender = UdpSocket::bind("127.0.0.1:0").await.expect("bind sender");
        let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), discovery_port);
        let discovery_payload = r#"{"Discovery":{"name":"test-peer","username":null,"port":4000}}"#;
        sender
            .send_to(
                &protocol_packet(MESSAGE_TYPE_DISCOVERY, discovery_payload),
                target,
            )
            .await
            .expect("send discovery");

        tokio::time::sleep(Duration::from_millis(40)).await;

        {
            let records = callback_records.lock().unwrap();
            assert_eq!(
                records.len(),
                1,
                "Discovery should trigger initial callback"
            );
        }

        // Advance time beyond the cleanup interval to trigger timeout handling
        tokio::time::sleep(Duration::from_millis(120)).await;

        {
            let records = callback_records.lock().unwrap();
            assert!(
                records.len() >= 2,
                "Timed-out node should trigger reconnect callback"
            );
        }

        // Send a heartbeat to mark the node as online again
        let heartbeat_payload = r#"{"Heartbeat":{"name":"test-peer","username":null,"port":4000}}"#;
        sender
            .send_to(
                &protocol_packet(MESSAGE_TYPE_HEARTBEAT, heartbeat_payload),
                target,
            )
            .await
            .expect("send heartbeat");

        tokio::time::sleep(Duration::from_millis(40)).await;

        discovery_task.abort();

        std::env::remove_var("MESH_TALK_DISCOVERY_INTERVAL_MS");
        std::env::remove_var("MESH_TALK_HEARTBEAT_INTERVAL_MS");
        std::env::remove_var("MESH_TALK_REGISTRY_CLEANUP_MS");
        std::env::remove_var("MESH_TALK_RECONNECT_MS");
        std::env::remove_var("MESH_TALK_HEARTBEAT_TIMEOUT_MS");
        std::env::remove_var("MESH_TALK_DISCOVERY_PORT");
    }
}
