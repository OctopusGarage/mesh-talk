//! Three-process offline-delivery rig for the post-office slice: a `--post-office`
//! relay + Alice + Bob. Bob goes offline; Alice DMs him (direct fails, the relay
//! holds an opaque encrypted wrapper); Bob returns and his drain decrypts it.
//! `#[ignore]`d — three real processes, a slow-starting post office (~15-25s cold
//! start: two KDFs), and UDP broadcast. Run explicitly:
//!   nice -n 10 cargo test -p mesh-talk-core --test post_office_offline -- --ignored

mod support;

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    Mutex,
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant};

use mesh_talk_core::eventlog::{
    ConversationId, Event, EventId, EventKind, EventLog, sync::network::NetworkSyncStore,
};
use mesh_talk_core::identity::device::{DeviceIdentity, PublicIdentity};
use mesh_talk_core::identity::keystore;
use mesh_talk_core::node::request_round;
use mesh_talk_core::postoffice::IndexedPostOffice;
use mesh_talk_core::postoffice::opaque::RelayMailbox;
use mesh_talk_core::storage::profile::ProfileStorageLease;
use mesh_talk_core::transport::SecureChannel;

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
        support::stop_cli(&mut self.child, &mut self.stdin);
    }
}

/// Read the identity and OS-assigned TCP port from the real startup line.
fn read_startup(node: &CliNode, prefix: &str, timeout: Duration) -> (String, SocketAddr) {
    let line = node
        .wait_for(|l| l.starts_with(prefix), timeout)
        .unwrap_or_else(|| panic!("startup line starting with {prefix:?} within {timeout:?}"));
    let user_id = line
        .split_whitespace()
        .nth(1)
        .expect("startup line has a user_id")
        .to_string();
    let port = line
        .split_whitespace()
        .find_map(|part| part.strip_prefix("tcp/"))
        .expect("startup line has a TCP port")
        .trim_end_matches(',')
        .parse::<u16>()
        .unwrap();
    (user_id, SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// Observe the running relay through its authenticated public sync boundary.
/// Opening its retired legacy path cannot inspect the indexed store, and an
/// observer must not acquire a second writer lease over either live profile.
fn await_relay_event(
    runtime: &tokio::runtime::Runtime,
    address: SocketAddr,
    identity: &DeviceIdentity,
    relay: &PublicIdentity,
    mailbox: &RelayMailbox,
    matches: impl Fn(&Event) -> bool,
) -> (EventId, EventId, ConversationId) {
    let store = Mutex::new(EventLog::default());
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last_observation = "no matching wrapper";
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let result = runtime.block_on(async {
            tokio::time::timeout(remaining.min(Duration::from_secs(5)), async {
                let stream = tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(|_| "connect failed")?;
                let mut channel = SecureChannel::connect(stream, identity, Some(relay))
                    .await
                    .map_err(|_| "authentication failed")?;
                request_round(&mut channel, &store, mailbox.id())
                    .await
                    .map_err(|_| "sync failed")?;
                Ok::<_, &str>(())
            })
            .await
        });
        last_observation = match result {
            Ok(Ok(())) => "no matching wrapper",
            Ok(Err(stage)) => stage,
            Err(_) => "observation timed out",
        };
        if result.is_ok_and(|result| result.is_ok()) {
            let observed = store.lock().unwrap();
            for outer in observed.events(&mailbox.id()) {
                let inner = mailbox
                    .open(outer)
                    .expect("relay wrapper must authenticate its signed inner event");
                if matches(&inner) {
                    assert_eq!(
                        mailbox.wrap(&inner).unwrap().id,
                        outer.id,
                        "relay retained the exact opaque wrapper"
                    );
                    assert_ne!(outer.id, inner.id);
                    assert!(!observed.has(&inner.id));
                    return (outer.id, inner.id, inner.conversation_id);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100).min(remaining));
    }
    panic!("relay durably contains only the opaque wrapper within 45s: {last_observation}");
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
    let (po_uid, po_address) = read_startup(&po, "post-office ", Duration::from_secs(90));

    // Alice and Bob (normal nodes). Keep Bob's keystore path to restart him later.
    let mut alice = CliNode::node(
        &dir.path().join("alice").join("identity.keystore"),
        "Alice",
        dp,
    );
    let bob_keystore = dir.path().join("bob").join("identity.keystore");
    let mut bob = CliNode::node(&bob_keystore, "Bob", dp);
    let (alice_uid, _) = read_startup(&alice, "node ", Duration::from_secs(90));
    let (bob_uid, _) = read_startup(&bob, "node ", Duration::from_secs(90));
    assert_ne!(alice_uid, bob_uid);

    // Everyone converges: Alice needs Bob (to address) + PO (to replicate); Bob
    // needs Alice (to decrypt later) + PO (to drain).
    await_peer(&mut alice, &bob_uid, "Alice→Bob");
    await_peer(&mut alice, &po_uid, "Alice→PO");
    await_peer(&mut bob, &alice_uid, "Bob→Alice");
    await_peer(&mut bob, &po_uid, "Bob→PO");
    // The receiver explicitly compares and verifies the sender's full device
    // fingerprint before automatic catch-up is permitted. This persists over restart.
    bob.send(&format!("/verify-peer {alice_uid}"));
    bob.wait_for(
        |line| line == "peer verified for automatic DM recovery",
        Duration::from_secs(5),
    )
    .expect("Bob verified Alice before going offline");

    // Bob goes offline.
    drop(bob);

    // Alice DMs offline Bob: direct dial fails, the event is replicated to the PO.
    alice.send(&format!("/msg {} held-hello", &bob_uid[..8]));
    // Derive the exact pairwise mailbox from the fixture identities. The
    // observer only requests already-retained wrappers from the running relay.
    let relay_log = dir.path().join("relay/relay.log");
    let alice_id = keystore::load(&dir.path().join("alice/identity.keystore"), "pw").unwrap();
    let bob_id = keystore::load(&bob_keystore, "pw").unwrap();
    let relay_id = keystore::load(&dir.path().join("relay/identity.keystore"), "pw").unwrap();
    let mailbox = RelayMailbox::new(&alice_id, &bob_id.public(), &relay_id.public()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let replicated = await_relay_event(
        &runtime,
        po_address,
        &alice_id,
        &relay_id.public(),
        &mailbox,
        |event| event.kind == EventKind::Message && event.author.user_id() == alice_uid,
    );

    // Create a private channel while Bob is still offline. The relay must hold
    // its original channel event only inside the same pairwise mailbox.
    alice.send(&format!("/channel-new relay-secret-room {}", &bob_uid[..8]));
    let created = alice
        .wait_for(
            |line| line.starts_with("channel ") && line.ends_with(" created"),
            Duration::from_secs(30),
        )
        .expect("offline channel invitation was accepted");
    let group_hex = created.split_whitespace().nth(1).unwrap().to_string();
    let group = ConversationId::new(hex::decode(&group_hex).unwrap().try_into().unwrap());
    alice.send(&format!("/channel-msg {} held-channel", &group_hex[..8]));
    let replicated_group = await_relay_event(
        &runtime,
        po_address,
        &alice_id,
        &relay_id.public(),
        &mailbox,
        |event| event.conversation_id == group && event.kind == EventKind::Message,
    );

    // Route isolation: the original sender must be gone before the recipient
    // returns, otherwise ordinary peer synchronization could fake relay success.
    drop(alice);

    // Bob comes back (same keystore → same identity); his drain pulls from the PO.
    let bob = CliNode::node(&bob_keystore, "Bob", dp);
    assert_eq!(
        read_startup(&bob, "node ", Duration::from_secs(90)).0,
        bob_uid
    );
    let deadline = Instant::now() + Duration::from_secs(45);
    let (mut got_dm, mut got_group) = (false, false);
    while !(got_dm && got_group) {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("Bob received the held DM and channel message from the relay");
        let line = bob
            .lines
            .recv_timeout(remaining)
            .expect("Bob received held relay events before timeout");
        got_dm |= line.starts_with(&format!("from {alice_uid}")) && line.contains("held-hello");
        got_group |= line.starts_with(&format!("channel {group_hex} from {alice_uid}:"))
            && line.contains("held-channel");
    }
    drop(bob);
    drop(po);
    let retained = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_log.parent().unwrap()).unwrap(),
        &relay_log,
        "pw",
        relay_id,
    )
    .expect("reopen stopped indexed relay store");
    for (outer, inner, conversation) in [replicated, replicated_group] {
        assert!(retained.durable_have(&mailbox.id(), &outer).unwrap());
        assert!(
            !retained.durable_have(&conversation, &inner).unwrap(),
            "the original signed event must never be retained by the relay"
        );
    }
    assert_eq!(
        retained
            .metadata(&retained.open_view(&group).unwrap())
            .unwrap()
            .count,
        0,
        "channel ID stays private"
    );
}
