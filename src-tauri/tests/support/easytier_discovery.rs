use super::*;
use serde_json::json;

#[test]
fn supported_cli_schemas_supply_virtual_addresses() {
    let fixture = include_bytes!("../fixtures/easytier-routes-2.6.4.json");
    assert_eq!(
        parse_routes(fixture, 47474),
        Some(vec!["10.253.77.2:47474".parse().unwrap()])
    );
}

#[test]
fn discovers_multihop_and_ipv6_without_expanding_proxy_networks() {
    let inventory = json!({
        "node_info": {"peer_id": 1, "ipv4_addr": "10.1.0.1/24"},
        "peer_routes": [
            {"route": {"peer_id": 2, "cost": 3,
                "ipv4_addr": {"address": {"addr": 167903234}, "network_length": 24},
                "ipv6_addr": {"address": {"part1": 4244635648u32, "part2": 0, "part3": 0, "part4": 2}, "network_length": 64},
                "proxy_cidrs": ["192.168.0.0/16"], "public_ip": ["203.0.113.1"]}},
            {"route": {"peer_id": 1, "cost": 1,
                "ipv4_addr": {"address": {"addr": 167837697}, "network_length": 24}}}
        ]
    });
    let endpoints = parse_routes(&serde_json::to_vec(&inventory).unwrap(), 47475).unwrap();
    assert_eq!(
        endpoints,
        vec![
            "10.2.0.2:47475".parse::<SocketAddr>().unwrap(),
            "[fd00::2]:47475".parse::<SocketAddr>().unwrap(),
        ]
    );
}

#[test]
fn malformed_and_unrelated_schemas_are_unavailable() {
    for input in [
        b"not json".as_slice(),
        b"{}",
        b"[]",
        b"{\"peer_routes\":[]}",
    ] {
        assert_eq!(parse_routes(input, 47474), None);
    }
}

#[test]
fn inactive_missing_and_unusable_ipv6_routes_are_not_probed() {
    let v6 = |part1: u32, part3: u32, part4: u32| {
        json!({
            "address": {"part1": part1, "part2": 0, "part3": part3, "part4": part4},
            "network_length": 128
        })
    };
    let mut routes: Vec<_> = [
        v6(0, 0, 0),
        v6(0, 0, 1),
        v6(0xfe800000, 0, 2),
        v6(0xff020000, 0, 2),
        v6(0, 0xffff, 0x7f000001),
    ]
    .into_iter()
    .map(|ip| json!({"route": {"peer_id": 2, "cost": 1, "ipv6_addr": ip}}))
    .collect();
    routes.extend([
        json!({"route": {"peer_id": 3, "cost": 1, "proxy_cidrs": ["192.168.0.0/16"]}}),
        json!({"route": {"peer_id": 4, "cost": 0,
            "ipv4_addr": {"address": {"addr": 0x0a000003u32}, "network_length": 24}}}),
        json!({"route": {"peer_id": 5, "cost": 1,
            "ipv4_addr": {"address": {"addr": 0x0a000004u32}, "network_length": 33}}}),
    ]);
    for peer_routes in [routes, vec![]] {
        let output = json!({"node_info": {"peer_id": 1, "ipv4_addr": "10.0.0.1/24"}, "peer_routes": peer_routes});
        assert_eq!(
            parse_routes(&serde_json::to_vec(&output).unwrap(), 47474),
            Some(vec![])
        );
    }
}

#[test]
fn combines_instances_excluding_all_local_ips_and_unusable_routes() {
    let instance = |own: u32, local: &str, addresses: &[u32]| {
        json!({
            "instance_id": format!("instance-{own}"),
            "result": {
                "node_info": {"peer_id": own, "ipv4_addr": local},
                "peer_routes": addresses.iter().enumerate().map(|(i, addr)| json!({
                    "route": {"peer_id": i as u32 + 100, "cost": 1,
                        "ipv4_addr": {"address": {"addr": addr}, "network_length": 32}}
                })).collect::<Vec<_>>()
            }
        })
    };
    let output = json!([
        instance(
            1,
            "10.0.0.1/24",
            &[0, 0x7f000001, 0xe00000a7, u32::MAX, 0x0a000002, 0x0a000003]
        ),
        instance(2, "10.0.0.2/24", &[0x0a000003, 0x0a000004])
    ]);
    assert_eq!(
        parse_routes(&serde_json::to_vec(&output).unwrap(), 47474).unwrap(),
        vec![
            "10.0.0.3:47474".parse::<SocketAddr>().unwrap(),
            "10.0.0.4:47474".parse::<SocketAddr>().unwrap(),
        ]
    );
}

#[test]
fn bounds_and_deduplicates_candidates() {
    let routes: Vec<_> = (2..=300)
        .flat_map(|id| {
            let pair = json!({"route": {"peer_id": id, "cost": 2,
            "ipv4_addr": {"address": {"addr": 0x0a000000u32 + id}, "network_length": 24}}});
            [pair.clone(), pair]
        })
        .chain([
            json!({"route": {"peer_id": 400, "cost": 1, "proxy_cidrs": ["192.168.0.0/16"]}}),
            json!({"route": {"peer_id": 401, "cost": 0,
            "ipv4_addr": {"address": {"addr": 0x0a010001u32}, "network_length": 24}}}),
        ])
        .collect();
    let output =
        json!({"node_info": {"peer_id": 1, "ipv4_addr": "10.0.0.1/24"}, "peer_routes": routes});
    let candidates = parse_routes(&serde_json::to_vec(&output).unwrap(), 47474).unwrap();
    assert_eq!(candidates.len(), 256);
    assert_eq!(
        candidates
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        256
    );
    assert_eq!(parse_routes(&serde_json::to_vec(&output).unwrap(), 0), None);
}

// Fresh executable fixtures compete for OS process-launch resources. Keep
// each bounded CLI sequence isolated from the other subprocess fixtures; the
// production snapshot deadline and cancellation assertions remain unchanged.
#[cfg(unix)]
static INVENTORY_CLI_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(unix)]
fn fake_cli(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_falls_back_and_queries_only_the_selected_rpc_routes() {
    let _cli = INVENTORY_CLI_TEST.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("routes.json");
    std::fs::write(
        &fixture,
        include_bytes!("../fixtures/easytier-routes-2.6.4.json"),
    )
    .unwrap();
    let failed = fake_cli(dir.path(), "failed", "exit 3");
    let malformed = fake_cli(dir.path(), "malformed", "printf '%s' '{}' ");
    let ready = fake_cli(
        dir.path(),
        "ready",
        &format!(
            "[ \"$*\" = '--rpc-portal 127.0.0.1:15899 --verbose --output json route' ] || exit 1\ncat '{}'",
            fixture.display()
        ),
    );
    let result = local_inventory_from(
        vec![dir.path().join("missing"), failed, malformed, ready],
        "127.0.0.1:15899",
        47475,
    )
    .await;
    assert_eq!(result, vec!["10.253.77.2:47475".parse().unwrap()]);
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_or_failed_output_never_becomes_candidates() {
    let _cli = INVENTORY_CLI_TEST.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let oversized = fake_cli(dir.path(), "oversized", "head -c 1048577 /dev/zero");
    let failed = fake_cli(dir.path(), "failed", "exit 1");
    assert!(
        local_inventory_from(vec![oversized, failed], "127.0.0.1:15888", 47474)
            .await
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_inventory_kills_the_running_cli() {
    let _cli = INVENTORY_CLI_TEST.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let hung = fake_cli(
        dir.path(),
        "hung",
        &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()),
    );
    let task = tokio::spawn(local_inventory_from(vec![hung], "127.0.0.1:15888", 47474));
    let pid = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            // Creating the file precedes writing the PID; existence alone can
            // expose an empty file when the test and child run concurrently.
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|value| value.trim().parse::<i32>().ok())
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("test CLI must publish its process ID");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), async {
        // SAFETY: signal zero only checks whether the known test child exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled inventory must not leave a CLI process running");
}

#[cfg(unix)]
#[tokio::test]
async fn a_hung_cli_returns_no_candidates_within_the_snapshot_budget() {
    let _cli = INVENTORY_CLI_TEST.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let hung = fake_cli(dir.path(), "hung", "exec sleep 30");
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        local_inventory_from(vec![hung], "127.0.0.1:15888", 47474),
    )
    .await;
    assert!(
        result
            .expect("inventory must finish before the core deadline")
            .is_empty()
    );
}
