use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct NetworkRuntime {
    tcp_handle: JoinHandle<()>,
    udp_discovery_handle: JoinHandle<()>,
    udp_broadcast_handle: Option<JoinHandle<()>>,
    reconnection_handle: JoinHandle<()>,
}

impl NetworkRuntime {
    pub fn new(
        tcp_handle: JoinHandle<()>,
        udp_discovery_handle: JoinHandle<()>,
        udp_broadcast_handle: Option<JoinHandle<()>>,
        reconnection_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            tcp_handle,
            udp_discovery_handle,
            udp_broadcast_handle,
            reconnection_handle,
        }
    }

    pub fn shutdown(self) {
        self.tcp_handle.abort();
        self.udp_discovery_handle.abort();
        if let Some(handle) = self.udp_broadcast_handle {
            handle.abort();
        }
        self.reconnection_handle.abort();
    }
}
