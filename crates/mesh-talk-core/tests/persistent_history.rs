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

mod support;

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct CliNode {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    transcript: Arc<Mutex<VecDeque<String>>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

fn retain_line(transcript: &Mutex<VecDeque<String>>, line: String) {
    let mut transcript = transcript.lock().unwrap();
    if transcript.len() == 64 {
        transcript.pop_front();
    }
    transcript.push_back(line.chars().take(1024).collect());
}

fn read_cli_lines(
    stream: impl Read,
    name: &str,
    transcript: &Mutex<VecDeque<String>>,
    output: Option<mpsc::Sender<String>>,
) {
    for line in BufReader::new(stream).lines() {
        match line {
            Ok(line) => {
                retain_line(transcript, format!("{name}: {line}"));
                if output
                    .as_ref()
                    .is_some_and(|output| output.send(line).is_err())
                {
                    break;
                }
            }
            Err(error) => {
                retain_line(transcript, format!("{name} read error: {error}"));
                return;
            }
        }
    }
    retain_line(transcript, format!("{name} EOF"));
}

#[derive(Debug, PartialEq, Eq)]
enum HistoryObservationError {
    Response(String),
    Timeout,
    Closed,
    TooManyRows,
}

fn wait_for_history_line(
    lines: &Receiver<String>,
    pending: &mut VecDeque<String>,
    message: &str,
    timeout: Duration,
) -> Result<String, HistoryObservationError> {
    let deadline = Instant::now() + timeout;
    if let Some(index) = pending.iter().position(|line| line.contains(message)) {
        return Ok(pending.remove(index).unwrap());
    }
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(HistoryObservationError::Timeout)?;
        let line = lines.recv_timeout(remaining).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => HistoryObservationError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => HistoryObservationError::Closed,
        })?;
        if line == "(no history)"
            || line.starts_with("history unavailable:")
            || line.starts_with("no peer matches prefix")
            || line == "peer vanished from roster"
            || line.ends_with("peers; be more specific")
            || line.starts_with("usage: /history")
        {
            return Err(HistoryObservationError::Response(line));
        }
        // Live reception output is `from ...`; only the history command prints
        // the timestamped row. Replayed reception cannot prove this query.
        if line.starts_with('[') {
            if line.contains(message) {
                return Ok(line);
            }
            // One /history command requested at most 20 rows. Equal clocks
            // sort by event identity, so the reply may precede our sent row.
            if pending.len() == 20 {
                return Err(HistoryObservationError::TooManyRows);
            }
            pending.push_back(line);
        }
    }
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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mesh-talk-node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");
        let transcript = Arc::new(Mutex::new(VecDeque::new()));
        retain_line(&transcript, format!("spawn {name} pid={}", child.id()));
        let (tx, rx) = mpsc::channel();
        let output_trace = transcript.clone();
        let error_trace = transcript.clone();
        let readers = vec![
            std::thread::spawn(move || read_cli_lines(stdout, "stdout", &output_trace, Some(tx))),
            std::thread::spawn(move || read_cli_lines(stderr, "stderr", &error_trace, None)),
        ];
        CliNode {
            child,
            stdin,
            lines: rx,
            transcript,
            readers,
        }
    }

    fn send(&mut self, line: &str) {
        retain_line(&self.transcript, format!("stdin: {line}"));
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
        support::stop_cli(&mut self.child, &mut self.stdin);
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        // The first Alice lifetime ends before a later restart/query failure.
        // Emit its bounded evidence now; the test harness retains successful
        // output too. Exit status shows success versus signal termination, but
        // does not claim which side won a concurrent graceful-exit/kill race.
        eprintln!(
            "CLI pid={} exit={:?} bounded transcript={:?}",
            self.child.id(),
            self.child.try_wait(),
            self.transcript.lock().unwrap()
        );
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
    assert_eq!(
        read_uid(&alice),
        alice_uid,
        "restart preserves device identity"
    );
    await_peer(&mut alice, &bob_uid, "Alice(restarted)→Bob"); // re-discover Bob to resolve + decrypt

    // /history shows BOTH directions, restored from disk.
    alice.send(&format!("/history {} 20", &bob_uid[..8]));
    let mut pending = VecDeque::new();
    let sent = wait_for_history_line(
        &alice.lines,
        &mut pending,
        "ping-from-alice",
        Duration::from_secs(10),
    )
    .expect("history shows Alice's sent message after restart");
    assert!(sent.contains("you:"), "sent line attributed to you: {sent}");
    wait_for_history_line(
        &alice.lines,
        &mut pending,
        "pong-from-bob",
        Duration::from_secs(10),
    )
    .expect("history shows Bob's received reply after restart");
}

#[test]
fn history_observer_preserves_terminal_query_failure_without_reissuing_command() {
    for response in [
        "(no history)",
        "history unavailable: persistent order cursor is stale",
        "no peer matches prefix '01234567'",
        "peer vanished from roster",
    ] {
        let (sender, lines) = mpsc::channel();
        sender.send(response.to_owned()).unwrap();
        assert_eq!(
            wait_for_history_line(
                &lines,
                &mut VecDeque::new(),
                "ping-from-alice",
                Duration::from_secs(1)
            ),
            Err(HistoryObservationError::Response(response.to_owned()))
        );
    }
    let (sender, lines) = mpsc::channel();
    drop(sender);
    assert_eq!(
        wait_for_history_line(
            &lines,
            &mut VecDeque::new(),
            "ping-from-alice",
            Duration::from_secs(1)
        ),
        Err(HistoryObservationError::Closed)
    );
}

#[test]
fn history_observer_requires_query_rows_and_retains_bounded_nonmatching_output() {
    let transcript = Mutex::new(VecDeque::new());
    let mut pending = VecDeque::new();
    let (sender, lines) = mpsc::channel();
    read_cli_lines(
        std::io::Cursor::new(
            b"from bob: ping-from-alice\n[123] you: ping-from-alice\n[124] bob: pong-from-bob\n",
        ),
        "stdout",
        &transcript,
        Some(sender),
    );
    assert_eq!(
        wait_for_history_line(
            &lines,
            &mut pending,
            "ping-from-alice",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[123] you: ping-from-alice"
    );
    assert_eq!(
        wait_for_history_line(
            &lines,
            &mut pending,
            "pong-from-bob",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[124] bob: pong-from-bob"
    );
    assert!(
        transcript
            .lock()
            .unwrap()
            .iter()
            .any(|line| line == "stdout: from bob: ping-from-alice")
    );
    let (sender, reversed) = mpsc::channel();
    read_cli_lines(
        std::io::Cursor::new(b"[123] bob: pong-from-bob\n[123] you: ping-from-alice\n"),
        "stdout",
        &transcript,
        Some(sender),
    );
    assert_eq!(
        wait_for_history_line(
            &reversed,
            &mut pending,
            "ping-from-alice",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[123] you: ping-from-alice"
    );
    assert_eq!(
        pending.front().map(String::as_str),
        Some("[123] bob: pong-from-bob")
    );
    assert_eq!(
        wait_for_history_line(
            &reversed,
            &mut pending,
            "pong-from-bob",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[123] bob: pong-from-bob"
    );
    assert!(pending.is_empty());
    for index in 0..100 {
        retain_line(&transcript, format!("stderr: {index} {}", "x".repeat(2000)));
    }
    let retained = transcript.lock().unwrap();
    assert_eq!(retained.len(), 64);
    assert!(retained.front().unwrap().starts_with("stderr: 36 "));
    assert!(retained.iter().all(|line| line.len() <= 1024));
}
