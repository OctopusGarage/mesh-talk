//! Three real nodes: Carol goes offline after joining a channel, Alice sends
//! through Bob, then Alice exits. Carol must recover Alice's message from Bob.
//! Run with `cargo test -p mesh-talk-core --test channel_peer_recovery -- --ignored`.

mod support;

use mesh_talk_core::eventlog::{EventKind, persist::PersistentEventLog};
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
    fn spawn(keystore: &std::path::Path, name: &str, discovery_port: u16) -> Self {
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
        writeln!(self.stdin, "{line}").expect("write command");
        self.stdin.flush().expect("flush command");
    }

    fn wait_for(&self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(remaining) {
                Ok(line) if pred(&line) => return Some(line),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    fn user_id(&self) -> String {
        self.wait_for(|line| line.starts_with("node "), Duration::from_secs(90))
            .expect("node startup line")
            .split_whitespace()
            .nth(1)
            .expect("user id in startup line")
            .to_owned()
    }

    fn wait_for_peer(&mut self, user_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            self.send("/peers");
            if self
                .wait_for(|line| line.contains(user_id), Duration::from_secs(1))
                .is_some()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        panic!("peer {user_id} not discovered");
    }
}

impl Drop for CliNode {
    fn drop(&mut self) {
        support::stop_cli(&mut self.child, &mut self.stdin);
    }
}

#[test]
#[ignore = "spawns three real nodes using UDP discovery"]
fn offline_member_recovers_from_replica_after_sender_exits() {
    let dir = tempfile::tempdir().unwrap();
    let port = 48900 + (std::process::id() % 100) as u16;
    let mut alice = CliNode::spawn(&dir.path().join("alice/identity.keystore"), "Alice", port);
    let alice_id = alice.user_id();
    let mut bob = CliNode::spawn(&dir.path().join("bob/identity.keystore"), "Bob", port);
    let bob_id = bob.user_id();
    let carol_path = dir.path().join("carol/identity.keystore");
    let carol = CliNode::spawn(&carol_path, "Carol", port);
    let carol_id = carol.user_id();
    alice.wait_for_peer(&bob_id);
    alice.wait_for_peer(&carol_id);
    bob.wait_for_peer(&alice_id);

    alice.send(&format!(
        "/channel-new recovery {} {}",
        &bob_id[..8],
        &carol_id[..8]
    ));
    let created = alice
        .wait_for(
            |line| line.starts_with("channel ") && line.ends_with(" created"),
            Duration::from_secs(20),
        )
        .expect("Alice created the three-member channel");
    let channel = created.split_whitespace().nth(1).unwrap().to_owned();
    let carol_log = dir.path().join("carol/messages.log");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(log) = PersistentEventLog::open(&carol_log, "pw")
            && log
                .conversations()
                .iter()
                .any(|id| hex::encode(id.as_bytes()) == channel)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        PersistentEventLog::open(&carol_log, "pw")
            .unwrap()
            .conversations()
            .iter()
            .any(|id| hex::encode(id.as_bytes()) == channel),
        "Carol had the channel before disconnecting"
    );
    drop(carol);

    alice.send(&format!("/channel-msg {} sender-gone", &channel[..8]));
    bob.wait_for(
        |line| line.contains(&format!("channel {channel} from {alice_id}: sender-gone")),
        Duration::from_secs(20),
    )
    .expect("Bob stored and decrypted Alice's message");
    let bob_log = dir.path().join("bob/messages.log");
    let bob_events = PersistentEventLog::open(&bob_log, "pw").unwrap();
    let event_id = bob_events
        .conversations()
        .iter()
        .filter(|id| hex::encode(id.as_bytes()) == channel)
        .flat_map(|id| bob_events.events(id))
        .find(|event| event.kind == EventKind::Message && event.author.user_id() == alice_id)
        .expect("Bob holds Alice's signed event")
        .id;
    drop(alice);

    let carol = CliNode::spawn(&carol_path, "Carol", port);
    assert_eq!(carol.user_id(), carol_id);
    carol
        .wait_for(
            |line| line.contains(&format!("channel {channel} from {alice_id}: sender-gone")),
            Duration::from_secs(30),
        )
        .expect("Carol recovered the message from Bob with Alice offline");
    let recovered = PersistentEventLog::open(&carol_log, "pw").unwrap();
    assert!(
        recovered.has(&event_id),
        "Carol recovered Bob's exact replica"
    );
}
