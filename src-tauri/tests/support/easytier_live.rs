//! Explicit physical acceptance using the production desktop source factory.

use super::*;
use mesh_talk_core::discovery::roster::RouteSource;
use mesh_talk_core::node::{NodeRuntime, RuntimeConfig, RuntimeEvents};
use std::net::IpAddr;

// Keep only an address actually returned by production inventory. This prevents
// a simultaneous Tailscale route from satisfying the data-transfer assertion.
struct EasyTierPath {
    upstream: Arc<dyn CandidateSource>,
    remote_ip: IpAddr,
}

impl CandidateSource for EasyTierPath {
    fn snapshot(&self) -> Pin<Box<dyn Future<Output = Vec<SocketAddr>> + Send + '_>> {
        Box::pin(async {
            self.upstream
                .snapshot()
                .await
                .into_iter()
                .filter(|endpoint| endpoint.ip() == self.remote_ip)
                .collect()
        })
    }
}

#[tokio::test]
#[ignore = "requires connected EasyTier clients and a remote Mesh Talk node"]
async fn live_easytier_inventory_automatically_discovers_remote_node() {
    let remote_ip: IpAddr = std::env::var("MESH_TALK_E2E_REMOTE_IP")
        .unwrap()
        .parse()
        .unwrap();
    let remote_port: u16 = std::env::var("MESH_TALK_E2E_DISCOVERY_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let remote_id = std::env::var("MESH_TALK_E2E_REMOTE_ID").unwrap();
    let token = std::env::var("MESH_TALK_E2E_AUTOMATIC_TOKEN").unwrap();
    let source = desktop_candidates(remote_port);
    assert!(
        source
            .snapshot()
            .await
            .contains(&SocketAddr::new(remote_ip, remote_port))
    );
    let reserved = mesh_talk_core::transport::net::discovery_socket(0).unwrap();
    let local_port = reserved.local_addr().unwrap().port();
    assert_ne!(
        local_port, remote_port,
        "separate discovery ports exclude LAN coincidence"
    );
    drop(reserved);
    let root = tempfile::tempdir().unwrap();
    let runtime = NodeRuntime::start_configured(
        RuntimeConfig {
            base_dir: root.path(),
            account_id: "easytier-e2e",
            display_name: "Automatic EasyTier Probe",
            password: "test-only",
            discovery_port: local_port,
            post_office_enabled: false,
            candidate_source: Some(Arc::new(EasyTierPath {
                upstream: source,
                remote_ip,
            })),
        },
        RuntimeEvents {
            on_dm: Box::new(|_| {}),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|_| {}),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|_| {}),
        },
    )
    .await
    .unwrap();
    let discovery = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if runtime.peers().iter().any(|peer| {
                peer.public.user_id() == remote_id
                    && peer.routes.iter().any(|route| {
                        route.addr.ip() == remote_ip && route.source == RouteSource::RoutedQuery
                    })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let sent = if discovery.is_ok() {
        Some(runtime.send_dm(&remote_id, token.as_bytes()).await)
    } else {
        None
    };
    runtime.stop().await;
    discovery.expect("automatic production inventory must discover the expected EasyTier identity");
    sent.unwrap()
        .expect("send encrypted DM through the automatically discovered EasyTier route");
}
