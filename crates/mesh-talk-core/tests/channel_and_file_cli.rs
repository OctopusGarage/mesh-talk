//! Two-process integration rig for the channel + file slices: two real
//! `mesh-talk-node` processes discover each other over UDP broadcast on loopback,
//! then (1) one creates a channel with the other and sends a message the recipient
//! prints, and (2) one sends a small file the recipient prints + writes to disk.
//! `#[ignore]`d — spawns real processes and uses UDP broadcast, which can be
//! blocked/flaky in sandboxed CI. Run explicitly:
//!   nice -n 10 cargo test -p mesh-talk-core --test channel_and_file_cli -- --ignored --test-threads=1
//!
//! SPEED: the `mesh-talk-node` binary is built with `fast-test-kdf` automatically
//! (the crate carries a self-dev-dependency enabling that feature), so cold start is
//! ~instant rather than ~60-70s of real KDF. Nodes are started SEQUENTIALLY (start A,
//! await its line, then start B) so a slow machine never runs two KDF cold starts at once.

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
        // Pace the poll so a roster of non-matching lines can't busy-spin.
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("{who} never discovered peer {want_uid}");
}

/// Spawn Alpha + Bravo (sequentially), await mutual discovery, return them and their uids.
/// Both directions must converge: the sender dials the receiver, and the receiver needs
/// the sender in its roster to look up keys and decrypt.
fn two_converged_nodes(
    dir: &std::path::Path,
    discovery_port: u16,
) -> (CliNode, String, CliNode, String) {
    let mut alpha = CliNode::spawn(
        &dir.join("alpha").join("identity.keystore"),
        "Alpha",
        discovery_port,
    );
    let alpha_uid = read_user_id(&alpha);
    let mut bravo = CliNode::spawn(
        &dir.join("bravo").join("identity.keystore"),
        "Bravo",
        discovery_port,
    );
    let bravo_uid = read_user_id(&bravo);
    assert_ne!(alpha_uid, bravo_uid, "two identities must differ");

    await_discovery(&mut alpha, &bravo_uid, "Alpha");
    await_discovery(&mut bravo, &alpha_uid, "Bravo");
    (alpha, alpha_uid, bravo, bravo_uid)
}

#[test]
#[ignore = "spawns two real processes using UDP broadcast; run with --ignored"]
fn two_cli_nodes_exchange_a_channel_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Discovery port range disjoint from the other rigs so concurrent `--ignored`
    // runs can't cross-talk, perturbed per-run by the pid.
    let discovery_port = 48900 + (std::process::id() % 100) as u16;

    let (mut alpha, alpha_uid, bravo, bravo_uid) = two_converged_nodes(dir.path(), discovery_port);

    // Alpha creates a channel including Bravo, then learns the channel id from the
    // `channel <hex> created` line.
    alpha.send(&format!("/channel-new general {}", &bravo_uid[..8]));
    let created = alpha
        .wait_for(
            |l| l.starts_with("channel ") && l.ends_with(" created"),
            Duration::from_secs(15),
        )
        .expect("Alpha printed the created channel id");
    let chan_id = created
        .split_whitespace()
        .nth(1)
        .expect("created line has a channel id")
        .to_string();

    // Alpha sends a channel message; Bravo prints it, attributed to Alpha by user_id.
    alpha.send(&format!("/channel-msg {} hello-channel", &chan_id[..8]));
    let got = bravo
        .wait_for(
            |l| {
                l.starts_with("channel ")
                    && l.contains(&format!("from {alpha_uid}"))
                    && l.contains("hello-channel")
            },
            Duration::from_secs(15),
        )
        .expect("Bravo received and decrypted Alpha's channel message");
    assert!(
        got.contains(&chan_id),
        "expected the channel id {chan_id} in: {got}"
    );
}

#[test]
#[ignore = "spawns two real processes using UDP broadcast; run with --ignored"]
fn two_cli_nodes_transfer_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let discovery_port = 49000 + (std::process::id() % 100) as u16;

    // A small file with known bytes, inside the tempdir (cleaned up on every exit path).
    let src = dir.path().join("payload.txt");
    let payload = b"mesh-talk-file-e2e-known-bytes\n";
    std::fs::write(&src, payload).expect("write source file");

    let (mut alpha, _alpha_uid, bravo, bravo_uid) = two_converged_nodes(dir.path(), discovery_port);

    // Alpha sends the file to Bravo by user_id prefix.
    alpha.send(&format!("/sendfile {} {}", &bravo_uid[..8], src.display()));

    // Bravo prints the inbound-file line (saved into his data dir) with name + size.
    let got = bravo
        .wait_for(
            |l| l.starts_with("file from ") && l.contains("payload.txt"),
            Duration::from_secs(20),
        )
        .expect("Bravo received and saved Alpha's file");
    assert!(
        got.contains(&format!("({} bytes)", payload.len())),
        "expected size {} in: {got}",
        payload.len()
    );
    assert!(got.contains("saved "), "expected a saved path in: {got}");

    // Verify the bytes actually landed on disk (the saved path is the last token).
    let saved_path = got
        .split("saved ")
        .nth(1)
        .expect("saved path in the line")
        .trim();
    let written = std::fs::read(saved_path).expect("read the saved file");
    assert_eq!(written, payload, "saved file bytes must match the source");
}
