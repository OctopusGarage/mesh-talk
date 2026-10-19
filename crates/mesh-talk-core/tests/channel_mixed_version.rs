//! Exercise an actual pre-replica CLI binary against the current CLI. The
//! old binary path is supplied by scripts/channel-compat-rehearsal.sh.

mod support;

use mesh_talk_core::eventlog::{EventKind, persist::PersistentEventLog};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct CliNode {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl CliNode {
    fn spawn(binary: &Path, keystore: &Path, name: &str, discovery_port: u16) -> Self {
        let mut child = Command::new(binary)
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
            .expect("spawn CLI");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn wait_for(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("did not observe {needle}"));
            let line = self.lines.recv_timeout(remaining).expect("CLI output");
            if line.contains(needle) {
                return line;
            }
        }
    }

    fn id(&self) -> String {
        self.wait_for("node ", Duration::from_secs(60))
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned()
    }

    fn wait_for_peer(&mut self, peer_id: &str) {
        for _ in 0..30 {
            self.send("/peers");
            if self
                .lines
                .recv_timeout(Duration::from_secs(1))
                .is_ok_and(|line| line.contains(peer_id))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        panic!("peer {peer_id} was not discovered");
    }
}

impl Drop for CliNode {
    fn drop(&mut self) {
        support::stop_cli(&mut self.child, &mut self.stdin);
    }
}

#[test]
#[ignore = "build the pre-replica binary with scripts/channel-compat-rehearsal.sh"]
fn old_binary_gets_legacy_messages_but_never_kind_14() {
    let old_binary = std::env::var("MESH_TALK_OLD_CLI").expect("old binary path");
    let dir = tempfile::tempdir().unwrap();
    let port = 49000 + (std::process::id() % 100) as u16;
    let mut modern = CliNode::spawn(
        Path::new(env!("CARGO_BIN_EXE_mesh-talk-node")),
        &dir.path().join("modern/identity.keystore"),
        "Modern",
        port,
    );
    let modern_id = modern.id();
    let old_log = dir.path().join("old/messages.log");
    let mut old = CliNode::spawn(
        Path::new(&old_binary),
        &dir.path().join("old/identity.keystore"),
        "Old",
        port,
    );
    let old_id = old.id();
    modern.wait_for_peer(&old_id);
    old.wait_for_peer(&modern_id);
    modern.send(&format!("/channel-new mixed {}", &old_id[..8]));
    let created = modern.wait_for(" created", Duration::from_secs(20));
    let channel = created.split_whitespace().nth(1).unwrap().to_owned();
    modern.send(&format!("/channel-msg {} before-upgrade", &channel[..8]));
    old.wait_for("before-upgrade", Duration::from_secs(20));
    modern.send(&format!("/channel-replica {} off", &channel[..8]));
    modern.wait_for("replica disabled", Duration::from_secs(20));
    modern.send(&format!("/channel-msg {} after-upgrade", &channel[..8]));
    std::thread::sleep(Duration::from_secs(3));
    let old_events = PersistentEventLog::open(&old_log, "pw").unwrap();
    let channel_id = mesh_talk_core::eventlog::ConversationId::new(
        hex::decode(&channel).unwrap().try_into().unwrap(),
    );
    assert!(
        old_events
            .events(&channel_id)
            .iter()
            .any(|event| event.kind == EventKind::Message)
    );
    assert!(
        old_events
            .events(&channel_id)
            .iter()
            .all(|event| event.kind != EventKind::ChannelReplica)
    );
    assert_eq!(
        old_events
            .events(&channel_id)
            .iter()
            .filter(|event| event.kind == EventKind::Message)
            .count(),
        1
    );
}
