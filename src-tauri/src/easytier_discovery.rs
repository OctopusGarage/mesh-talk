//! Optional candidates from an independently managed local EasyTier client.

use mesh_talk_core::discovery::candidates::CandidateSource;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(crate) struct EasyTierCandidates {
    pub discovery_port: u16,
}

impl CandidateSource for EasyTierCandidates {
    fn snapshot(&self) -> Pin<Box<dyn Future<Output = Vec<SocketAddr>> + Send + '_>> {
        Box::pin(async move {
            let rpc = std::env::var("MESH_TALK_EASYTIER_RPC")
                .unwrap_or_else(|_| "127.0.0.1:15888".into());
            // Inventory is local-client integration, not a remote management API.
            if !rpc
                .parse::<SocketAddr>()
                .is_ok_and(|addr| addr.ip().is_loopback() && addr.port() != 0)
            {
                return Vec::new();
            }
            local_inventory_from(
                cli_locations()
                    .into_iter()
                    .map(crate::background_command::command)
                    .collect(),
                &rpc,
                self.discovery_port,
            )
            .await
        })
    }
}

fn cli_locations() -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("MESH_TALK_EASYTIER_CLI") {
        return vec![PathBuf::from(path)];
    }
    let mut paths = vec![PathBuf::from("easytier-cli")];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        paths.push(home.join(".local/bin/easytier-cli"));
        paths.push(home.join(".cargo/bin/easytier-cli"));
    }
    #[cfg(target_os = "macos")]
    paths.extend(
        [
            "/opt/homebrew/bin/easytier-cli",
            "/usr/local/bin/easytier-cli",
            "/Library/Application Support/MeshTalk-EasyTier/bin/easytier-cli",
        ]
        .map(PathBuf::from),
    );
    #[cfg(target_os = "linux")]
    paths.extend(["/usr/local/bin/easytier-cli", "/usr/bin/easytier-cli"].map(PathBuf::from));
    #[cfg(target_os = "windows")]
    {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            paths.push(PathBuf::from(program_files).join("EasyTier/easytier-cli.exe"));
        }
        if let Ok(executable) = std::env::current_exe()
            && let Some(directory) = executable.parent()
        {
            paths.push(directory.join("easytier-cli.exe"));
        }
    }
    paths
}

async fn local_inventory_from(
    commands: Vec<tokio::process::Command>,
    rpc: &str,
    port: u16,
) -> Vec<SocketAddr> {
    // Bound the whole search, including executable fallbacks, below the host's
    // deadline. Dropping this future also drops/kills any in-flight CLI child.
    tokio::time::timeout(Duration::from_secs(2), async {
        for command in commands {
            if let Some(bytes) = read_inventory(command, rpc).await
                && let Some(candidates) = parse_routes(&bytes, port)
            {
                return candidates;
            }
        }
        Vec::new()
    })
    .await
    .unwrap_or_default()
}

async fn read_inventory(mut command: tokio::process::Command, rpc: &str) -> Option<Vec<u8>> {
    let mut child = command
        .args([
            "--rpc-portal",
            rpc,
            "--verbose",
            "--output",
            "json",
            "route",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()?
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    if bytes.len() > 1_048_576 || !child.wait().await.ok()?.success() {
        return None;
    }
    Some(bytes)
}

#[derive(Deserialize)]
struct Inventory {
    node_info: LocalNode,
    peer_routes: Vec<PeerRoute>,
}

#[derive(Deserialize)]
struct LocalNode {
    peer_id: u32,
    ipv4_addr: String,
    public_ipv6_addr: Option<Value>,
}

#[derive(Deserialize)]
struct PeerRoute {
    route: Option<Route>,
}

#[derive(Deserialize)]
struct Route {
    peer_id: u32,
    cost: i32,
    ipv4_addr: Option<Value>,
    ipv6_addr: Option<Value>,
    public_ipv6_addr: Option<Value>,
}

#[derive(Deserialize)]
struct Instance {
    result: Inventory,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Output {
    Single(Inventory),
    Multiple(Vec<Instance>),
}

fn ipv4(value: &Value) -> Option<IpAddr> {
    if value.get("network_length")?.as_u64()? > 32 {
        return None;
    }
    let addr = u32::try_from(value.get("address")?.get("addr")?.as_u64()?).ok()?;
    Some(Ipv4Addr::from(addr).into())
}

fn ipv6(value: &Value) -> Option<IpAddr> {
    if value.get("network_length")?.as_u64()? > 128 {
        return None;
    }
    let address = value.get("address")?;
    let mut bits = 0u128;
    for key in ["part1", "part2", "part3", "part4"] {
        let part = u32::try_from(address.get(key)?.as_u64()?).ok()?;
        bits = (bits << 32) | u128::from(part);
    }
    Some(Ipv6Addr::from(bits).into())
}

fn usable(ip: IpAddr) -> Option<IpAddr> {
    let ip = match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip.into()),
        ip => ip,
    };
    let allowed = match ip {
        IpAddr::V4(ip) => {
            !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && ip.octets()[0] != 0
        }
        IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_unicast_link_local()
        }
    };
    allowed.then_some(ip)
}

/// Read the raw route-list schema (2.6.4), not display rows or tunnel endpoints.
/// Deserialize only routing fields: CLI output can also contain private config,
/// which must never be logged or retained as a fixture.
fn parse_routes(bytes: &[u8], port: u16) -> Option<Vec<SocketAddr>> {
    if port == 0 {
        return None;
    }
    let inventories = match serde_json::from_slice::<Output>(bytes).ok()? {
        Output::Single(inventory) => vec![inventory],
        Output::Multiple(instances) if !instances.is_empty() => instances
            .into_iter()
            .map(|instance| instance.result)
            .collect(),
        _ => return None,
    };
    let self_ips: HashSet<IpAddr> = inventories
        .iter()
        .flat_map(|inventory| {
            let node = &inventory.node_info;
            let v4 = node
                .ipv4_addr
                .split('/')
                .next()
                .and_then(|ip| ip.parse().ok());
            let v6 = node.public_ipv6_addr.as_ref().and_then(ipv6);
            [v4, v6].into_iter().flatten()
        })
        .collect();
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for inventory in inventories {
        for pair in inventory.peer_routes {
            let Some(route) = pair.route else { continue };
            if route.peer_id == inventory.node_info.peer_id || route.cost <= 0 {
                continue;
            }
            for ip in [
                route.ipv4_addr.as_ref().and_then(ipv4),
                route.ipv6_addr.as_ref().and_then(ipv6),
                route.public_ipv6_addr.as_ref().and_then(ipv6),
            ]
            .into_iter()
            .flatten()
            .filter_map(usable)
            {
                if !self_ips.contains(&ip) && seen.insert(ip) {
                    candidates.push(SocketAddr::new(ip, port));
                    if candidates.len() == 256 {
                        return Some(candidates);
                    }
                }
            }
        }
    }
    Some(candidates)
}

#[cfg(test)]
#[path = "../tests/support/easytier_discovery.rs"]
mod tests;
