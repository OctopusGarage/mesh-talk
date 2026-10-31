//! Targeted dedicated CLI lifecycle/adapter acceptance, using disposable profiles.
#![cfg(unix)]
use mesh_talk_core::{
    discovery::{Announce, Roster},
    identity::{device::DeviceIdentity, keystore},
    node::{Node, ReceivedDm},
    postoffice::network::{NetworkMembership, NetworkPolicy, unix_now},
    storage::profile::ProfileStorageLease,
};
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
struct Relay {
    child: Child,
    reader: Option<std::thread::JoinHandle<()>>,
    lines: mpsc::Receiver<String>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn command(root: &Path, extras: &[String]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"));
    cmd.args([
        "--post-office",
        "--name",
        "acceptance",
        "--password",
        "pw",
        "--port",
        "0",
        "--discovery-port",
        "0",
        "--keystore",
    ])
    .arg(root.join("identity"))
    .args(extras);
    cmd
}
impl Relay {
    fn start(root: &Path, extras: &[String]) -> (Self, u16) {
        let mut child = command(root, extras)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let relay = Self {
            child,
            reader: Some(reader),
            lines,
        };
        loop {
            let line = relay
                .lines
                .recv_timeout(Duration::from_secs(15))
                .expect("CLI readiness");
            if let Some(tcp) = line.split("listening on tcp/").nth(1) {
                return (relay, tcp.split(',').next().unwrap().parse().unwrap());
            }
        }
    }
    fn stop(&mut self) {
        assert!(
            Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "SIGTERM must use owned shutdown: {status}"
                );
                break;
            }
            assert!(Instant::now() < end, "CLI failed to join shutdown");
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}
fn node(
    root: &Path,
    identity: DeviceIdentity,
    peers: &[Announce],
) -> (Arc<Node>, tokio::sync::mpsc::UnboundedReceiver<ReceivedDm>) {
    std::fs::create_dir(root).unwrap();
    let mut roster = Roster::default();
    for announce in peers {
        roster.update(announce, "127.0.0.1".parse().unwrap(), &identity.user_id());
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (ct, _) = tokio::sync::mpsc::unbounded_channel();
    let (ft, _) = tokio::sync::mpsc::unbounded_channel();
    let node = Node::open(
        identity,
        Arc::new(Mutex::new(roster)),
        tx,
        ct,
        ft,
        &root.join("messages.log"),
        &root.join("sent.log"),
        "pw",
    )
    .unwrap();
    (node, rx)
}
async fn ordinary_exchange(root: &Path, relay: &DeviceIdentity, port: u16) {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_id = bob.user_id();
    let relay_announce = Announce::new_post_office(relay, "acceptance", port);
    let alice_announce = Announce::new(&alice, "alice", 1);
    let bob_announce = Announce::new(&bob, "bob", 1);
    let (a, _) = node(
        &root.join("alice"),
        alice,
        &[relay_announce.clone(), bob_announce],
    );
    let (b, mut inbox) = node(&root.join("bob"), bob, &[relay_announce, alice_announce]);
    a.replace_verified_dm_pull_peers(vec![bob_id.clone()]);
    b.replace_verified_dm_pull_peers(vec![a.user_id()]);
    a.send_dm(&bob_id, b"actual indexed CLI mailbox")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            b.drain_from_post_office().await;
            if let Ok(message) = inbox.try_recv() {
                assert_eq!(message.text, b"actual indexed CLI mailbox");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("ordinary authenticated mailbox custody and recovery");
}
#[tokio::test]
async fn all_dedicated_modes_use_indexed_storage_and_join_shutdown_before_releasing_lease() {
    for mode in ["plain", "bridge", "gateway", "pinned", "network"] {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("relay");
        std::fs::create_dir(&profile).unwrap();
        let identity = keystore::load_or_create(&profile.join("identity"), "pw").unwrap();
        let mut extras = Vec::new();
        match mode {
            "bridge" => extras.push("--bridge".into()),
            "gateway" => extras.extend([
                "--bridge".into(),
                "--bridge-peer".into(),
                "127.0.0.1:1".into(),
                "--bridge-peer-id".into(),
                DeviceIdentity::generate().user_id(),
                "--bridge-proxy-port".into(),
                "0".into(),
            ]),
            "pinned" => {
                let config = root.path().join("federation.conf");
                std::fs::write(&config, "mesh-talk-federation-v1\n").unwrap();
                extras.extend([
                    "--bridge".into(),
                    "--federation-config".into(),
                    config.to_str().unwrap().into(),
                ]);
            }
            "network" => {
                let authority = DeviceIdentity::generate();
                let policy = root.path().join("policy");
                let membership = root.path().join("membership");
                std::fs::write(
                    &policy,
                    NetworkPolicy::new(authority.public().ed25519_pub)
                        .encode()
                        .unwrap(),
                )
                .unwrap();
                std::fs::write(
                    &membership,
                    NetworkMembership::issue(
                        &authority,
                        &identity.user_id(),
                        unix_now(),
                        Duration::from_secs(3600),
                    )
                    .unwrap()
                    .encode()
                    .unwrap(),
                )
                .unwrap();
                extras.extend([
                    "--relay-network".into(),
                    policy.to_str().unwrap().into(),
                    "--relay-membership".into(),
                    membership.to_str().unwrap().into(),
                ]);
            }
            _ => {}
        }
        let (mut relay, port) = Relay::start(&profile, &extras);
        assert!(
            profile.join("profile.db").exists(),
            "{mode} must publish indexed profile"
        );
        assert!(
            ProfileStorageLease::acquire(&profile).is_err(),
            "live {mode} owns the sole OS lease"
        );
        let before = std::fs::read(profile.join("identity")).unwrap();
        let mut second = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"));
        let other = second
            .args([
                "--post-office",
                "--name",
                "second",
                "--password",
                "pw",
                "--keystore",
            ])
            .arg(profile.join("second-identity"))
            .output()
            .unwrap();
        assert!(!other.status.success());
        assert!(
            !profile.join("second-identity").exists(),
            "second process fails before keystore creation"
        );
        assert_eq!(std::fs::read(profile.join("identity")).unwrap(), before);
        ordinary_exchange(root.path(), &identity, port).await;
        // A completed authenticated Noise exchange proves userspace admission.
        // Withhold the first application frame while the service stops.
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let mut admitted = mesh_talk_core::transport::SecureChannel::connect(
            stream,
            &DeviceIdentity::generate(),
            Some(&identity.public()),
        )
        .await
        .unwrap();
        relay.stop();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), admitted.recv())
                .await
                .unwrap()
                .is_err(),
            "{mode} shutdown must close the admitted authenticated exchange"
        );
        assert!(
            ProfileStorageLease::acquire(&profile).is_ok(),
            "{mode} final lease released"
        );
        // Warm published source reopens and stops through the same production path.
        let (mut warm, _) = Relay::start(&profile, &extras);
        warm.stop();
    }
}
#[test]
fn constructor_failure_releases_pre_keystore_lease_without_empty_fallback() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("profile.db"), b"invalid encrypted profile").unwrap();
    let failed = command(root.path(), &[]).output().unwrap();
    assert!(!failed.status.success());
    assert!(ProfileStorageLease::acquire(root.path()).is_ok());
    assert_eq!(
        std::fs::read(root.path().join("profile.db")).unwrap(),
        b"invalid encrypted profile"
    );
}
#[test]
fn actual_dedicated_callgraph_has_one_indexed_constructor_and_shared_endpoint() {
    let main = include_str!("../src/bin/mesh-talk-node.rs");
    let start = main.find("async fn run_post_office(").unwrap();
    let end = main[start..]
        .find("/// Parse and dispatch a `/msg")
        .unwrap();
    let relay = &main[start..start + end];
    assert_eq!(
        relay.matches("IndexedPostOffice::open_with_lease(").count(),
        1
    );
    assert_eq!(relay.matches("IndexedRelayEndpoint::new(").count(), 1);
    for forbidden in [
        "PostOffice::open(",
        "run_relay_accept_loop(",
        "run_named_bridge_relay_accept_loop(",
        "run_federated_relay_accept_loop_with_directory(",
        "run_network_relay_accept_loop_with_directory(",
        "LegacyCollectingNetworkAdapter",
    ] {
        assert!(
            !relay.contains(forbidden),
            "legacy dedicated call path: {forbidden}"
        );
    }
    let federation = include_str!("../src/bin/mesh-talk-node/federation_runtime.rs")
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    assert!(!federation.contains("Mutex<PostOffice>"));
    assert!(federation.contains("sync_with_relay_checked"));
}
