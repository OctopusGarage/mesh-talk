//! Optional address inventory from an already running local Tailscale client.

use mesh_talk_core::discovery::candidates::CandidateSource;
use std::collections::HashSet;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(crate) struct TailscaleCandidates {
    pub discovery_port: u16,
}

fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            octets[0] == 100 && (64..128).contains(&octets[1])
        }
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// The CLI JSON is explicitly subject to change, so every field is optional
/// except the currently documented running state and peer IP arrays.
fn parse_status(bytes: &[u8], port: u16) -> Vec<SocketAddr> {
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    if status.get("BackendState").and_then(|v| v.as_str()) != Some("Running") {
        return Vec::new();
    }
    let self_ips: HashSet<IpAddr> = status
        .get("Self")
        .and_then(|v| v.get("TailscaleIPs"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().and_then(|ip| ip.parse().ok()))
        .collect();
    let mut seen = HashSet::new();
    let mut endpoints = Vec::new();
    if let Some(peers) = status.get("Peer").and_then(|v| v.as_object()) {
        for peer in peers.values() {
            if peer.get("Online").and_then(|v| v.as_bool()) != Some(true) {
                continue;
            }
            let Some(ips) = peer.get("TailscaleIPs").and_then(|v| v.as_array()) else {
                continue;
            };
            for ip in ips {
                if let Some(ip) = ip.as_str().and_then(|value| value.parse::<IpAddr>().ok())
                    && is_tailnet_ip(ip)
                    && !self_ips.contains(&ip)
                    && seen.insert(ip)
                {
                    endpoints.push(SocketAddr::new(ip, port));
                    if endpoints.len() == 256 {
                        return endpoints;
                    }
                }
            }
        }
    }
    endpoints
}

fn cli_locations() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("tailscale")];
    #[cfg(target_os = "macos")]
    paths.push(PathBuf::from(
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ));
    #[cfg(target_os = "windows")]
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        paths.push(
            PathBuf::from(program_files)
                .join("Tailscale")
                .join("tailscale.exe"),
        );
    }
    paths
}

async fn local_inventory(port: u16) -> Vec<SocketAddr> {
    for executable in cli_locations() {
        let mut command = tokio::process::Command::new(executable);
        command
            .args(["status", "--json"])
            .env("TAILSCALE_BE_CLI", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let Ok(mut child) = command.spawn() else {
            continue;
        };
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let mut stdout = child.stdout.take()?.take(1_048_577);
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.ok()?;
            if bytes.len() > 1_048_576 || !child.wait().await.ok()?.success() {
                return None;
            }
            Some(bytes)
        })
        .await;
        if let Ok(Some(bytes)) = result {
            return parse_status(&bytes, port);
        }
    }
    Vec::new()
}

impl CandidateSource for TailscaleCandidates {
    fn snapshot(&self) -> Pin<Box<dyn Future<Output = Vec<SocketAddr>> + Send + '_>> {
        Box::pin(local_inventory(self.discovery_port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_only_yields_visible_online_peer_ips_when_client_is_running() {
        let adapter = TailscaleCandidates {
            discovery_port: 47474,
        };
        drop(adapter.snapshot());
        let status = br#"{
            "BackendState":"Running",
            "Self":{"TailscaleIPs":["100.101.1.1"]},
            "Peer":{
                "nodekey:a":{"Online":true,"TailscaleIPs":["100.101.1.2","fd7a:115c:a1e0::2"]},
                "nodekey:b":{"Online":false,"TailscaleIPs":["100.101.1.3"]},
                "nodekey:c":{"TailscaleIPs":["100.101.1.2","127.0.0.1"]}
            }
        }"#;
        let endpoints = parse_status(status, 47474);
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.contains(&"100.101.1.2:47474".parse().unwrap()));
        assert!(endpoints.contains(&"[fd7a:115c:a1e0::2]:47474".parse().unwrap()));
        assert!(parse_status(br#"{"BackendState":"NeedsLogin","Peer":{}}"#, 47474).is_empty());
        assert!(parse_status(b"not json", 47474).is_empty());
    }
}
