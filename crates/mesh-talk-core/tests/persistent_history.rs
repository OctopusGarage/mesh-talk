//! Persistence rig: Alice and Bob exchange DMs both directions; Alice restarts on
//! the same keystore; her `/history` shows both her sent message (from the local
//! sidecar) and Bob's reply (decrypted from the durable log) — proving message
//! history survives a real process restart. No post office: history comes purely
//! from disk. `#[ignore]`d (real processes + UDP broadcast). Run explicitly:
//!   nice -n 10 cargo test -p mesh-talk-core --test persistent_history -- --ignored
//!
//! SPEED: the `mesh-talk-node` binary handed to this test is built with the
//! `fast-test-kdf` feature automatically — the crate carries a self-dev-dependency
//! (`mesh-talk-core = { path = ".", features = ["fast-test-kdf"] }`) so Cargo unifies
//! that feature onto the bin when compiling test targets. Cold start is therefore
//! ~instant rather than ~60-70s of real Argon2/PBKDF2, with no effect on release builds.
//! Nodes are also started SEQUENTIALLY (start one, await its line, then the next) so a
//! slow machine never has to run two KDF-heavy cold starts concurrently.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct CliNode {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl CliNode {
    fn spawn(keystore: &std::path::Path, name: &str, discovery_port: u16) -> CliNode {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
            .arg("--keystore")
            .arg(keystore)
            .arg("--password")
            .arg("pw")
            .arg("--name")
            .arg(name)
            .arg("--port")
            .arg("0")
            .arg("--discovery-port")
            .arg(discovery_port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mesh-talk-node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        CliNode {
            child,
            stdin,
            lines: rx,
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to child stdin");
        self.stdin.flush().expect("flush child stdin");
    }

    fn wait_for(&self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(remaining) {
                Ok(l) => {
                    if pred(&l) {
                        return Some(l);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}

impl Drop for CliNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_uid(node: &CliNode) -> String {
    // With `fast-test-kdf` the startup line is near-instant; this generous 180s ceiling
    // is pure slow-machine margin (e.g. a contended CI box building without the feature),
    // not a tuned value. Nodes are started sequentially, so at most one cold start is ever
    // in flight at a time.
    let line = node
        .wait_for(|l| l.starts_with("node "), Duration::from_secs(180))
        .expect("node startup line");
    line.split_whitespace().nth(1).expect("uid").to_string()
}

fn await_peer(node: &mut CliNode, want_uid: &str, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        node.send("/peers");
        if node
            .wait_for(|l| l.contains(want_uid), Duration::from_secs(1))
            .is_some()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("{who} never discovered {want_uid}");
}

#[test]
#[ignore = "real processes + UDP broadcast; run with --ignored"]
fn message_history_survives_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Perturbed per-run, disjoint from the other rigs' ranges (47600.., 48700..).
    let dp = 48900 + (std::process::id() % 100) as u16;

    // Each node gets its own sub-directory so their sibling data files
    // (messages.log, sent.log) don't collide in the shared tempdir.
    let alice_dir = dir.path().join("alice");
    std::fs::create_dir_all(&alice_dir).expect("alice dir");
    let bob_dir = dir.path().join("bob");
    std::fs::create_dir_all(&bob_dir).expect("bob dir");
    let alice_keystore = alice_dir.join("identity.keystore");
    // Start SEQUENTIALLY — spawn Alice and await her startup line BEFORE spawning Bob —
    // so a slow machine never runs two KDF-heavy cold starts at once (the cause of the
    // 90s-budget flake). This mirrors post_office_offline's reliable ordering.
    let mut alice = CliNode::spawn(&alice_keystore, "Alice", dp);
    let alice_uid = read_uid(&alice);
    let mut bob = CliNode::spawn(&bob_dir.join("identity.keystore"), "Bob", dp);
    let bob_uid = read_uid(&bob);

    await_peer(&mut alice, &bob_uid, "Alice→Bob");
    await_peer(&mut bob, &alice_uid, "Bob→Alice");

    // Alice → Bob, then Bob → Alice (both persisted on Alice's side).
    alice.send(&format!("/msg {} ping-from-alice", &bob_uid[..8]));
    bob.wait_for(|l| l.contains("ping-from-alice"), Duration::from_secs(10))
        .expect("Bob received Alice's ping");
    bob.send(&format!("/msg {} pong-from-bob", &alice_uid[..8]));
    alice
        .wait_for(|l| l.contains("pong-from-bob"), Duration::from_secs(10))
        .expect("Alice received Bob's pong");

    // Alice restarts on the same keystore (same identity + same durable files).
    drop(alice);
    let mut alice = CliNode::spawn(&alice_keystore, "Alice", dp);
    let _ = read_uid(&alice);
    await_peer(&mut alice, &bob_uid, "Alice(restarted)→Bob"); // re-discover Bob to resolve + decrypt

    // /history shows BOTH directions, restored from disk.
    alice.send(&format!("/history {} 20", &bob_uid[..8]));
    let sent = alice
        .wait_for(|l| l.contains("ping-from-alice"), Duration::from_secs(10))
        .expect("history shows Alice's sent message after restart");
    assert!(sent.contains("you:"), "sent line attributed to you: {sent}");
    alice
        .wait_for(|l| l.contains("pong-from-bob"), Duration::from_secs(10))
        .expect("history shows Bob's received reply after restart");
}
