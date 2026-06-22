//! Three-process offline-delivery rig for the post-office slice: a `--post-office`
//! relay + Alice + Bob. Bob goes offline; Alice DMs him (direct fails, the relay
//! holds the sealed event); Bob returns and his drain pulls and decrypts it.
//! `#[ignore]`d — three real processes, a slow-starting post office (~15-25s cold
//! start: two KDFs), and UDP broadcast. Run explicitly:
//!   nice -n 10 cargo test -p mesh-talk-core --test post_office_offline -- --ignored

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A spawned `mesh-talk-node` (normal or `--post-office`) with a line view of stdout.
struct CliNode {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl CliNode {
    fn spawn(
        keystore: &std::path::Path,
        name: &str,
        discovery_port: u16,
        post_office: bool,
    ) -> CliNode {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"));
        cmd.arg("--keystore")
            .arg(keystore)
            .arg("--password")
            .arg("pw")
            .arg("--name")
            .arg(name)
            .arg("--port")
            .arg("0")
            .arg("--discovery-port")
            .arg(discovery_port.to_string());
        if post_office {
            cmd.arg("--post-office");
        }
        let mut child = cmd
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

    fn node(keystore: &std::path::Path, name: &str, dp: u16) -> CliNode {
        Self::spawn(keystore, name, dp, false)
    }

    fn post_office(keystore: &std::path::Path, name: &str, dp: u16) -> CliNode {
        Self::spawn(keystore, name, dp, true)
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

/// Read the user_id from a startup line beginning with `prefix` (`node ` or
/// `post-office `): the token after the prefix.
fn read_user_id(node: &CliNode, prefix: &str, timeout: Duration) -> String {
    let line = node
        .wait_for(|l| l.starts_with(prefix), timeout)
        .unwrap_or_else(|| panic!("startup line starting with {prefix:?} within {timeout:?}"));
    line.split_whitespace()
        .nth(1)
        .expect("startup line has a user_id")
        .to_string()
}

/// Poll `/peers` until `want_uid` appears in `node`'s roster, or fail after 25s.
/// Paces the poll so a roster of non-matching lines can't busy-spin.
fn await_peer(node: &mut CliNode, want_uid: &str, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(25);
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
#[ignore = "three real processes incl. a slow post office + UDP broadcast; run with --ignored"]
fn offline_dm_delivered_via_post_office() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Perturb the shared discovery port per-run, in a range disjoint from
    // two_node_cli's (47600..48599), so concurrent `--ignored` rigs can't
    // cross-talk. All three processes here use this same value.
    let dp = 48700 + (std::process::id() % 100) as u16;

    // Every process cold-starts slowly: several PBKDF2-600k unlocks each (a node opens the
    // device + account keystores + encrypted log stores; the relay also opens its log) —
    // ~18s+ per process on an idle machine under `nice`, and here THREE start concurrently
    // and contend. Timeouts are generous so the rig isn't flaky, not tuned to the cold start.
    // Each process gets its OWN subdir: a node's account.keystore + messages.log + sent.log
    // (and the relay's log) are all derived from the keystore's PARENT dir, so sharing one
    // dir would clobber them and crash a process on startup.
    let po = CliNode::post_office(
        &dir.path().join("relay").join("identity.keystore"),
        "Relay",
        dp,
    );
    let po_uid = read_user_id(&po, "post-office ", Duration::from_secs(90));

    // Alice and Bob (normal nodes). Keep Bob's keystore path to restart him later.
    let mut alice = CliNode::node(
        &dir.path().join("alice").join("identity.keystore"),
        "Alice",
        dp,
    );
    let bob_keystore = dir.path().join("bob").join("identity.keystore");
    let mut bob = CliNode::node(&bob_keystore, "Bob", dp);
    let alice_uid = read_user_id(&alice, "node ", Duration::from_secs(90));
    let bob_uid = read_user_id(&bob, "node ", Duration::from_secs(90));
    assert_ne!(alice_uid, bob_uid);

    // Everyone converges: Alice needs Bob (to address) + PO (to replicate); Bob
    // needs Alice (to decrypt later) + PO (to drain).
    await_peer(&mut alice, &bob_uid, "Alice→Bob");
    await_peer(&mut alice, &po_uid, "Alice→PO");
    await_peer(&mut bob, &alice_uid, "Bob→Alice");
    await_peer(&mut bob, &po_uid, "Bob→PO");

    // Bob goes offline.
    drop(bob);

    // Alice DMs offline Bob: direct dial fails, the event is replicated to the PO.
    alice.send(&format!("/msg {} held-hello", &bob_uid[..8]));
    // Best-effort wait for Alice's spawned send (direct-fail + PO replication) to
    // land at the relay. Not synchronised — there is no PO inbox-count command to
    // poll yet; generous for loopback. Bob's final wait_for is the real backstop.
    std::thread::sleep(Duration::from_secs(3));

    // Bob comes back (same keystore → same identity); his drain pulls from the PO.
    let bob = CliNode::node(&bob_keystore, "Bob", dp);
    let _ = read_user_id(&bob, "node ", Duration::from_secs(90));
    let got = bob
        .wait_for(
            |l| l.starts_with(&format!("from {alice_uid}")) && l.contains("held-hello"),
            Duration::from_secs(30),
        )
        .expect("Bob received the held DM from the post office after coming back online");
    assert!(
        got.contains("(Alice)"),
        "expected sender name Alice, got: {got}"
    );
}
