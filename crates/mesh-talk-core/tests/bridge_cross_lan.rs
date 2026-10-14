//! Four-process bridge rig. Separate UDP ports model LAN discovery scopes;
//! each participant learns E through a routed unicast query, then E introduces
//! A/B and C. DMs must cross the opaque post-office mailbox in both directions.
mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Node {
    child: Child,
    input: ChildStdin,
    lines: Receiver<String>,
}

impl Node {
    fn start(dir: &std::path::Path, name: &str, port: u16, relay: bool) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"));
        command
            .args([
                "--keystore",
                dir.join("identity.keystore").to_str().unwrap(),
            ])
            .args(["--password", "pw", "--name", name])
            .args(["--discovery-port", &port.to_string()]);
        if relay {
            command.args(["--post-office", "--bridge"]);
        } else {
            command.arg("--bridge-discovery");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            lines,
        }
    }

    fn send(&mut self, command: &str) {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
    }

    fn until(&self, text: &str, timeout: Duration) -> String {
        let end = Instant::now() + timeout;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("did not observe {text:?} within {timeout:?}"));
            if line.contains(text) {
                return line;
            }
        }
    }

    fn identity(&self, prefix: &str) -> String {
        self.until(prefix, Duration::from_secs(90))
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned()
    }

    fn await_peer(&mut self, id: &str) {
        let end = Instant::now() + Duration::from_secs(45);
        while Instant::now() < end {
            self.send("/peers");
            let poll_end = Instant::now() + Duration::from_millis(500);
            while Instant::now() < poll_end {
                if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(100))
                    && line.contains(id)
                {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        panic!("peer {id} was not introduced");
    }

    fn await_relay(&mut self, id: &str, port: u16) {
        let end = Instant::now() + Duration::from_secs(30);
        let mut last = String::new();
        while Instant::now() < end {
            self.send(&format!("/probe 127.0.0.1:{port}"));
            self.send("/peers");
            let poll_end = Instant::now() + Duration::from_secs(2);
            while Instant::now() < poll_end {
                if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(150)) {
                    if line.contains(id) {
                        return;
                    }
                    last = line;
                }
            }
        }
        panic!("relay {id} was not discovered; last output: {last}");
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        support::stop_cli(&mut self.child, &mut self.input);
    }
}

#[test]
#[ignore = "four real processes and three UDP discovery scopes"]
fn bridge_introduces_both_lans_and_delivers_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    let base = 49000 + (std::process::id() % 1000) as u16 * 4;
    let mut e = Node::start(&dir.path().join("e"), "E", base, true);
    let e_id = e.identity("post-office ");
    let mut a = Node::start(&dir.path().join("a"), "A", base + 1, false);
    let a_id = a.identity("node ");
    let mut b = Node::start(&dir.path().join("b"), "B", base + 2, false);
    let b_id = b.identity("node ");
    let mut c = Node::start(&dir.path().join("c"), "C", base + 3, false);
    let c_id = c.identity("node ");

    // All three can reach E. C's UDP scope is isolated from A/B's.
    for node in [&mut a, &mut b, &mut c] {
        node.await_relay(&e_id, base);
    }
    // On real LAN X, A and B hear multicast. Distinct UDP ports avoid
    // SO_REUSEPORT unicast ambiguity when simulating both on one host.
    a.await_relay(&b_id, base + 2);
    b.await_relay(&a_id, base + 1);
    c.await_peer(&a_id);
    c.await_peer(&b_id);
    a.await_peer(&c_id);
    b.await_peer(&c_id);

    c.send(&format!("/msg {} hello-from-c", &a_id[..8]));
    a.until("hello-from-c", Duration::from_secs(30));
    b.send(&format!("/msg {} hello-from-b", &c_id[..8]));
    c.until("hello-from-b", Duration::from_secs(30));
    // Keep the relay alive until after both deliveries.
    e.send("/peers");
}
