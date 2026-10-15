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
fn parse_status(bytes: &[u8], port: u16) -> Option<Vec<SocketAddr>> {
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return None;
    };
    if status.get("BackendState").and_then(|v| v.as_str()) != Some("Running") {
        return None;
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
                        return Some(endpoints);
                    }
                }
            }
        }
    }
    Some(endpoints)
}

fn cli_locations() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("tailscale")];
    if cfg!(target_os = "macos") {
        paths.push(PathBuf::from(
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        ));
    }
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
    local_inventory_from(cli_locations(), port).await
}

async fn local_inventory_from(executables: Vec<PathBuf>, port: u16) -> Vec<SocketAddr> {
    for executable in executables {
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
        if let Ok(Some(bytes)) = result
            && let Some(endpoints) = parse_status(&bytes, port)
        {
            return endpoints;
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

    #[cfg(unix)]
    fn fake_cli(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

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
        let endpoints = parse_status(status, 47474).unwrap();
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.contains(&"100.101.1.2:47474".parse().unwrap()));
        assert!(endpoints.contains(&"[fd7a:115c:a1e0::2]:47474".parse().unwrap()));
        assert!(parse_status(br#"{"BackendState":"NeedsLogin","Peer":{}}"#, 47474).is_none());
        assert!(parse_status(b"not json", 47474).is_none());
        assert_eq!(
            parse_status(br#"{"BackendState":"Running","Peer":{}}"#, 47474),
            Some(Vec::new())
        );
    }

    #[test]
    fn status_bounds_candidate_count_without_including_our_own_ip() {
        let peers: serde_json::Map<String, serde_json::Value> = (0..260)
            .map(|index| {
                (
                    format!("nodekey:{index}"),
                    serde_json::json!({
                        "Online": true,
                        "TailscaleIPs": [format!("100.101.{}.{}", index / 256, index % 256)]
                    }),
                )
            })
            .collect();
        let status = serde_json::json!({
            "BackendState": "Running",
            "Self": { "TailscaleIPs": ["100.101.0.0"] },
            "Peer": peers,
        });
        let endpoints = parse_status(&serde_json::to_vec(&status).unwrap(), 47474).unwrap();
        assert_eq!(endpoints.len(), 256);
        assert!(
            !endpoints
                .iter()
                .any(|endpoint| endpoint.ip().to_string() == "100.101.0.0")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inventory_uses_an_available_cli_without_changing_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let failed = fake_cli(dir.path(), "failed-client", "exit 3");
        let ready = fake_cli(
            dir.path(),
            "running-client",
            "printf '%s' '{\"BackendState\":\"Running\",\"Peer\":{\"peer\":{\"Online\":true,\"TailscaleIPs\":[\"100.101.2.3\"]}}}'",
        );
        let candidates = local_inventory_from(
            vec![dir.path().join("missing-client"), failed, ready],
            47474,
        )
        .await;
        assert_eq!(candidates, vec!["100.101.2.3:47474".parse().unwrap()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inventory_tries_another_cli_when_the_first_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let logged_out = fake_cli(
            dir.path(),
            "logged-out-client",
            "printf '%s' '{\"BackendState\":\"NeedsLogin\",\"Peer\":{}}'",
        );
        let running = fake_cli(
            dir.path(),
            "running-client",
            "printf '%s' '{\"BackendState\":\"Running\",\"Peer\":{\"peer\":{\"Online\":true,\"TailscaleIPs\":[\"100.101.2.3\"]}}}'",
        );

        let candidates = local_inventory_from(vec![logged_out, running], 47474).await;
        assert_eq!(candidates, vec!["100.101.2.3:47474".parse().unwrap()]);
    }
}

#[cfg(test)]
#[path = "../tests/support/tailscale_live.rs"]
mod live_tests;
