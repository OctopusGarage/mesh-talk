//! Manual integration assertion for an already running, reachable tailnet peer.

use super::*;
use mesh_talk_core::discovery::roster::RouteSource;
use mesh_talk_core::node::{NodeRuntime, RuntimeConfig, RuntimeEvents};
use std::sync::Arc;

/// Route-source verification distinguishes the automatic unicast query from
/// coincidental LAN multicast when the two Macs are physically nearby.
#[tokio::test]
#[ignore = "requires a live remote Mesh Talk node on the same tailnet"]
async fn live_tailnet_inventory_automatically_discovers_remote_node() {
    let remote_ip: IpAddr = std::env::var("MESH_TALK_E2E_REMOTE_IP")
        .expect("set MESH_TALK_E2E_REMOTE_IP")
        .parse()
        .expect("remote Tailscale IP");
    let port: u16 = std::env::var("MESH_TALK_E2E_DISCOVERY_PORT")
        .expect("set MESH_TALK_E2E_DISCOVERY_PORT")
        .parse()
        .expect("remote discovery port");
    let source = Arc::new(TailscaleCandidates {
        discovery_port: port,
    });
    assert!(
        source
            .snapshot()
            .await
            .contains(&SocketAddr::new(remote_ip, port)),
        "running Tailscale inventory must contain the remote node"
    );

    let root = tempfile::tempdir().expect("isolated profile");
    let runtime = NodeRuntime::start_configured(
        RuntimeConfig {
            base_dir: root.path(),
            account_id: "tailnet-e2e",
            display_name: "Automatic Tailnet Probe",
            password: "test-only",
            discovery_port: port,
            post_office_enabled: false,
            candidate_source: Some(source.clone()),
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
    .expect("start local runtime");
    let discovery = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if runtime.peers().iter().any(|peer| {
                peer.routes.iter().any(|route| {
                    route.addr.ip() == remote_ip && route.source == RouteSource::RoutedQuery
                })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if discovery.is_err() {
        eprintln!(
            "automatic discovery timed out; peers: {:?}",
            runtime.peers()
        );
        eprintln!("candidate snapshot: {:?}", source.snapshot().await);
    }
    discovery.expect("automatic Tailscale candidate produced a verified routed peer");
    runtime.stop().await;
}
