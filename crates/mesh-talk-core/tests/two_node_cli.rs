//! Two-process integration rig for the "online direct DM" slice: two real
//! `mesh-talk-node` processes discover each other over UDP broadcast on
//! loopback, then one DMs the other and we assert the recipient prints the
//! decrypted message. `#[ignore]`d — spawns real processes and uses UDP
//! broadcast, which can be blocked/flaky in sandboxed CI. Run explicitly:
//!   nice -n 10 cargo test -p mesh-talk-core --test two_node_cli -- --ignored
//!
//! SPEED: the `mesh-talk-node` binary handed to this test is built with the
//! `fast-test-kdf` feature automatically — the crate carries a self-dev-dependency
//! (`mesh-talk-core = { path = ".", features = ["fast-test-kdf"] }`) so Cargo unifies
//! that feature onto the bin when compiling test targets. Cold start is therefore
//! ~instant rather than ~60-70s of real Argon2/PBKDF2, with no effect on release builds.
//! Nodes are also started SEQUENTIALLY (start A, await its line, then start B) so a
//! slow machine never has to run two KDF-heavy cold starts concurrently.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A spawned `mesh-talk-node` child with a line-buffered view of its stdout.
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

    /// Block until a line satisfying `pred` arrives, or `timeout` elapses.
    /// Non-matching lines are discarded.
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

/// Read the `user_id` from a node's startup line: `node <uid> listening ...`.
fn read_user_id(node: &CliNode) -> String {
    let line = node
        // With `fast-test-kdf` the startup line is near-instant; this generous 180s
        // ceiling is pure slow-machine margin (e.g. a heavily-contended CI box building
        // without the feature), not a tuned value. Nodes are started sequentially, so at
        // most one cold start is ever in flight here.
        .wait_for(|l| l.starts_with("node "), Duration::from_secs(180))
        .expect("node printed its startup line");
    line.split_whitespace()
        .nth(1)
        .expect("startup line has a user_id")
        .to_string()
}

/// Poll `/peers` until `want_uid` appears in the roster, or fail after 20s.
fn await_discovery(node: &mut CliNode, want_uid: &str, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        node.send("/peers");
        if node
            .wait_for(|l| l.contains(want_uid), Duration::from_secs(1))
            .is_some()
        {
            return;
        }
        // Pace the poll: without this, a roster full of non-matching lines (a
        // stray node on the shared port) would let `wait_for` return instantly
        // and spin this loop at full CPU until the deadline.
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("{who} never discovered peer {want_uid}");
}

#[test]
#[ignore = "spawns two real processes using UDP broadcast; run with --ignored"]
fn two_cli_nodes_exchange_a_dm() {
    let dir = tempfile::tempdir().expect("tempdir");
    // An uncommon shared discovery port for the rig, perturbed per-run by the
    // process id so overlapping runs (or a stray node on a fixed port) can't
    // cross-talk. Both children use this same value, so they still find each other.
    let discovery_port = 47600 + (std::process::id() % 1000) as u16;

    // Each node gets its OWN subdir: account.keystore + messages.log + sent.log are derived
    // from the keystore's parent dir, so a shared dir would clobber them and crash a node.
    //
    // Start SEQUENTIALLY — spawn Alpha and await its startup line BEFORE spawning Bravo —
    // so a slow machine never runs two KDF-heavy cold starts at once (the cause of the
    // 90s-budget flake). This mirrors post_office_offline's reliable ordering.
    let mut alpha = CliNode::spawn(
        &dir.path().join("alpha").join("identity.keystore"),
        "Alpha",
        discovery_port,
    );
    let alpha_uid = read_user_id(&alpha);
    let mut bravo = CliNode::spawn(
        &dir.path().join("bravo").join("identity.keystore"),
        "Bravo",
        discovery_port,
    );
    let bravo_uid = read_user_id(&bravo);
    assert_ne!(alpha_uid, bravo_uid, "two identities must differ");

    // Both directions must converge: Alpha needs Bravo to dial; Bravo needs
    // Alpha in its roster to look up her x25519 key and DECRYPT.
    await_discovery(&mut alpha, &bravo_uid, "Alpha");
    await_discovery(&mut bravo, &alpha_uid, "Bravo");

    // Alpha DMs Bravo by a user_id prefix.
    alpha.send(&format!("/msg {} hello-bravo", &bravo_uid[..8]));

    // Bravo prints the decrypted DM, attributed to Alpha by id and name.
    let got = bravo
        .wait_for(
            |l| l.starts_with(&format!("from {alpha_uid}")) && l.contains("hello-bravo"),
            Duration::from_secs(10),
        )
        .expect("Bravo received and decrypted Alpha's DM");
    assert!(
        got.contains("(Alpha)"),
        "expected sender name Alpha, got: {got}"
    );
}
