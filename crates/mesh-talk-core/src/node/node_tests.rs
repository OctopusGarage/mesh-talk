use super::*;
use crate::discovery::announce::Announce;
use crate::node::conversation::dm_conversation_id;
use crate::node::message::MessageBody;
use crate::node::postbox::run_relay_accept_loop;
use crate::postoffice::PostOffice;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::TcpListener;

fn seed_roster(
    peer: &DeviceIdentity,
    name: &str,
    port: u16,
    self_user_id: &str,
) -> Arc<Mutex<Roster>> {
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &Announce::new(peer, name, port),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        self_user_id,
    );
    roster
}

/// Add another known peer to an existing roster (for multi-peer rigs).
fn add_peer(
    roster: &Arc<Mutex<Roster>>,
    peer: &DeviceIdentity,
    name: &str,
    port: u16,
    self_user_id: &str,
) {
    roster.lock().unwrap().update(
        &Announce::new(peer, name, port),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        self_user_id,
    );
}

#[tokio::test]
async fn send_dm_to_unknown_peer_errors() {
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let roster = Arc::new(Mutex::new(Roster::default())); // empty
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();
    let err = node.send_dm("nope", b"hi").await.unwrap_err();
    assert!(matches!(err, NodeError::UnknownPeer(u) if u == "nope"));
}

#[tokio::test]
async fn two_nodes_exchange_a_dm_over_loopback_tcp() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();

    // Bob's listener (ephemeral loopback port).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();

    // Rosters: Alice knows Bob (at his real listen port); Bob knows Alice
    // (any port — Bob never dials Alice; he just needs her key to decrypt).
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", 4000, &bob_uid);

    let dir = tempfile::tempdir().unwrap();
    let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
    let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
    let (a_ch_tx, _a_ch_rx) = mpsc::unbounded_channel();
    let (b_ch_tx, _b_ch_rx) = mpsc::unbounded_channel();
    let (a_file_tx, _a_file_rx) = mpsc::unbounded_channel();
    let (b_file_tx, _b_file_rx) = mpsc::unbounded_channel();
    let alice_node = Node::open(
        alice,
        alice_roster,
        alice_tx,
        a_ch_tx,
        a_file_tx,
        &dir.path().join("alice.log"),
        &dir.path().join("alice-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        bob_tx,
        b_ch_tx,
        b_file_tx,
        &dir.path().join("bob.log"),
        &dir.path().join("bob-sent.log"),
        "pw",
    )
    .unwrap();

    // Bob accepts and serves connections.
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    // Alice sends Bob a DM.
    alice_node
        .send_dm(&bob_uid, b"meet at 5")
        .await
        .expect("send_dm");

    // Bob surfaces the decrypted DM (bounded wait).
    let received = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
        .await
        .expect("bob received a dm within 5s")
        .expect("incoming channel open");
    assert_eq!(received.from, alice_uid);
    assert_eq!(received.from_name, "Alice");
    assert_eq!(received.text, b"meet at 5");
}

#[tokio::test]
async fn send_call_signal_to_unknown_peer_errors() {
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let roster = Arc::new(Mutex::new(Roster::default())); // empty
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();
    let err = node.send_call_signal("nope", b"{}").await.unwrap_err();
    assert!(matches!(err, NodeError::UnknownPeer(u) if u == "nope"));
}

#[tokio::test]
async fn two_nodes_exchange_a_call_signal_and_bind_from_to_the_authenticated_peer() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", 4000, &bob_uid);

    let dir = tempfile::tempdir().unwrap();
    let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
    // Bob's DM receiver: a call signal must NOT surface as a DM (it never enters the log).
    let (bob_tx, mut bob_dm_rx) = mpsc::unbounded_channel();
    let (a_ch_tx, _a_ch_rx) = mpsc::unbounded_channel();
    let (b_ch_tx, _b_ch_rx) = mpsc::unbounded_channel();
    let (a_file_tx, _a_file_rx) = mpsc::unbounded_channel();
    let (b_file_tx, _b_file_rx) = mpsc::unbounded_channel();
    let alice_node = Node::open(
        alice,
        alice_roster,
        alice_tx,
        a_ch_tx,
        a_file_tx,
        &dir.path().join("alice.log"),
        &dir.path().join("alice-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        bob_tx,
        b_ch_tx,
        b_file_tx,
        &dir.path().join("bob.log"),
        &dir.path().join("bob-sent.log"),
        "pw",
    )
    .unwrap();
    let mut bob_call_rx = bob_node
        .take_call_signal_receiver()
        .expect("call-signal receiver is claimable once");

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    // The payload self-asserts a DIFFERENT sender ("from": all-f's). The receiver must IGNORE
    // it and bind `from` to the cryptographically-authenticated Noise peer (Alice).
    let spoofed = "f".repeat(32);
    let payload = format!(r#"{{"kind":"offer","sdp":"v=0","from":"{spoofed}"}}"#);
    alice_node
        .send_call_signal(&bob_uid, payload.as_bytes())
        .await
        .expect("send_call_signal");

    let signal = tokio::time::timeout(Duration::from_secs(5), bob_call_rx.recv())
        .await
        .expect("bob received a call signal within 5s")
        .expect("call-signal channel open");
    // Authenticated binding: from is Alice's real user_id, NOT the spoofed payload field.
    assert_eq!(signal.from, alice_uid);
    assert_ne!(signal.from, spoofed);
    // Payload is delivered opaquely, byte-for-byte.
    assert_eq!(signal.payload, payload.as_bytes());

    // It is ephemeral: nothing surfaced on the DM path, and Bob's event log stayed empty.
    assert!(
        bob_dm_rx.try_recv().is_err(),
        "a call signal must not surface as a DM"
    );
    let conv = dm_conversation_id(&alice_node.identity.public(), &bob_node.identity.public());
    assert!(
        bob_node.log.lock().unwrap().events(&conv).is_empty(),
        "a call signal must not be appended to the event log"
    );
}

#[tokio::test]
async fn offline_dm_delivered_via_post_office_over_loopback() {
    // Alice sends Bob a DM while Bob is offline; a post office holds it; Bob
    // drains it and decrypts — all in-process over loopback TCP.
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let po_id = DeviceIdentity::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();

    // Post office relay on loopback. Keep extra identity copies (open() moves one).
    let po_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let po_addr = po_listener.local_addr().unwrap();
    let (po_ed, po_x) = po_id.secret_bytes();
    let po_transport = DeviceIdentity::from_secret_bytes(po_ed, po_x);
    let po_seed = DeviceIdentity::from_secret_bytes(po_ed, po_x);
    let po_store = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("po.log"), "pw", po_id).unwrap(),
    ));
    tokio::spawn(run_relay_accept_loop(
        po_transport,
        po_listener,
        Arc::clone(&po_store),
    ));

    // A closed port stands in for offline Bob — Alice's direct dial fails fast.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_dead_addr = dead.local_addr().unwrap();
    drop(dead);

    // Seed a roster entry whose addr/keys/role we control (addr = (ip, port)).
    fn seed(
        roster: &Arc<Mutex<Roster>>,
        id: &DeviceIdentity,
        name: &str,
        addr: SocketAddr,
        post_office: bool,
        self_uid: &str,
    ) {
        let announce = if post_office {
            Announce::new_post_office(id, name, addr.port())
        } else {
            Announce::new(id, name, addr.port())
        };
        roster
            .lock()
            .unwrap()
            .update(&announce, addr.ip(), self_uid);
    }

    // Alice knows offline-Bob (dead addr) + the PO (real addr).
    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    seed(&alice_roster, &bob, "Bob", bob_dead_addr, false, &alice_uid);
    seed(&alice_roster, &po_seed, "PO", po_addr, true, &alice_uid);
    // Bob knows Alice (real keys, for decryption) + the PO (to drain).
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    seed(
        &bob_roster,
        &alice,
        "Alice",
        "127.0.0.1:4000".parse().unwrap(),
        false,
        &bob_uid,
    );
    seed(&bob_roster, &po_seed, "PO", po_addr, true, &bob_uid);

    let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
    let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
    let (a_ch_tx, _a_ch_rx) = mpsc::unbounded_channel();
    let (b_ch_tx, _b_ch_rx) = mpsc::unbounded_channel();
    let (a_file_tx, _a_file_rx) = mpsc::unbounded_channel();
    let (b_file_tx, _b_file_rx) = mpsc::unbounded_channel();
    let alice_node = Node::open(
        alice,
        alice_roster,
        alice_tx,
        a_ch_tx,
        a_file_tx,
        &dir.path().join("alice.log"),
        &dir.path().join("alice-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        bob_tx,
        b_ch_tx,
        b_file_tx,
        &dir.path().join("bob.log"),
        &dir.path().join("bob-sent.log"),
        "pw",
    )
    .unwrap();

    // Alice sends while Bob is offline: direct fails, PO replication succeeds.
    // Delivery is provably PO-only here: Alice's direct dial targets a closed
    // port pinned to Bob's identity (cannot succeed), and Bob never dials Alice
    // (his drain only dials the PO) — so anything Bob receives transited the PO.
    alice_node
        .send_dm(&bob_uid, b"held-hello")
        .await
        .expect("send_dm succeeds via the post office despite offline recipient");

    // Bob drains from the PO (retry to absorb the relay's async ingest).
    let mut received = None;
    for _ in 0..25 {
        bob_node.drain_from_post_office().await;
        if let Ok(dm) = tokio::time::timeout(Duration::from_millis(200), bob_rx.recv()).await {
            received = dm;
            break;
        }
    }
    let received = received.expect("Bob received the held DM from the post office");
    assert_eq!(received.from, alice_uid);
    assert_eq!(received.from_name, "Alice");
    assert_eq!(received.text, b"held-hello");
}

#[tokio::test]
async fn history_merges_sent_and_received_in_time_order() {
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let me_pub = me.public();
    let bob_uid = bob.public().user_id();
    let conv = crate::node::conversation::dm_conversation_id(&me_pub, &bob.public());

    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &Announce::new(&bob, "Bob", 4000),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &me_pub.user_id(),
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();

    // Bob sent us a message at t=1000 (decrypted on receipt → received store).
    node.received
        .lock()
        .unwrap()
        .record(
            conv,
            bob_uid.clone(),
            1000,
            &MessageBody::new(b"hi from bob".to_vec(), None).encode(),
            EventId::new([7u8; 32]),
        )
        .unwrap();
    // We sent one at t=2000 (recorded in the sidecar as the wrapped body).
    node.sentlog
        .lock()
        .unwrap()
        .record(
            conv,
            1,
            2000,
            &MessageBody::new(b"hi from me".to_vec(), None).encode(),
        )
        .unwrap();

    let hist = node.dm_history(&bob.public(), 10);
    assert_eq!(hist.len(), 2);
    assert!(!hist[0].from_me && hist[0].text == b"hi from bob"); // t=1000 first
    assert!(hist[1].from_me && hist[1].who == "you" && hist[1].text == b"hi from me");

    // `limit` keeps the most-recent entries.
    let last1 = node.dm_history(&bob.public(), 1);
    assert_eq!(last1.len(), 1);
    assert!(last1[0].from_me); // the newer (t=2000) one
}

#[tokio::test]
async fn delete_clear_and_prune_erase_local_history() {
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let me_pub = me.public();
    let bob_uid = bob.public().user_id();
    let conv = crate::node::conversation::dm_conversation_id(&me_pub, &bob.public());

    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &Announce::new(&bob, "Bob", 4000),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &me_pub.user_id(),
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();

    let bob_msg_id = EventId::new([7u8; 32]);
    let seed = |node: &Node| {
        node.received
            .lock()
            .unwrap()
            .record(
                conv,
                bob_uid.clone(),
                1000,
                &MessageBody::new(b"old from bob".to_vec(), None).encode(),
                bob_msg_id,
            )
            .unwrap();
        node.sentlog
            .lock()
            .unwrap()
            .record(
                conv,
                1,
                3000,
                &MessageBody::new(b"new from me".to_vec(), None).encode(),
            )
            .unwrap();
    };
    seed(&node);
    assert_eq!(node.dm_history(&bob.public(), 10).len(), 2);

    // 1. Delete Bob's message for me (matched by event id; not an account conversation).
    assert_eq!(node.delete_message(conv, bob_msg_id, false).unwrap(), 1);
    let hist = node.dm_history(&bob.public(), 10);
    assert_eq!(hist.len(), 1);
    assert!(hist[0].from_me, "only my message remains");

    // 2. Retention prune: re-add only Bob's old (t=1000) message, then drop everything
    //    older than t=1500 (leaves my t=3000 send).
    node.received
        .lock()
        .unwrap()
        .record(
            conv,
            bob_uid.clone(),
            1000,
            &MessageBody::new(b"old from bob".to_vec(), None).encode(),
            bob_msg_id,
        )
        .unwrap();
    assert_eq!(node.prune_older_than(1500).unwrap(), 1); // only the t=1000 one
    let hist = node.dm_history(&bob.public(), 10);
    assert_eq!(hist.len(), 1);
    assert!(hist[0].from_me && hist[0].wall_clock == 3000);

    // 3. Clear the whole conversation.
    let removed = node.clear_conversation(conv).unwrap();
    assert!(removed >= 1);
    assert!(node.dm_history(&bob.public(), 10).is_empty());
}

#[tokio::test]
async fn recall_account_tombstones_own_message_and_enforces_window() {
    use crate::node::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();

    let my_account = node.account_id();
    let peer_account = "f".repeat(32);
    let acct_conv = crate::node::conversation::account_conversation_id(&my_account, &peer_account);

    // Helper: record one of MY account sends with a given logical id + wall_clock.
    let seed_send = |id: [u8; 32], wall: u64, text: &[u8]| {
        let env = DmEnvelope::new(
            my_account.clone(),
            peer_account.clone(),
            id,
            MessageBody::new(text.to_vec(), None).encode(),
        )
        .encode();
        node.sentlog
            .lock()
            .unwrap()
            .record(acct_conv, wall, wall, &env)
            .unwrap();
    };

    let now = crate::node::node::now_millis();
    let fresh = EventId::new([1u8; 32]);
    let old = EventId::new([2u8; 32]);
    seed_send(*fresh.as_bytes(), now, b"recall me");
    seed_send(*old.as_bytes(), now - 5 * 60 * 1000, b"too old"); // 5 min ago

    // A fresh message can be recalled (no peers → just records our own recall + tombstones).
    node.recall_account(&peer_account, fresh).await.unwrap();
    // The window has passed for the old one.
    assert!(node.recall_account(&peer_account, old).await.is_err());

    let hist = node.account_history(&peer_account, 10);
    let recalled = hist
        .iter()
        .find(|e| e.id == fresh)
        .expect("fresh entry present");
    assert!(recalled.recalled, "recalled message is tombstoned");
    assert!(recalled.text.is_empty(), "recalled content is dropped");
    // Our own recalled text stays available for "re-edit".
    assert_eq!(
        recalled.recalled_text.as_deref(),
        Some(b"recall me".as_ref()),
        "sender keeps the original text for re-edit"
    );
    let kept = hist
        .iter()
        .find(|e| e.id == old)
        .expect("old entry present");
    assert!(!kept.recalled, "the un-recalled message is untouched");
    assert_eq!(kept.text, b"too old");
}

/// Account-conversation read/erase paths that the channel tests don't reach: a sticker in
/// account history, local delete of a single account message, clearing the whole account
/// conversation, channel full-text search, and the recall_channel "unknown message" guard.
#[tokio::test]
async fn account_history_sticker_delete_clear_channel_search_and_recall_guard() {
    use crate::node::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();

    let my_account = node.account_id();
    let peer_account = "a".repeat(32);
    let acct_conv = crate::node::conversation::account_conversation_id(&my_account, &peer_account);
    let now = crate::node::node::now_millis();

    // Seed two of my own account sends (a text and a sticker) directly into the sidecar.
    let seed = |id: [u8; 32], wall: u64, body: Vec<u8>| {
        let env = DmEnvelope::new(my_account.clone(), peer_account.clone(), id, body).encode();
        let mut sl = node.sentlog.lock().unwrap();
        let seq = sl.entries(&acct_conv).len() as u64 + 1;
        sl.record(acct_conv, seq, wall, &env).unwrap();
    };
    let text_id = EventId::new([11u8; 32]);
    let sticker_id = EventId::new([12u8; 32]);
    seed(
        *text_id.as_bytes(),
        now,
        MessageBody::new(b"lunch at noon".to_vec(), None).encode(),
    );
    seed(
        *sticker_id.as_bytes(),
        now + 1,
        MessageBody::sticker("1f602".to_string(), "😂".as_bytes().to_vec()).encode(),
    );

    // Account history renders the sticker (id + emoji fallback) and the text.
    let hist = node.account_history(&peer_account, 10);
    assert_eq!(hist.len(), 2);
    let st = hist
        .iter()
        .find(|e| e.id == sticker_id)
        .expect("sticker entry present");
    assert_eq!(st.sticker.as_deref(), Some("1f602"));
    assert_eq!(st.text, "😂".as_bytes());

    // Delete a single account message locally → gone, the other remains.
    assert_eq!(
        node.delete_account_message(&peer_account, text_id).unwrap(),
        1
    );
    let hist = node.account_history(&peer_account, 10);
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].id, sticker_id);

    // Clear the whole account conversation → empty.
    assert_eq!(node.clear_account_conversation(&peer_account).unwrap(), 1);
    assert!(node.account_history(&peer_account, 10).is_empty());

    // Channel full-text search finds a channel message (the search test only covered DMs).
    let channel = node.create_channel("general", vec![]).await.unwrap();
    node.send_channel_message(channel, b"standup planning at ten")
        .await
        .unwrap();
    let hits = node.search("STANDUP");
    assert!(
        hits.iter()
            .any(|h| h.is_channel && h.text == b"standup planning at ten"),
        "channel search finds the message"
    );

    // recall_channel rejects an unknown target id.
    assert!(
        node.recall_channel(channel, EventId::new([9u8; 32]))
            .await
            .is_err(),
        "recalling an unknown message errors"
    );
}

#[tokio::test]
async fn channel_sticker_send_recall_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();

    let roster = Arc::new(Mutex::new(Roster::default()));
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();

    let channel = node.create_channel("general", vec![bob_pub]).await.unwrap();
    node.send_channel_message(channel, b"plain text")
        .await
        .unwrap();
    node.send_sticker_channel(channel, "1f602", "😂".as_bytes())
        .await
        .unwrap();

    // The sticker surfaces with its id + emoji fallback; the text message is normal.
    let hist = node.channel_history(channel, 10);
    assert_eq!(hist.len(), 2);
    let sticker = hist
        .iter()
        .find(|e| e.sticker.as_deref() == Some("1f602"))
        .expect("sticker message present");
    assert_eq!(sticker.text, "😂".as_bytes());
    let text_msg = hist
        .iter()
        .find(|e| e.text == b"plain text")
        .expect("text message present");
    let text_id = text_msg.id;
    let sticker_id_ev = sticker.id;

    // Recall the sticker (our own, fresh) → tombstoned, sticker cleared.
    node.recall_channel(channel, sticker_id_ev).await.unwrap();
    let hist = node.channel_history(channel, 10);
    let recalled = hist.iter().find(|e| e.id == sticker_id_ev).unwrap();
    assert!(recalled.recalled && recalled.sticker.is_none());

    // Delete the text message locally (sent seq->id path) → gone from history.
    assert_eq!(node.delete_message(channel, text_id, false).unwrap(), 1);
    let hist = node.channel_history(channel, 10);
    assert!(!hist.iter().any(|e| e.id == text_id));
}

#[tokio::test]
async fn reopen_suppresses_recorded_history_but_retries_unrecorded() {
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let alice = DeviceIdentity::generate();
    let me_pub = me.public();
    let conv = crate::node::conversation::dm_conversation_id(&me_pub, &alice.public());
    let log_path = dir.path().join("me.log");
    let sent_path = dir.path().join("me-sent.log");

    // Alice's ratchet (to seal wire she sends us). Her sessions live in their own
    // temp dir — only the wire bytes matter to us.
    let alice_dir = dir.path().join("alice-sess");
    std::fs::create_dir_all(&alice_dir).unwrap();
    let mut alice_ratchet =
        DmRatchet::new(RatchetSessions::open(&alice_dir.join("ratchet.sessions"), "pw").unwrap());

    // Two prior-session DMs from Alice are durably in the log:
    //  - `recorded` was decrypted + written to the received store last session (surfaced).
    //  - `unrecorded` arrived durably but was NEVER recorded — e.g. Alice wasn't in the
    //    roster yet when it arrived (the DM-convergence race). It must be RETRIED after a
    //    restart, not silently dropped.
    let rec_wire = alice_ratchet
        .encrypt(
            &alice,
            &me_pub,
            &MessageBody::new(b"recorded".to_vec(), None).encode(),
        )
        .unwrap();
    let recorded = crate::eventlog::event::Event::new(
        &alice,
        conv,
        1,
        vec![],
        1,
        1000,
        EventKind::Message,
        rec_wire,
    );
    let recorded_id = recorded.id;
    let unrec_wire = alice_ratchet
        .encrypt(
            &alice,
            &me_pub,
            &MessageBody::new(b"unrecorded".to_vec(), None).encode(),
        )
        .unwrap();
    let unrecorded = crate::eventlog::event::Event::new(
        &alice,
        conv,
        2,
        vec![recorded_id],
        2,
        2000,
        EventKind::Message,
        unrec_wire,
    );
    {
        let mut log = crate::eventlog::persist::PersistentEventLog::open(&log_path, "pw").unwrap();
        log.append(recorded).unwrap();
        log.append(unrecorded).unwrap();
    }
    // Record ONLY `recorded` in the received store, as a real receipt last session would have.
    {
        let mut received =
            crate::node::received_log::ReceivedLog::open(&dir.path().join("received.log"), "pw")
                .unwrap();
        received
            .record(
                conv,
                alice.public().user_id(),
                1000,
                b"recorded",
                recorded_id,
            )
            .unwrap();
    }

    // Roster now knows Alice, so the previously-undecryptable `unrecorded` event can be retried.
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &Announce::new(&alice, "Alice", 4000),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &me_pub.user_id(),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(me, roster, tx, ch_tx, file_tx, &log_path, &sent_path, "pw").unwrap();

    node.emit_new_messages(conv);
    let mut texts: Vec<Vec<u8>> = Vec::new();
    while let Ok(dm) = rx.try_recv() {
        texts.push(dm.text);
    }
    assert!(
        !texts.iter().any(|t| t == b"recorded"),
        "already-recorded history must not be re-streamed after restart"
    );
    assert!(
        texts.iter().any(|t| t == b"unrecorded"),
        "a durably-received-but-unrecorded event must be retried after restart, not lost"
    );
}

#[tokio::test]
async fn reopen_retries_unsurfaced_file_manifest_after_restart() {
    // The file-path analogue of the message test: a FileManifest that arrived durably but was
    // never opened (author not yet in the roster — the convergence race) must be RETRIED after
    // restart, not silently lost; an already-surfaced manifest must NOT re-surface.
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let alice = DeviceIdentity::generate();
    let me_pub = me.public();
    let conv = crate::node::conversation::dm_conversation_id(&me_pub, &alice.public());
    let log_path = dir.path().join("me.log");
    let sent_path = dir.path().join("me-sent.log");

    let mk = |fc: u8| crate::file::FileManifest {
        name: "f.txt".into(),
        size: 3,
        mime: "text/plain".into(),
        checksum: [0u8; 32],
        file_key: [1u8; 32],
        file_conv: crate::eventlog::event::ConversationId::new([fc; 32]),
        chunk_count: 1,
    };
    // Two manifests from Alice (sealed to me), both durably in the log.
    let surfaced_m = mk(50);
    let surfaced_ev = crate::eventlog::event::Event::new(
        &alice,
        conv,
        1,
        vec![],
        1,
        1000,
        EventKind::FileManifest,
        crate::dm::seal(&alice, &me_pub.x25519_pub, &surfaced_m.encode()).unwrap(),
    );
    let surfaced_id = surfaced_ev.id;
    let unsurfaced_m = mk(51);
    let unsurfaced_ev = crate::eventlog::event::Event::new(
        &alice,
        conv,
        2,
        vec![surfaced_id],
        2,
        2000,
        EventKind::FileManifest,
        crate::dm::seal(&alice, &me_pub.x25519_pub, &unsurfaced_m.encode()).unwrap(),
    );
    {
        let mut log = crate::eventlog::persist::PersistentEventLog::open(&log_path, "pw").unwrap();
        log.append(surfaced_ev).unwrap();
        log.append(unsurfaced_ev).unwrap();
    }
    // Record ONLY `surfaced` durably, as a real surface last session would have.
    {
        let mut rf = crate::node::received_log::ReceivedLog::open(
            &dir.path().join("received_files.log"),
            "pw",
        )
        .unwrap();
        rf.record(
            surfaced_m.file_conv,
            alice.public().user_id(),
            1000,
            &surfaced_m.encode(),
            surfaced_id,
        )
        .unwrap();
    }

    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &Announce::new(&alice, "Alice", 4000),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &me_pub.user_id(),
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, mut file_rx) = mpsc::unbounded_channel();
    let node = Node::open(me, roster, tx, ch_tx, file_tx, &log_path, &sent_path, "pw").unwrap();

    node.process_file_events(conv);
    let mut convs: Vec<crate::eventlog::event::ConversationId> = Vec::new();
    while let Ok(rf) = file_rx.try_recv() {
        convs.push(rf.file_conv);
    }
    assert!(
        !convs.contains(&surfaced_m.file_conv),
        "an already-surfaced file manifest must not re-surface after restart"
    );
    assert!(
        convs.contains(&unsurfaced_m.file_conv),
        "a durably-received-but-unsurfaced file manifest must be retried after restart, not lost"
    );
}

#[tokio::test]
async fn two_nodes_exchange_a_channel_message_over_loopback_tcp() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let dir = tempfile::tempdir().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = Arc::new(Mutex::new(Roster::default()));

    let (alice_dm_tx, _alice_dm_rx) = mpsc::unbounded_channel();
    let (alice_ch_tx, _alice_ch_rx) = mpsc::unbounded_channel();
    let (alice_file_tx, _alice_file_rx) = mpsc::unbounded_channel();
    let (bob_dm_tx, _bob_dm_rx) = mpsc::unbounded_channel();
    let (bob_ch_tx, mut bob_ch_rx) = mpsc::unbounded_channel();
    let (bob_file_tx, _bob_file_rx) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        alice_dm_tx,
        alice_ch_tx,
        alice_file_tx,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        bob_dm_tx,
        bob_ch_tx,
        bob_file_tx,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();
    alice_node
        .send_channel_message(channel, b"hello channel")
        .await
        .unwrap();

    let got = tokio::time::timeout(std::time::Duration::from_secs(5), bob_ch_rx.recv())
        .await
        .expect("bob received a channel message within 5s")
        .expect("channel stream open");
    assert_eq!(got.text, b"hello channel");
    assert_eq!(got.from, alice_node.user_id());
    assert_eq!(got.channel_name, "general");

    // Alice's own channel_history shows her sent message — served from the sent
    // sidecar (the sender-key wire key is single-use and not self-decryptable).
    let hist = alice_node.channel_history(channel, 10);
    assert_eq!(hist.len(), 1);
    assert!(hist[0].from_me);
    assert_eq!(hist[0].text, b"hello channel");

    // Bob's channel_history shows Alice's message — served from his received store
    // (he decrypted it once via the sender-key ratchet; the key is now consumed).
    let bob_hist = bob_node.channel_history(channel, 10);
    assert_eq!(bob_hist.len(), 1);
    assert!(!bob_hist[0].from_me);
    assert_eq!(bob_hist[0].text, b"hello channel");
    assert_eq!(bob_hist[0].who, alice_node.user_id());
}

/// Bidirectional channel rig: Alice (creator) + Bob both run accept loops and know
/// each other. Alice sends -> Bob receives (already worked). THEN Bob sends -> Alice
/// receives + decrypts. The reverse direction was BROKEN before the fix: a
/// non-creator never distributed its own sender-key distribution, so the creator
/// had no `SenderChain` for it and silently dropped its messages.
#[tokio::test]
async fn two_nodes_exchange_channel_messages_both_directions_over_loopback_tcp() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    // Both nodes listen, so each can receive the other's pushed channel events.
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    // Each knows the other at its real listen port (so distribution can dial back).
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, mut a_ch_r) = mpsc::unbounded_channel();
    let (a_file, _a_file_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
    let (b_file, _b_file_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_file,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_file,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();

    // Forward direction: Alice -> Bob (already worked).
    alice_node
        .send_channel_message(channel, b"hi bob")
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
        .await
        .expect("bob received Alice's message within 5s")
        .expect("channel stream open");
    assert_eq!(got.text, b"hi bob");
    assert_eq!(got.from, alice_node.user_id());

    // Reverse direction: Bob -> Alice. This is the path the fix repairs. Bob is a
    // non-creator member; his send must first distribute his own SKD so Alice can
    // build his sender chain and decrypt.
    bob_node
        .send_channel_message(channel, b"hi alice")
        .await
        .unwrap();
    let back = tokio::time::timeout(Duration::from_secs(5), a_ch_r.recv())
        .await
        .expect("alice received Bob's message within 5s (broken before the fix)")
        .expect("channel stream open");
    assert_eq!(back.text, b"hi alice");
    assert_eq!(back.from, bob_node.user_id());

    // Alice's channel_history now shows both: her own sent line + Bob's decrypted one.
    let alice_hist = alice_node.channel_history(channel, 10);
    assert!(alice_hist.iter().any(|h| h.from_me && h.text == b"hi bob"));
    assert!(alice_hist
        .iter()
        .any(|h| !h.from_me && h.text == b"hi alice" && h.who == bob_node.user_id()));
}

/// Restart rig: Alice creates a {Alice, Bob} channel and sends a message Bob
/// decrypts. Alice's `Node` is then DROPPED and REOPENED from the SAME dir (same
/// log + derived stores + password). Alice sends ANOTHER message in the SAME epoch;
/// Bob must still decrypt it. This works only if Alice resumed her SAME sending
/// chain (persisted in `channel.senders`) rather than minting a fresh one: Bob is
/// first-wins on the chain he already holds, so a fresh chain would be undecryptable.
/// Without the persist+inject fix, the post-restart message is silently dropped.
#[tokio::test]
async fn channel_sending_survives_restart() {
    let alice = DeviceIdentity::generate();
    let (alice_ed, alice_x) = alice.secret_bytes(); // to reconstruct Alice on restart
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();
    // Separate sub-dirs so the two nodes' derived stores (ratchet.sessions,
    // channel.senders, ...) don't collide. Alice reopens from her SAME sub-dir.
    let alice_dir = dir.path().join("alice");
    let bob_dir = dir.path().join("bob");
    std::fs::create_dir_all(&alice_dir).unwrap();
    std::fs::create_dir_all(&bob_dir).unwrap();
    let alice_log = alice_dir.join("a.log");
    let alice_sent = alice_dir.join("a-sent.log");

    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    // Build Alice's post-restart roster now, before `bob` is moved into his node.
    let alice_roster2 = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_file, _a_file_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
    let (b_file, _b_file_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_file,
        &alice_log,
        &alice_sent,
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_file,
        &bob_dir.join("b.log"),
        &bob_dir.join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();
    alice_node
        .send_channel_message(channel, b"before restart")
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
        .await
        .expect("bob received the pre-restart message within 5s")
        .expect("channel stream open");
    assert_eq!(got.text, b"before restart");

    // RESTART Alice: drop her node, reopen from the SAME paths/password. Her
    // `my_sender` is rebuilt from `channel.senders`, not the (sender-key-less) log.
    drop(alice_node);
    let (a_dm2, _a_dm_r2) = mpsc::unbounded_channel();
    let (a_ch2, _a_ch_r2) = mpsc::unbounded_channel();
    let (a_file2, _a_file_r2) = mpsc::unbounded_channel();
    let alice2 = DeviceIdentity::from_secret_bytes(alice_ed, alice_x);
    let alice_node2 = Node::open(
        alice2,
        alice_roster2,
        a_dm2,
        a_ch2,
        a_file2,
        &alice_log,
        &alice_sent,
        "pw",
    )
    .unwrap();

    // Same-epoch send AFTER restart. Bob (still running, first-wins on Alice's
    // existing chain) must decrypt it — proving Alice resumed her SAME chain.
    alice_node2
        .send_channel_message(channel, b"after restart")
        .await
        .unwrap();
    let after = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
        .await
        .expect("bob received the post-restart message within 5s (broken without persist)")
        .expect("channel stream open");
    assert_eq!(after.text, b"after restart");
    assert_eq!(after.from, alice_node2.user_id());
}

/// Live add-member rig over loopback TCP: Alice creates a {Alice, Bob} channel and
/// they exchange an epoch-0 message. Alice then adds Carol (a third node, member of
/// nobody) via `add_channel_member`, bumping to epoch 1, and sends a new message.
/// Carol — who got the `MembershipChange` + Alice's lazily-distributed epoch-1 SKD —
/// receives + decrypts the epoch-1 message, but NOT the epoch-0 one she joined after
/// (late-join isolation / forward secrecy).
#[tokio::test]
async fn adding_a_channel_member_lets_them_receive_new_messages() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let carol = DeviceIdentity::generate();
    let bob_pub = bob.public();
    // A second copy of Bob's public for the later non-owner add-member attempt (the first
    // is moved into Alice's create_channel call).
    let bob_pub2 = bob.public();
    let carol_pub = carol.public();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let carol_uid = carol.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();
    let carol_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let carol_addr = carol_listener.local_addr().unwrap();

    // Alice knows Bob and Carol (so she can dial both to distribute). Bob and Carol
    // each know Alice (enough to receive her pushes).
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    add_peer(
        &alice_roster,
        &carol,
        "Carol",
        carol_addr.port(),
        &alice_uid,
    );
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);
    let carol_roster = seed_roster(&alice, "Alice", alice_addr.port(), &carol_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_file, _a_file_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
    let (b_file, _b_file_r) = mpsc::unbounded_channel();
    let (c_dm, _c_dm_r) = mpsc::unbounded_channel();
    let (c_ch, mut c_ch_r) = mpsc::unbounded_channel();
    let (c_file, _c_file_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_file,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_file,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();
    let carol_node = Node::open(
        carol,
        carol_roster,
        c_dm,
        c_ch,
        c_file,
        &dir.path().join("c.log"),
        &dir.path().join("c-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
    tokio::spawn(Arc::clone(&carol_node).run_accept_loop(carol_listener));

    // Epoch 0: {Alice, Bob}. Alice sends; Bob receives.
    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();
    alice_node
        .send_channel_message(channel, b"epoch0 msg")
        .await
        .unwrap();
    let got0 = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
        .await
        .expect("bob received the epoch-0 message within 5s")
        .expect("channel stream open");
    assert_eq!(got0.text, b"epoch0 msg");

    // Add Carol (epoch 1). She gets the MembershipChange via Alice's distribution.
    alice_node
        .add_channel_member(channel, carol_pub)
        .await
        .unwrap();

    // Epoch 1: Alice sends. Her lazy SKD for epoch 1 is distributed before the seal,
    // so both Bob (still a member) and Carol (newly joined) can read it.
    alice_node
        .send_channel_message(channel, b"epoch1 msg")
        .await
        .unwrap();

    let got_carol = tokio::time::timeout(Duration::from_secs(5), c_ch_r.recv())
        .await
        .expect("carol received the epoch-1 message within 5s")
        .expect("channel stream open");
    assert_eq!(got_carol.text, b"epoch1 msg");
    assert_eq!(got_carol.from, alice_node.user_id());

    // Late-join isolation: Carol never receives the epoch-0 message (she joined at
    // epoch 1). Her history holds only the epoch-1 message.
    let carol_hist = carol_node.channel_history(channel, 10);
    assert!(
        carol_hist.iter().all(|h| h.text != b"epoch0 msg"),
        "late joiner must not read pre-join messages"
    );
    assert!(carol_hist.iter().any(|h| h.text == b"epoch1 msg"));
    assert!(
        c_ch_r.try_recv().is_err(),
        "carol must not receive the pre-join epoch-0 message"
    );

    // Owner-only membership: Bob is a member but NOT the owner. His attempts to change
    // membership are refused locally (and would be ignored by every node anyway).
    let bob_remove = bob_node.remove_channel_member(channel, &carol_uid).await;
    assert!(
        matches!(bob_remove, Err(NodeError::Channel(_))),
        "a non-owner member cannot remove anyone"
    );
    let bob_add = bob_node.add_channel_member(channel, bob_pub2).await;
    assert!(
        matches!(bob_add, Err(NodeError::Channel(_))),
        "a non-owner member cannot add anyone"
    );
    // Carol is still a member of the channel on every node (Bob's kick was a no-op).
    for node in [&alice_node, &bob_node, &carol_node] {
        assert!(
            node.channel_members(channel)
                .iter()
                .any(|m| m.user_id() == carol_uid),
            "the victim must remain a member after a non-owner's kick attempt",
        );
    }
    // The owner reaches everyone via channel_owner: it is Alice on every node.
    for node in [&alice_node, &bob_node, &carol_node] {
        assert_eq!(node.channel_owner(channel), alice_uid);
    }

    // The OWNER can remove Carol and it converges (legit owner-authored change).
    alice_node
        .remove_channel_member(channel, &carol_uid)
        .await
        .unwrap();
    assert!(
        !alice_node
            .channel_members(channel)
            .iter()
            .any(|m| m.user_id() == carol_uid),
        "the owner can remove a member",
    );
}

/// Forward-secrecy rig: Alice + Bob channel; Alice sends two messages; Bob reads
/// both via the sender-key ratchet and `channel_history` serves them from his
/// store; re-feeding the events does NOT re-open them (single-use keys).
#[tokio::test]
async fn channel_messages_are_forward_secret_and_history_comes_from_stores() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let dir = tempfile::tempdir().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = Arc::new(Mutex::new(Roster::default()));

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_file, _a_file_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
    let (b_file, _b_file_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_file,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_file,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();
    alice_node
        .send_channel_message(channel, b"m0")
        .await
        .unwrap();
    alice_node
        .send_channel_message(channel, b"m1")
        .await
        .unwrap();

    // Bob receives both via the sender-key ratchet.
    let mut texts: Vec<Vec<u8>> = Vec::new();
    for _ in 0..2 {
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), b_ch_r.recv())
            .await
            .expect("bob received a channel message within 5s")
            .expect("channel stream open");
        texts.push(got.text);
    }
    texts.sort();
    assert_eq!(texts, vec![b"m0".to_vec(), b"m1".to_vec()]);

    // Bob's history is served from his received store (single-use keys consumed).
    let bob_hist = bob_node.channel_history(channel, 10);
    assert_eq!(bob_hist.len(), 2);
    assert!(bob_hist.iter().all(|e| !e.from_me));
    let mut htexts: Vec<Vec<u8>> = bob_hist.iter().map(|e| e.text.clone()).collect();
    htexts.sort();
    assert_eq!(htexts, vec![b"m0".to_vec(), b"m1".to_vec()]);

    // Re-feeding the same events does NOT re-open them (single-use ratchet keys);
    // history is unchanged and nothing new is streamed.
    bob_node.process_channel(channel);
    assert!(
        b_ch_r.try_recv().is_err(),
        "a consumed message was re-opened"
    );
    assert_eq!(bob_node.channel_history(channel, 10).len(), 2);
}

#[tokio::test]
async fn renaming_a_channel_propagates_to_members_and_is_owner_only() {
    // Alice (owner) renames the channel; the new name must converge on Bob (a member) —
    // the bug was that a rename never left the originating node. A non-owner's rename is
    // refused locally (every node would reject the event anyway).
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let dir = tempfile::tempdir().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = Arc::new(Mutex::new(Roster::default()));

    let (alice_dm_tx, _alice_dm_rx) = mpsc::unbounded_channel();
    let (alice_ch_tx, _alice_ch_rx) = mpsc::unbounded_channel();
    let (alice_file_tx, _alice_file_rx) = mpsc::unbounded_channel();
    let (bob_dm_tx, _bob_dm_rx) = mpsc::unbounded_channel();
    let (bob_ch_tx, mut bob_ch_rx) = mpsc::unbounded_channel();
    let (bob_file_tx, _bob_file_rx) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        alice_dm_tx,
        alice_ch_tx,
        alice_file_tx,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        bob_dm_tx,
        bob_ch_tx,
        bob_file_tx,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();
    alice_node
        .send_channel_message(channel, b"before rename")
        .await
        .unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), bob_ch_rx.recv())
        .await
        .expect("bob received the pre-rename message within 5s")
        .expect("channel stream open");
    assert_eq!(first.channel_name, "general");

    // A non-owner cannot rename — refused locally, with no epoch bump.
    let bob_rename = bob_node.rename_channel(channel, "hacked").await;
    assert!(
        matches!(bob_rename, Err(NodeError::Channel(_))),
        "a non-owner member cannot rename the channel"
    );

    // The owner renames; the change converges on Bob.
    alice_node.rename_channel(channel, "renamed").await.unwrap();
    // A message after the rename carries the new name — a deterministic barrier proving
    // Bob processed the rename's MembershipChange.
    alice_node
        .send_channel_message(channel, b"after rename")
        .await
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(5), bob_ch_rx.recv())
        .await
        .expect("bob received the post-rename message within 5s")
        .expect("channel stream open");
    assert_eq!(second.text, b"after rename");
    assert_eq!(second.channel_name, "renamed");

    // Both nodes now list the channel under its new name.
    for node in [&alice_node, &bob_node] {
        let summary = node
            .list_channels()
            .into_iter()
            .find(|c| c.id == channel)
            .expect("channel is listed");
        assert_eq!(summary.name, "renamed");
    }
}

#[tokio::test]
async fn list_channels_reports_created_channels() {
    let me = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        dm_tx,
        ch_tx,
        file_tx,
        &dir.path().join("m.log"),
        &dir.path().join("m-sent.log"),
        "pw",
    )
    .unwrap();

    assert!(node.list_channels().is_empty());
    let id = node.create_channel("general", vec![]).await.unwrap();
    let channels = node.list_channels();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].id, id);
    assert_eq!(channels[0].name, "general");
    assert_eq!(channels[0].member_count, 1); // just the creator
}

// A sent/received IMAGE is persisted to the durable chat-media store and is readable from
// THERE after the transient chunks are pruned AND after the node is reopened (durable). A
// generic (non-media) attachment creates NO media-store entry. This is the storage/display
// separation: media lives durably in the store; attachments stay chunk+manual-save only.
#[tokio::test]
async fn media_lands_in_durable_store_survives_prune_and_restart() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    // Capture Bob's secret bytes up front (the identity is moved into the node) so the restart
    // can reopen the SAME device against the same on-disk stores + media dir.
    let bob_secrets = bob.secret_bytes();
    let bob_roster_reopen = seed_roster(&alice, "Alice", 1, &bob.public().user_id());
    let dir = tempfile::tempdir().unwrap();
    // Per-account dirs (as production does under `accounts/<id>/`), so Alice's send-side media
    // store doesn't double as Bob's — the receiver must persist its OWN copy independently.
    let adir = dir.path().join("alice");
    let bdir = dir.path().join("bob");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::create_dir_all(&bdir).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
    let (b_f, mut b_f_r) = mpsc::unbounded_channel();

    // Bob's paths live under `bdir`, so reopening with the same paths re-points at the same
    // `<bdir>/media` store — proving durability across restart.
    let b_log = bdir.join("b.log");
    let b_sent = bdir.join("b-sent.log");

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &adir.join("a.log"),
        &adir.join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(bob, bob_roster, b_dm, b_ch, b_f, &b_log, &b_sent, "pw").unwrap();
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    // A multi-chunk IMAGE payload sent via the MEDIA button (kind drives media-store).
    let payload = vec![0x89u8; crate::file::CHUNK_SIZE + 4242];
    let src = dir.path().join("shot.png");
    std::fs::write(&src, &payload).unwrap();

    let bob_uid = bob_node.user_id();
    let file_conv = alice_node
        .send_file_dm(&bob_uid, &src, crate::file::FileKind::Media)
        .await
        .unwrap();

    // SENDER: the image was copied into Alice's media store on send, readable from there.
    assert!(
        alice_node.has_media(file_conv),
        "sender persists media on send"
    );
    assert_eq!(
        alice_node.read_media(file_conv).as_deref(),
        Some(&payload[..]),
        "sender's stored media matches the source bytes"
    );

    // RECEIVER: surface the manifest, then let the chunk sync + media persist converge.
    let rf = tokio::time::timeout(std::time::Duration::from_secs(5), b_f_r.recv())
        .await
        .expect("bob received the media file within 5s")
        .expect("file stream open");
    assert_eq!(rf.name, "shot.png");

    // The receive-complete persist runs from a background tick in the real runtime; here we
    // drive it directly (mirroring `pull_pending_files`) after the chunks have synced.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            // Pull any outstanding chunks from Alice, then try to persist the media.
            bob_node.pull_pending_files().await;
            if bob_node.persist_media_if_complete(file_conv) || bob_node.has_media(file_conv) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("bob persisted the media within 5s");

    assert!(bob_node.has_media(file_conv));
    assert_eq!(
        bob_node.read_media(file_conv).as_deref(),
        Some(&payload[..]),
        "receiver's stored media matches"
    );

    // The transient chunks were pruned once the media was stored — but the durable copy
    // still reads back (the whole point: display no longer depends on the chunks).
    assert_eq!(bob_node.file_progress(file_conv).map(|p| p.done), Some(0));
    assert_eq!(
        bob_node.read_media(file_conv).as_deref(),
        Some(&payload[..])
    );

    // A NON-MEDIA attachment must NOT create a media-store entry (storage-side separation).
    let doc = dir.path().join("notes.bin");
    std::fs::write(&doc, vec![0x11u8; 100]).unwrap();
    let doc_conv = alice_node
        .send_file_dm(&bob_uid, &doc, crate::file::FileKind::File)
        .await
        .unwrap();
    assert!(
        !alice_node.has_media(doc_conv),
        "a generic attachment is NOT written to the media store"
    );

    // RESTART: reopen Bob on the same dir — the media store is on disk, so the image still
    // reads back even though its chunks are gone and history was reloaded from the logs.
    drop(bob_node);
    let (b2_dm, _b2_dm_r) = mpsc::unbounded_channel();
    let (b2_ch, _b2_ch_r) = mpsc::unbounded_channel();
    let (b2_f, _b2_f_r) = mpsc::unbounded_channel();
    let bob_node2 = Node::open(
        DeviceIdentity::from_secret_bytes(bob_secrets.0, bob_secrets.1),
        bob_roster_reopen,
        b2_dm,
        b2_ch,
        b2_f,
        &b_log,
        &b_sent,
        "pw",
    )
    .unwrap();
    assert!(
        bob_node2.has_media(file_conv),
        "media survives a node restart"
    );
    assert_eq!(
        bob_node2.read_media(file_conv).as_deref(),
        Some(&payload[..]),
        "stored media still reads back after restart, with no chunks"
    );
}

#[tokio::test]
async fn two_nodes_transfer_a_file_over_loopback_tcp() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    // Capture both public identities before the device identities are moved into their
    // nodes (the history queries key the DM conversation by the counterparty's identity).
    let alice_public = alice.public();
    let bob_public = bob.public();
    let dir = tempfile::tempdir().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    // Bob knows Alice (to open her DM-sealed manifest); Alice knows Bob (to dial).
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
    let (b_f, mut b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    // A multi-chunk payload.
    let payload = vec![0xABu8; crate::file::CHUNK_SIZE + 1234];
    let src = dir.path().join("photo.bin");
    std::fs::write(&src, &payload).unwrap();

    let bob_uid = bob_node.user_id();
    let file_conv = alice_node
        .send_file_dm(&bob_uid, &src, crate::file::FileKind::File)
        .await
        .unwrap();

    // The sender sees her own sent file as a first-class history message immediately:
    // a `from_me` file entry carrying the manifest metadata (name/size/file_conv).
    let alice_hist = alice_node.dm_history(&bob_public, 10);
    let alice_file = alice_hist
        .iter()
        .find(|h| h.file.is_some())
        .expect("sender's file message is in her own history");
    assert!(alice_file.from_me);
    let af = alice_file.file.as_ref().unwrap();
    assert_eq!(af.name, "photo.bin");
    assert_eq!(af.size, payload.len() as u64);
    assert_eq!(af.file_conv, file_conv);

    // Bob surfaces the received file.
    let rf = tokio::time::timeout(std::time::Duration::from_secs(5), b_f_r.recv())
        .await
        .expect("bob received a file within 5s")
        .expect("file stream open");
    assert_eq!(rf.name, "photo.bin");
    assert_eq!(rf.size, payload.len() as u64);
    assert_eq!(rf.file_conv, file_conv);
    assert_eq!(rf.from, alice_node.user_id());

    // And the received file appears in Bob's conversation history (not just the tray) as
    // a non-`from_me` file message — so it persists across reload.
    let bob_hist = bob_node.dm_history(&alice_public, 10);
    let bob_file = bob_hist
        .iter()
        .find(|h| h.file.is_some())
        .expect("received file message is in bob's history");
    assert!(!bob_file.from_me);
    assert_eq!(bob_file.file.as_ref().unwrap().file_conv, file_conv);

    // Bob saves it and the bytes match. Retry briefly: the file_conv chunk
    // sync runs in a separate task and may land just after the manifest
    // notification fires (separate TCP connection, same loopback).
    let dest = dir.path().join("saved.bin");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bob_node.save_file(rf.file_conv, &dest) {
                Ok(()) => break,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("bob saved the file within 5s");
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
}

// Regression: when the sender's one-shot chunk push misses (the recipient wasn't reachable
// at send time — the discovery race) and there is NO post office, the recipient still gets
// the manifest but 0 chunks. It must then PULL the chunks directly from peers and converge.
#[tokio::test]
async fn recipient_pulls_file_chunks_directly_when_push_missed() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let alice_public = alice.public();
    let bob_public = bob.public();
    let dir = tempfile::tempdir().unwrap();

    // ALICE listens — Bob will pull from her. Alice's roster has Bob at a DEAD port, so her
    // send-time push to Bob misses (simulating the race); the file stays only in Alice's log.
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let alice_roster = seed_roster(&bob, "Bob", 1, &alice.public().user_id());
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob.public().user_id());

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
    let (b_f, _b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();
    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));

    let payload = vec![0xCDu8; crate::file::CHUNK_SIZE + 999];
    let src = dir.path().join("doc.bin");
    std::fs::write(&src, &payload).unwrap();
    let bob_uid = bob_node.user_id();
    // Alice sends; her push to Bob (dead port) misses, but the file is now in Alice's log.
    let file_conv = alice_node
        .send_file_dm(&bob_uid, &src, crate::file::FileKind::File)
        .await
        .unwrap();

    // Bob gets the MANIFEST by syncing the DM conversation from Alice (the reliable path:
    // the DM conv is one Bob pulls), then surfaces it — which queues a pending file pull.
    let dm_conv = dm_conversation_id(&alice_public, &bob_public);
    let alice_peer = bob_node
        .roster
        .lock()
        .unwrap()
        .peers()
        .into_iter()
        .find(|p| p.public.user_id() == alice_public.user_id())
        .unwrap();
    bob_node.deliver_direct(&alice_peer, dm_conv).await.unwrap();
    bob_node.process_file_events(dm_conv);

    // Manifest known, but the chunks are NOT here (the push missed): strictly incomplete.
    let prog = bob_node.file_progress(file_conv).expect("manifest known");
    assert!(
        prog.done < prog.total,
        "chunks must be missing before the direct pull ({}/{})",
        prog.done,
        prog.total
    );

    // The fix: the recipient pulls the chunks DIRECTLY from peers (no post office) and saves.
    let dest = dir.path().join("got.bin");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            bob_node.pull_pending_files().await;
            if bob_node.save_file(file_conv, &dest).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("bob pulled the chunks directly + saved within 5s");
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
}

#[tokio::test]
async fn two_nodes_transfer_a_multi_chunk_file_over_loopback_tcp() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());

    let (a_dm, _a) = mpsc::unbounded_channel();
    let (a_ch, _b) = mpsc::unbounded_channel();
    let (a_f, _c) = mpsc::unbounded_channel();
    let (b_dm, _d) = mpsc::unbounded_channel();
    let (b_ch, _e) = mpsc::unbounded_channel();
    let (b_f, mut b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    // ~5 chunks → the file_conv batch far exceeds one frame → multiple rounds.
    let payload = vec![0x5Au8; crate::file::CHUNK_SIZE * 4 + 999];
    let src = dir.path().join("big.bin");
    std::fs::write(&src, &payload).unwrap();

    let bob_uid = bob_node.user_id();
    let file_conv = alice_node
        .send_file_dm(&bob_uid, &src, crate::file::FileKind::File)
        .await
        .unwrap();

    let rf = tokio::time::timeout(std::time::Duration::from_secs(10), b_f_r.recv())
        .await
        .expect("received within 10s")
        .expect("stream open");
    assert_eq!(rf.size, payload.len() as u64);

    // Saving may need a brief retry while the file_conv chunks finish syncing.
    let dest = dir.path().join("big-saved.bin");
    let mut saved = false;
    for _ in 0..100 {
        if bob_node.save_file(rf.file_conv, &dest).is_ok() {
            saved = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(saved, "bob saved the multi-chunk file");
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    let _ = file_conv;
}

#[tokio::test]
async fn a_dm_reaction_is_visible_to_both_peers() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();

    // Capture public identities BEFORE moving into Node::open.
    let alice_pub = alice.public();
    let bob_pub = bob.public();
    let alice_uid = alice_pub.user_id();
    let bob_uid = bob_pub.user_id();

    let dir = tempfile::tempdir().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_pub.user_id());
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_pub.user_id());

    let (a_dm, _a1) = mpsc::unbounded_channel();
    let (a_ch, _a2) = mpsc::unbounded_channel();
    let (a_f, _a3) = mpsc::unbounded_channel();
    let (b_dm, mut b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b2) = mpsc::unbounded_channel();
    let (b_f, _b3) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));
    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));

    // Alice DMs Bob; Bob receives it and learns its event id from history.
    alice_node.send_dm(&bob_uid, b"hi bob").await.unwrap();
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), b_dm_r.recv())
        .await
        .expect("bob got the dm")
        .expect("stream open");
    assert_eq!(got.text, b"hi bob");

    // Derive the DM conversation id from the two public identities.
    let conv = dm_conversation_id(&alice_pub, &bob_pub);

    let target = {
        let h = bob_node.dm_history(&alice_pub, 10);
        h.iter()
            .find(|e| !e.from_me)
            .map(|e| e.id)
            .expect("bob has the message id")
    };

    // Bob reacts; it distributes to Alice.
    bob_node
        .react_dm(&alice_uid, target, "👍", false)
        .await
        .unwrap();

    // Bob sees his own reaction (from my_dm_reactions).
    let bob_views = bob_node.reactions(conv);
    assert!(
        bob_views
            .iter()
            .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)),
        "bob sees his own reaction"
    );

    // Give the distribution a moment, then check Alice.
    let mut ok = false;
    for _ in 0..50 {
        let av = alice_node.reactions(conv);
        if av
            .iter()
            .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid))
        {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(ok, "alice sees bob's reaction");

    // Regression (FIX 4): toggling the same reaction many times must NOT grow
    // `my_dm_reactions` without bound — it collapses to ONE entry per
    // (conv, target, emoji), resolved by recency. Spam add/remove on the same
    // emoji, then a second emoji.
    for i in 0..50 {
        bob_node
            .react_dm(&alice_uid, target, "👍", i % 2 == 0)
            .await
            .unwrap();
    }
    bob_node
        .react_dm(&alice_uid, target, "🎉", false)
        .await
        .unwrap();
    {
        let map = bob_node
            .my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned");
        // Two distinct (conv, target, emoji) keys only — not 50+ pushed entries.
        assert_eq!(
            map.len(),
            2,
            "own reactions collapse to one per (conv,target,emoji)"
        );
    }
    // Aggregation still works on the collapsed map: 🎉 (last action add) is on.
    let bob_views = bob_node.reactions(conv);
    assert!(
        bob_views
            .iter()
            .any(|v| v.emoji == "🎉" && v.who.contains(&bob_uid)),
        "🎉 is on"
    );
}

/// Channel file transfer over the wire, both directions. The manifest is sealed
/// with the sender-key ratchet (`seal_sender_message`) and opened by
/// `process_file_events`; the chunk conversation syncs separately. Bob is a
/// non-creator member, so his send must first distribute his own sender key —
/// the same path the creator-only-send fix repaired for messages.
#[tokio::test]
async fn two_nodes_transfer_a_file_in_a_channel() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    // Both nodes listen so each can receive the other's pushed file events.
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, mut a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
    let (b_f, mut b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();

    // Forward direction: Alice -> Bob. Multi-chunk payload.
    let payload = vec![0xABu8; crate::file::CHUNK_SIZE + 1234];
    let src = dir.path().join("photo.bin");
    std::fs::write(&src, &payload).unwrap();
    let file_conv = alice_node
        .send_file_channel(channel, &src, crate::file::FileKind::File)
        .await
        .unwrap();

    let rf = tokio::time::timeout(Duration::from_secs(5), b_f_r.recv())
        .await
        .expect("bob received the channel file within 5s")
        .expect("file stream open");
    assert_eq!(rf.name, "photo.bin");
    assert_eq!(rf.size, payload.len() as u64);
    assert_eq!(rf.conv, channel);
    assert_eq!(rf.file_conv, file_conv);
    assert_eq!(rf.from, alice_node.user_id());

    // The file is a first-class message in the channel history for BOTH the sender (her
    // own, `from_me`) and the receiver (Bob's, not `from_me`) — not just the tray.
    let alice_file = alice_node
        .channel_history(channel, 10)
        .into_iter()
        .find(|h| h.file.is_some())
        .expect("sender sees her channel file in history");
    assert!(alice_file.from_me);
    assert_eq!(alice_file.file.as_ref().unwrap().file_conv, file_conv);
    let bob_file = bob_node
        .channel_history(channel, 10)
        .into_iter()
        .find(|h| h.file.is_some())
        .expect("receiver sees the channel file in history");
    assert!(!bob_file.from_me);
    assert_eq!(bob_file.file.as_ref().unwrap().file_conv, file_conv);

    let dest = dir.path().join("saved.bin");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match bob_node.save_file(rf.file_conv, &dest) {
                Ok(()) => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("bob saved the channel file within 5s");
    assert_eq!(std::fs::read(&dest).unwrap(), payload);

    // Reverse direction: Bob -> Alice. Bob is a non-creator member; his
    // send_file_channel must distribute his own sender key first so Alice can
    // open the manifest (mirrors the bidirectional message fix).
    let payload2 = vec![0xCDu8; crate::file::CHUNK_SIZE * 2 + 77];
    let src2 = dir.path().join("from-bob.bin");
    std::fs::write(&src2, &payload2).unwrap();
    let file_conv2 = bob_node
        .send_file_channel(channel, &src2, crate::file::FileKind::File)
        .await
        .unwrap();

    let rf2 = tokio::time::timeout(Duration::from_secs(5), a_f_r.recv())
        .await
        .expect("alice received Bob's channel file within 5s (non-creator send)")
        .expect("file stream open");
    assert_eq!(rf2.name, "from-bob.bin");
    assert_eq!(rf2.size, payload2.len() as u64);
    assert_eq!(rf2.conv, channel);
    assert_eq!(rf2.file_conv, file_conv2);
    assert_eq!(rf2.from, bob_node.user_id());

    let dest2 = dir.path().join("alice-saved.bin");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match alice_node.save_file(rf2.file_conv, &dest2) {
                Ok(()) => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("alice saved Bob's channel file within 5s");
    assert_eq!(std::fs::read(&dest2).unwrap(), payload2);
}

/// Channel reactions over the wire. They use a per-member `SealedPayload`
/// (re-readable, not single-use), so both the reacting author and the other
/// members must see them via `reactions(channel)`. Also covers toggle-off.
#[tokio::test]
async fn channel_reactions_are_visible_to_members() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
    let (b_f, _b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();

    // Alice sends a channel message; Bob receives it and learns its event id.
    alice_node
        .send_channel_message(channel, b"hi bob")
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
        .await
        .expect("bob received Alice's channel message within 5s")
        .expect("channel stream open");
    assert_eq!(got.text, b"hi bob");
    let target = got.event_id;

    // Bob reacts; it distributes to Alice.
    bob_node
        .react_channel(channel, target, "👍", false)
        .await
        .unwrap();

    // Bob re-reads his own channel reaction (sealed per-member, re-readable).
    let bob_views = bob_node.reactions(channel);
    assert!(
        bob_views
            .iter()
            .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)),
        "bob sees his own channel reaction"
    );

    // Alice sees Bob's reaction after distribution lands.
    let mut ok = false;
    for _ in 0..50 {
        let av = alice_node.reactions(channel);
        if av
            .iter()
            .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid))
        {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ok, "alice sees bob's channel reaction");
    let _ = alice_uid;

    // Toggle off: Bob removes the reaction; it disappears for both.
    bob_node
        .react_channel(channel, target, "👍", true)
        .await
        .unwrap();

    let bob_after = bob_node.reactions(channel);
    assert!(
        !bob_after
            .iter()
            .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)),
        "bob's reaction is gone after toggle-off"
    );

    let mut gone = false;
    for _ in 0..50 {
        let av = alice_node.reactions(channel);
        if !av
            .iter()
            .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid))
        {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gone, "alice no longer sees bob's reaction after toggle-off");
}

/// Two-node channel recall over the wire:
///  1. A member who recalls their OWN message is tombstoned for every other member — the
///     read path that opens the recaller's sealed `Delete` using their x25519 (from the
///     member set) and authorises it against the target's author.
///  2. A member CANNOT recall someone else's message (send-side author-only guard).
///  3. After the recaller is removed from the channel, the tombstone STILL holds — exercising
///     the roster fallback for the recaller's key (the member set no longer has them).
#[tokio::test]
async fn channel_recall_by_member_is_visible_and_survives_removal() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_pub = bob.public();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, mut a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
    let (b_f, _b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub])
        .await
        .unwrap();

    // Alice sends so Bob syncs into the channel; Bob learns Alice's message id.
    alice_node
        .send_channel_message(channel, b"hi bob")
        .await
        .unwrap();
    let alice_msg = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
        .await
        .expect("bob received Alice's channel message within 5s")
        .expect("channel stream open");
    assert_eq!(alice_msg.text, b"hi bob");
    let alice_msg_id = alice_msg.event_id;

    // (2) A member cannot recall a message they did not author.
    assert!(
        bob_node
            .recall_channel(channel, alice_msg_id)
            .await
            .is_err(),
        "bob must not be able to recall Alice's message"
    );

    // Bob sends his own message; Alice receives it and learns its id.
    bob_node
        .send_channel_message(channel, b"from bob")
        .await
        .unwrap();
    let bob_msg = tokio::time::timeout(Duration::from_secs(5), a_ch_r.recv())
        .await
        .expect("alice received Bob's channel message within 5s")
        .expect("channel stream open");
    assert_eq!(bob_msg.text, b"from bob");
    let bob_msg_id = bob_msg.event_id;

    // Bob recalls his own message; it distributes to Alice.
    bob_node.recall_channel(channel, bob_msg_id).await.unwrap();

    let tombstoned = |node: &Arc<Node>| {
        node.channel_history(channel, 20)
            .into_iter()
            .any(|e| e.id == bob_msg_id && e.recalled)
    };

    // (1) Alice sees Bob's recall while Bob is still a member.
    let mut ok = false;
    for _ in 0..50 {
        if tombstoned(&alice_node) {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ok, "alice sees Bob's recall while Bob is a member");

    // (3) Remove Bob from the channel; his recall must STILL apply — the key now comes from
    // the discovery roster, not the reduced member set.
    alice_node
        .remove_channel_member(channel, &bob_uid)
        .await
        .unwrap();
    assert!(
        tombstoned(&alice_node),
        "Bob's recall survives his removal from the channel (roster fallback for his key)"
    );
}

#[tokio::test]
async fn search_finds_a_sent_dm_by_keyword() {
    let me = DeviceIdentity::generate();
    let peer = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();
    let roster = seed_roster(&peer, "Peer", 1, &me.public().user_id());
    let (dm, _a) = mpsc::unbounded_channel();
    let (ch, _b) = mpsc::unbounded_channel();
    let (f, _c) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        dm,
        ch,
        f,
        &dir.path().join("m.log"),
        &dir.path().join("m-sent.log"),
        "pw",
    )
    .unwrap();

    // No peer is reachable (port 1), but send_dm still records to the sidecar.
    let peer_uid = peer.public().user_id();
    let _ = node.send_dm(&peer_uid, b"lunch at noon tomorrow").await; // delivery fails; sidecar written
    let hits = node.search("NOON");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].from_me);
    assert_eq!(hits[0].label, "Peer");
    assert_eq!(hits[0].target, peer_uid);
    assert!(node.search("   ").is_empty());
    assert!(node.search("absent-keyword").is_empty());
}

#[tokio::test]
async fn a_reply_round_trips_with_its_reply_to() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();

    // Capture public identities BEFORE moving into Node::open.
    let alice_pub = alice.public();
    let bob_pub = bob.public();
    let alice_uid = alice_pub.user_id();
    let bob_uid = bob_pub.user_id();

    let dir = tempfile::tempdir().unwrap();

    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();

    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_pub.user_id());
    let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_pub.user_id());

    let (a_dm, mut a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch) = mpsc::unbounded_channel();
    let (a_f, _a_f) = mpsc::unbounded_channel();
    let (b_dm, mut b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch) = mpsc::unbounded_channel();
    let (b_f, _b_f) = mpsc::unbounded_channel();

    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));

    // Alice sends a normal DM to Bob (no reply_to).
    alice_node
        .send_dm(&bob_uid, b"hi")
        .await
        .expect("alice sends hi");

    // Bob receives it.
    let got = tokio::time::timeout(Duration::from_secs(5), b_dm_r.recv())
        .await
        .expect("bob got alice's dm within 5s")
        .expect("stream open");
    assert_eq!(got.text, b"hi");
    assert_eq!(got.reply_to, None, "normal message has no reply_to");

    // Bob looks up the parent message id from his history.
    let parent_id = bob_node
        .dm_history(&alice_pub, 10)
        .into_iter()
        .find(|e| !e.from_me)
        .map(|e| e.id)
        .expect("bob has alice's message in history");

    // Bob replies to Alice's message.
    bob_node
        .send_dm_reply(&alice_uid, b"hey back", Some(parent_id))
        .await
        .expect("bob sends reply");

    // Alice receives the reply with reply_to set.
    let reply = tokio::time::timeout(Duration::from_secs(5), a_dm_r.recv())
        .await
        .expect("alice got bob's reply within 5s")
        .expect("stream open");
    assert_eq!(reply.text, b"hey back");
    assert_eq!(
        reply.reply_to,
        Some(parent_id),
        "alice's ReceivedDm carries reply_to"
    );

    // Alice's dm_history shows the reply entry with reply_to.
    let alice_hist = alice_node.dm_history(&bob_pub, 10);
    let reply_entry = alice_hist
        .iter()
        .find(|e| !e.from_me)
        .expect("alice has bob's reply in history");
    assert_eq!(reply_entry.text, b"hey back");
    assert_eq!(
        reply_entry.reply_to,
        Some(parent_id),
        "history entry carries reply_to"
    );

    // A normal send_dm (no reply) yields reply_to == None in history.
    alice_node.send_dm(&bob_uid, b"just a message").await.ok();
    let alice_hist2 = alice_node.dm_history(&bob_pub, 10);
    let normal_sent = alice_hist2
        .iter()
        .find(|e| e.from_me && e.text == b"just a message")
        .expect("alice's normal sent message is in history");
    assert_eq!(
        normal_sent.reply_to, None,
        "normal send_dm yields reply_to == None"
    );
}

#[tokio::test]
async fn dm_messages_are_forward_secret_over_the_ratchet() {
    // Two nodes over loopback TCP. Alice sends DMs sealed with the Double
    // Ratchet; Bob decrypts them once into his received store. Proves: (a) both
    // land in Bob's dm_history; (b) re-feeding the SAME wire event to
    // emit_new_messages does NOT re-surface it (the wire key is single-use); and
    // (c) out-of-order arrival still surfaces every message.
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let alice_pub = alice.public();
    let bob_pub = bob.public();
    let alice_uid = alice_pub.user_id();
    let bob_uid = bob_pub.user_id();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", 4000, &bob_uid);

    let dir = tempfile::tempdir().unwrap();
    let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
    let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
    let (a_ch_tx, _a_ch_rx) = mpsc::unbounded_channel();
    let (b_ch_tx, _b_ch_rx) = mpsc::unbounded_channel();
    let (a_file_tx, _a_file_rx) = mpsc::unbounded_channel();
    let (b_file_tx, _b_file_rx) = mpsc::unbounded_channel();
    let alice_node = Node::open(
        alice,
        alice_roster,
        alice_tx,
        a_ch_tx,
        a_file_tx,
        &dir.path().join("alice.log"),
        &dir.path().join("alice-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open(
        bob,
        bob_roster,
        bob_tx,
        b_ch_tx,
        b_file_tx,
        &dir.path().join("bob.log"),
        &dir.path().join("bob-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    // Alice sends two DMs; Bob receives both via the ratchet.
    alice_node.send_dm(&bob_uid, b"first").await.unwrap();
    let r1 = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
        .await
        .expect("bob got the first dm")
        .expect("stream open");
    assert_eq!(r1.text, b"first");
    assert_eq!(r1.from, alice_uid);
    alice_node.send_dm(&bob_uid, b"second").await.unwrap();
    let r2 = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
        .await
        .expect("bob got the second dm")
        .expect("stream open");
    assert_eq!(r2.text, b"second");

    // (a) Bob's dm_history shows both (served from his received store).
    let conv = dm_conversation_id(&alice_pub, &bob_pub);
    let hist = bob_node.dm_history(&alice_pub, 10);
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].text, b"first");
    assert_eq!(hist[1].text, b"second");

    // (b) Re-feeding the same wire events does NOT re-surface them: the ratchet
    // keys were consumed (single-use) AND they're marked emitted. Draining again
    // yields nothing new.
    bob_node.emit_new_messages(conv);
    assert!(
        bob_rx.try_recv().is_err(),
        "consumed-key events must not re-surface"
    );
    assert_eq!(
        bob_node.dm_history(&alice_pub, 10).len(),
        2,
        "history unchanged after re-feed"
    );

    // (c) Out-of-order: Alice seals m0,m1,m2 and Bob is fed them as 2,0,1. The
    // ratchet opens skipped messages, so all three surface. Use a fresh peer pair
    // so this exercises a clean session.
    let carol = DeviceIdentity::generate();
    let dave = DeviceIdentity::generate();
    let carol_pub = carol.public();
    let dave_pub = dave.public();
    let dave_uid = dave_pub.user_id();
    let conv2 = dm_conversation_id(&carol_pub, &dave_pub);

    // Carol's ratchet (seals the wire she "sends"); only the bytes matter.
    let carol_sess_dir = dir.path().join("carol-sess");
    std::fs::create_dir_all(&carol_sess_dir).unwrap();
    let mut carol_ratchet = DmRatchet::new(
        RatchetSessions::open(&carol_sess_dir.join("ratchet.sessions"), "pw").unwrap(),
    );
    let mk = |r: &mut DmRatchet, body: &[u8], seq: u64, wc: u64| {
        let wire = r
            .encrypt(
                &carol,
                &dave_pub,
                &MessageBody::new(body.to_vec(), None).encode(),
            )
            .unwrap();
        Event::new(
            &carol,
            conv2,
            seq,
            vec![],
            seq,
            wc,
            EventKind::Message,
            wire,
        )
    };
    let m0 = mk(&mut carol_ratchet, b"m0", 1, 1000);
    let m1 = mk(&mut carol_ratchet, b"m1", 2, 2000);
    let m2 = mk(&mut carol_ratchet, b"m2", 3, 3000);

    // Dave's node knows Carol.
    let dave_roster = seed_roster(&carol, "Carol", 4000, &dave_uid);
    let (dave_tx, mut dave_rx) = mpsc::unbounded_channel();
    let (d_ch_tx, _d_ch_rx) = mpsc::unbounded_channel();
    let (d_file_tx, _d_file_rx) = mpsc::unbounded_channel();
    let dave_node = Node::open(
        dave,
        dave_roster,
        dave_tx,
        d_ch_tx,
        d_file_tx,
        &dir.path().join("dave.log"),
        &dir.path().join("dave-sent.log"),
        "pw",
    )
    .unwrap();

    // Feed them in arrival order 2,0,1 into Dave's log, draining after each.
    for ev in [m2, m0, m1] {
        dave_node.log.lock().unwrap().append(ev).unwrap();
        dave_node.emit_new_messages(conv2);
    }
    let mut got: Vec<Vec<u8>> = Vec::new();
    while let Ok(dm) = dave_rx.try_recv() {
        got.push(dm.text);
    }
    got.sort();
    assert_eq!(
        got,
        vec![b"m0".to_vec(), b"m1".to_vec(), b"m2".to_vec()],
        "out-of-order delivery surfaces all three messages"
    );
    // History (sorted by wall_clock) shows them in send order.
    let dhist = dave_node.dm_history(&carol_pub, 10);
    assert_eq!(
        dhist.iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
        vec![b"m0".to_vec(), b"m1".to_vec(), b"m2".to_vec()]
    );
}

/// Seed `roster` with `peer` advertised under `account`, reachable at `port`.
fn add_account_peer(
    roster: &Arc<Mutex<Roster>>,
    peer: &DeviceIdentity,
    account: &crate::identity::account::Account,
    name: &str,
    port: u16,
    self_user_id: &str,
) {
    roster.lock().unwrap().update(
        &Announce::new_with_account(peer, account, name, port),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        self_user_id,
    );
}

#[tokio::test]
async fn send_to_account_delivers_to_a_peer_account_over_loopback() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &alice_roster,
        &bob,
        &bob_acct,
        "Bob",
        bob_addr.port(),
        &alice_uid,
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(&bob_roster, &alice, &alice_acct, "Alice", 4000, &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
    let (b_f, _b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open_with_account(
        alice,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_acct.secret_bytes()),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    alice_node
        .send_to_account(&bob_acct.account_id(), b"hi account", None)
        .await
        .unwrap();

    // Bob's account history (keyed by Alice's account) shows the message.
    let mut bob_hist = Vec::new();
    for _ in 0..50 {
        bob_hist = bob_node.account_history(&alice_acct.account_id(), 10);
        if !bob_hist.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        bob_hist.len(),
        1,
        "bob received the account-addressed message"
    );
    assert!(!bob_hist[0].from_me);
    assert_eq!(bob_hist[0].text, b"hi account");

    // Alice's own account history shows it once, from her.
    let a_hist = alice_node.account_history(&bob_acct.account_id(), 10);
    assert_eq!(a_hist.len(), 1);
    assert!(a_hist[0].from_me);
    assert_eq!(a_hist[0].text, b"hi account");
}

/// Two-node ACCOUNT recall over the wire: the sender recalls their own account message and
/// the RECEIVER tombstones it — exercising recalled_targets_account's peer path (decrypt the
/// RecallEnvelope, bind the claimed sender account to the authenticating peer, authorise
/// against the target's author). Also: a peer cannot recall a message that isn't theirs.
#[tokio::test]
async fn account_recall_over_the_wire_tombstones_for_the_receiver() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &alice_roster,
        &bob,
        &bob_acct,
        "Bob",
        bob_addr.port(),
        &alice_uid,
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &bob_roster,
        &alice,
        &alice_acct,
        "Alice",
        alice_addr.port(),
        &bob_uid,
    );

    let (a_dm, _a) = mpsc::unbounded_channel();
    let (a_ch, _b) = mpsc::unbounded_channel();
    let (a_f, _c) = mpsc::unbounded_channel();
    let (b_dm, _d) = mpsc::unbounded_channel();
    let (b_ch, _e) = mpsc::unbounded_channel();
    let (b_f, _g) = mpsc::unbounded_channel();

    let alice_node = Node::open_with_account(
        alice,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_acct.secret_bytes()),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    // Bob sends an account message to Alice; its logical id is in Bob's own history at once.
    bob_node
        .send_to_account(&alice_acct.account_id(), b"recall me", None)
        .await
        .unwrap();
    let bob_msg_id = {
        let h = bob_node.account_history(&alice_acct.account_id(), 10);
        assert_eq!(h.len(), 1);
        h[0].id
    };

    // Alice receives it.
    let mut received = false;
    for _ in 0..50 {
        let h = alice_node.account_history(&bob_acct.account_id(), 10);
        if h.iter()
            .any(|e| e.id == bob_msg_id && !e.recalled && e.text == b"recall me")
        {
            received = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(received, "alice received Bob's account message");

    // A peer cannot recall a message that isn't their own (Alice didn't author it).
    assert!(
        alice_node
            .recall_account(&bob_acct.account_id(), bob_msg_id)
            .await
            .is_err(),
        "alice cannot recall a message she didn't send"
    );

    // Bob recalls his own message; Alice tombstones it (recalled_targets_account peer path).
    bob_node
        .recall_account(&alice_acct.account_id(), bob_msg_id)
        .await
        .unwrap();
    let mut tombstoned = false;
    for _ in 0..50 {
        let h = alice_node.account_history(&bob_acct.account_id(), 10);
        if h.iter().any(|e| e.id == bob_msg_id && e.recalled) {
            tombstoned = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        tombstoned,
        "alice tombstones Bob's recalled account message (receiver-side peer recall path)"
    );
}

// Regression: sending a FILE to an account (the UI's DM file-send path) must show up in
// the SENDER's own account history as a from_me file message — otherwise the chat shows
// no record of the file at all.
#[tokio::test]
async fn send_file_to_account_shows_in_sender_account_history() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &alice_roster,
        &bob,
        &bob_acct,
        "Bob",
        bob_addr.port(),
        &alice_uid,
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(&bob_roster, &alice, &alice_acct, "Alice", 4000, &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
    let (b_f, _b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open_with_account(
        alice,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_acct.secret_bytes()),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let file_path = dir.path().join("hello.txt");
    std::fs::write(&file_path, b"hello file contents").unwrap();
    alice_node
        .send_file_to_account(
            &bob_acct.account_id(),
            &file_path,
            crate::file::FileKind::File,
        )
        .await
        .unwrap();

    // Alice's own account history shows the file she sent, once, as a from_me file message.
    let a_hist = alice_node.account_history(&bob_acct.account_id(), 10);
    let files: Vec<_> = a_hist.iter().filter(|e| e.file.is_some()).collect();
    assert_eq!(files.len(), 1, "sender sees exactly one file record");
    assert!(files[0].from_me, "the sent file shows as ours");
    assert_eq!(files[0].file.as_ref().unwrap().name, "hello.txt");
}

#[tokio::test]
async fn send_to_account_self_syncs_to_own_other_device() {
    use crate::identity::account::Account;
    // Alice has TWO devices sharing one account; Bob is a separate account.
    let a1 = DeviceIdentity::generate();
    let a2 = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let a1_uid = a1.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let a2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a2_addr = a2_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    // A1 knows its sibling A2 (same account) and Bob (other account).
    let a1_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(&a1_roster, &a2, &alice_acct, "A2", a2_addr.port(), &a1_uid);
    add_account_peer(&a1_roster, &bob, &bob_acct, "Bob", bob_addr.port(), &a1_uid);
    let a2_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &a2_roster,
        &a1,
        &alice_acct,
        "A1",
        4000,
        &a2.public().user_id(),
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &bob_roster,
        &a1,
        &alice_acct,
        "A1",
        4000,
        &bob.public().user_id(),
    );

    let (a1_dm, _x1) = mpsc::unbounded_channel();
    let (a1_ch, _x2) = mpsc::unbounded_channel();
    let (a1_f, _x3) = mpsc::unbounded_channel();
    let (a2_dm, _x4) = mpsc::unbounded_channel();
    let (a2_ch, _x5) = mpsc::unbounded_channel();
    let (a2_f, _x6) = mpsc::unbounded_channel();
    let (b_dm, _x7) = mpsc::unbounded_channel();
    let (b_ch, _x8) = mpsc::unbounded_channel();
    let (b_f, _x9) = mpsc::unbounded_channel();

    let a1_node = Node::open_with_account(
        a1,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        a1_roster,
        a1_dm,
        a1_ch,
        a1_f,
        &dir.path().join("a1.log"),
        &dir.path().join("a1-sent.log"),
        "pw",
    )
    .unwrap();
    let a2_node = Node::open_with_account(
        a2,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        a2_roster,
        a2_dm,
        a2_ch,
        a2_f,
        &dir.path().join("a2.log"),
        &dir.path().join("a2-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_acct.secret_bytes()),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&a2_node).run_accept_loop(a2_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    a1_node
        .send_to_account(&bob_acct.account_id(), b"to bob", None)
        .await
        .unwrap();

    // A2 (Alice's other device) shows the message in the Alice↔Bob conversation,
    // marked from_me (it was sent by Alice's account).
    let mut a2_hist = Vec::new();
    for _ in 0..50 {
        a2_hist = a2_node.account_history(&bob_acct.account_id(), 10);
        if !a2_hist.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(a2_hist.len(), 1, "A2 self-synced the message");
    assert!(a2_hist[0].from_me, "shown as ours on the other device");
    assert_eq!(a2_hist[0].text, b"to bob");

    // Bob receives it once, not from_me.
    let mut b_hist = Vec::new();
    for _ in 0..50 {
        b_hist = bob_node.account_history(&alice_acct.account_id(), 10);
        if !b_hist.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(b_hist.len(), 1);
    assert!(!b_hist[0].from_me);
    assert_eq!(b_hist[0].text, b"to bob");

    // Note-to-self: A1 sends to its OWN account. The fan-out target set (account == alice)
    // and own-device set fully overlap on the sibling A2, so `account_fanout_targets`' dedup
    // must deliver exactly ONE copy — a regression dropping the HashSet dedup would deliver
    // (and surface) it twice on A2.
    a1_node
        .send_to_account(&alice_acct.account_id(), b"note to self", None)
        .await
        .unwrap();
    let mut a2_self = Vec::new();
    for _ in 0..50 {
        a2_self = a2_node.account_history(&alice_acct.account_id(), 10);
        if !a2_self.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        a2_self.len(),
        1,
        "note-to-self must dedupe to exactly one copy on the sibling device"
    );
    assert_eq!(a2_self[0].text, b"note to self");
}

#[tokio::test]
async fn start_linking_is_idempotent_until_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (tx, _r) = mpsc::unbounded_channel();
    let (ch, _r2) = mpsc::unbounded_channel();
    let (f, _r3) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        tx,
        ch,
        f,
        &dir.path().join("m.log"),
        &dir.path().join("m-sent.log"),
        "pw",
    )
    .unwrap();

    // A double-fire returns the SAME unexpired code (so a UI double-tap doesn't invalidate
    // the code the user is already reading aloud)...
    let code1 = node.start_linking();
    let code2 = node.start_linking();
    assert_eq!(code1, code2);

    // ...but after stop_linking a fresh code is minted.
    node.stop_linking();
    let code3 = node.start_linking();
    assert_ne!(code1, code3);
}

#[tokio::test]
async fn linking_transfers_the_account_secret_with_a_valid_code() {
    use crate::identity::account::Account;
    let linker_id = DeviceIdentity::generate();
    let linker_account = Account::generate();
    let joiner_id = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let linker_addr = listener.local_addr().unwrap();

    let (d, _dr) = mpsc::unbounded_channel();
    let (c, _cr) = mpsc::unbounded_channel();
    let (f, _fr) = mpsc::unbounded_channel();
    let linker_public = linker_id.public();
    let linker = Node::open_with_account(
        linker_id,
        Account::from_secret_bytes(linker_account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        d,
        c,
        f,
        &dir.path().join("l.log"),
        &dir.path().join("l-sent.log"),
        "pw",
    )
    .unwrap();

    let (d2, _d2r) = mpsc::unbounded_channel();
    let (c2, _c2r) = mpsc::unbounded_channel();
    let (f2, _f2r) = mpsc::unbounded_channel();
    let joiner = Node::open_with_account(
        joiner_id,
        Account::generate(), // its own throwaway account before linking
        Arc::new(Mutex::new(Roster::default())),
        d2,
        c2,
        f2,
        &dir.path().join("j.log"),
        &dir.path().join("j-sent.log"),
        "pw",
    )
    .unwrap();

    let code = linker.start_linking();
    tokio::spawn(Arc::clone(&linker).run_accept_loop(listener));

    let linked = joiner
        .link_to_device(linker_addr, &linker_public, &code)
        .await
        .expect("linking succeeds");
    assert_eq!(linked.secret, linker_account.secret_bytes());
    assert_eq!(linked.account_id, linker_account.account_id());
    // Code is single-use: a second attempt is refused.
    let again = joiner
        .link_to_device(linker_addr, &linker_public, &code)
        .await;
    assert!(again.is_err(), "code consumed after first successful link");
}

#[tokio::test]
async fn linking_with_a_wrong_code_is_refused() {
    use crate::identity::account::Account;
    use crate::node::pairing::PairingCode;
    let linker_id = DeviceIdentity::generate();
    let joiner_id = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let linker_addr = listener.local_addr().unwrap();
    let linker_public = linker_id.public();

    let (d, _dr) = mpsc::unbounded_channel();
    let (c, _cr) = mpsc::unbounded_channel();
    let (f, _fr) = mpsc::unbounded_channel();
    let linker = Node::open_with_account(
        linker_id,
        Account::generate(),
        Arc::new(Mutex::new(Roster::default())),
        d,
        c,
        f,
        &dir.path().join("l.log"),
        &dir.path().join("l-sent.log"),
        "pw",
    )
    .unwrap();
    let (d2, _d2r) = mpsc::unbounded_channel();
    let (c2, _c2r) = mpsc::unbounded_channel();
    let (f2, _f2r) = mpsc::unbounded_channel();
    let joiner = Node::open_with_account(
        joiner_id,
        Account::generate(),
        Arc::new(Mutex::new(Roster::default())),
        d2,
        c2,
        f2,
        &dir.path().join("j.log"),
        &dir.path().join("j-sent.log"),
        "pw",
    )
    .unwrap();

    let _real = linker.start_linking();
    tokio::spawn(Arc::clone(&linker).run_accept_loop(listener));
    let wrong = PairingCode::generate().as_hex();
    let res = joiner
        .link_to_device(linker_addr, &linker_public, &wrong)
        .await;
    assert!(
        res.is_err(),
        "a wrong code must not yield the account secret"
    );
}

#[tokio::test]
async fn send_file_to_account_delivers_to_a_peer_account() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &alice_roster,
        &bob,
        &bob_acct,
        "Bob",
        bob_addr.port(),
        &alice_uid,
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(&bob_roster, &alice, &alice_acct, "Alice", 4000, &bob_uid);

    let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
    let (a_f, _a_f_r) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
    let (b_f, mut b_f_r) = mpsc::unbounded_channel();

    let alice_node = Node::open_with_account(
        alice,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_acct.secret_bytes()),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let file = dir.path().join("hello.txt");
    std::fs::write(&file, b"account file payload").unwrap();
    alice_node
        .send_file_to_account(&bob_acct.account_id(), &file, crate::file::FileKind::File)
        .await
        .unwrap();

    let got = tokio::time::timeout(std::time::Duration::from_secs(5), b_f_r.recv())
        .await
        .expect("bob received the file within 5s")
        .expect("file stream open");
    assert_eq!(got.name, "hello.txt");
}

#[tokio::test]
async fn account_reaction_aggregates_on_the_peer_account() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    // Both listen so the reaction (alice→bob) and the message (bob→alice) flow.
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_addr = alice_listener.local_addr().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &alice_roster,
        &bob,
        &bob_acct,
        "Bob",
        bob_addr.port(),
        &alice_uid,
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &bob_roster,
        &alice,
        &alice_acct,
        "Alice",
        alice_addr.port(),
        &bob_uid,
    );

    let (a_dm, _a) = mpsc::unbounded_channel();
    let (a_ch, _b) = mpsc::unbounded_channel();
    let (a_f, _c) = mpsc::unbounded_channel();
    let (b_dm, _d) = mpsc::unbounded_channel();
    let (b_ch, _e) = mpsc::unbounded_channel();
    let (b_f, _f) = mpsc::unbounded_channel();
    let alice_node = Node::open_with_account(
        alice,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_acct.secret_bytes()),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    // Bob sends Alice an account message; Alice picks up its logical id from history.
    bob_node
        .send_to_account(&alice_acct.account_id(), b"hi alice", None)
        .await
        .unwrap();
    let mut target = None;
    for _ in 0..50 {
        let h = alice_node.account_history(&bob_acct.account_id(), 10);
        if let Some(m) = h.first() {
            target = Some(m.id);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let target = target.expect("alice has the message id");

    // Alice reacts to it by its logical id; the reaction reaches Bob's account.
    alice_node
        .react_to_account(&bob_acct.account_id(), target, "👍", false)
        .await
        .unwrap();
    let mut views = Vec::new();
    for _ in 0..50 {
        views = bob_node.account_reactions(&alice_acct.account_id());
        if !views.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(views.len(), 1, "bob sees the reaction");
    assert_eq!(views[0].emoji, "👍");
    assert!(views[0].who.contains(&alice_acct.account_id()));

    // Alice's own view includes her reaction too (sealed-to-peer, merged locally).
    let a_views = alice_node.account_reactions(&bob_acct.account_id());
    assert_eq!(a_views.len(), 1);
    assert!(a_views[0].who.contains(&alice_acct.account_id()));
}

#[tokio::test]
async fn account_history_backfills_to_a_linked_device() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let alice_uid = alice.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    // Alice (account A) has one past account message to Bob's account.
    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(&alice_roster, &bob, &bob_acct, "Bob", 4000, &alice_uid);
    let (a_dm, _a) = mpsc::unbounded_channel();
    let (a_ch, _b) = mpsc::unbounded_channel();
    let (a_f, _c) = mpsc::unbounded_channel();
    let alice_node = Node::open_with_account(
        alice,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    alice_node
        .send_to_account(&bob_acct.account_id(), b"old message", None)
        .await
        .unwrap();

    let records = alice_node.export_account_backfill();
    assert!(!records.is_empty(), "there is history to back-fill");

    // A new device links into account A: import the backfill, then it shows history.
    let alice2 = DeviceIdentity::generate();
    let (d, _d) = mpsc::unbounded_channel();
    let (e, _e) = mpsc::unbounded_channel();
    let (f, _f) = mpsc::unbounded_channel();
    let alice2_node = Node::open_with_account(
        alice2,
        Account::from_secret_bytes(alice_acct.secret_bytes()), // adopted account A
        Arc::new(Mutex::new(Roster::default())),
        d,
        e,
        f,
        &dir.path().join("a2.log"),
        &dir.path().join("a2-sent.log"),
        "pw",
    )
    .unwrap();
    alice2_node.import_account_backfill(&records);

    let hist = alice2_node.account_history(&bob_acct.account_id(), 10);
    assert_eq!(
        hist.len(),
        1,
        "the linked device sees the back-filled message"
    );
    assert!(hist[0].from_me, "it was sent by our (shared) account");
    assert_eq!(hist[0].text, b"old message");
}

// Avatar propagation: Alice sets a custom avatar; it must ride the reliable sync to Bob,
// who ends up holding Alice's avatar bytes keyed by Alice's ACCOUNT id. Mirrors the
// account-delivery rig.
#[tokio::test]
async fn setting_an_avatar_propagates_to_a_peer_account_over_loopback() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();

    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();

    let alice_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &alice_roster,
        &bob,
        &bob_acct,
        "Bob",
        bob_addr.port(),
        &alice_uid,
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(&bob_roster, &alice, &alice_acct, "Alice", 4000, &bob_uid);

    let (a_dm, _a) = mpsc::unbounded_channel();
    let (a_ch, _b) = mpsc::unbounded_channel();
    let (a_f, _c) = mpsc::unbounded_channel();
    let (b_dm, _d) = mpsc::unbounded_channel();
    let (b_ch, _e) = mpsc::unbounded_channel();
    let (b_f, _g) = mpsc::unbounded_channel();

    let alice_node = Node::open_with_account(
        alice,
        Account::from_secret_bytes(alice_acct.secret_bytes()),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a.log"),
        &dir.path().join("a-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_acct.secret_bytes()),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b.log"),
        &dir.path().join("b-sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    let avatar = b"data:image/jpeg;base64,QUJD".to_vec();
    alice_node.set_avatar(Some(avatar.clone())).await.unwrap();

    // Bob ends up with Alice's avatar keyed by Alice's ACCOUNT id.
    let mut got = None;
    for _ in 0..50 {
        got = bob_node.peer_avatar(&alice_acct.account_id());
        if got.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(got, Some(avatar), "bob received alice's avatar");

    // A STALE update (older updated_at) must NOT overwrite the newer one Bob holds.
    let newer = b"data:image/jpeg;base64,WFla".to_vec();
    alice_node.set_avatar(Some(newer.clone())).await.unwrap();
    let mut converged = false;
    for _ in 0..50 {
        if bob_node.peer_avatar(&alice_acct.account_id()).as_deref() == Some(newer.as_slice()) {
            converged = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(converged, "bob sees the newer avatar");

    // Feed Bob a hand-built OLDER profile directly: newest-wins must keep the newer one.
    let stale = crate::node::profile::ProfilePayload::new(
        &Account::from_secret_bytes(alice_acct.secret_bytes()),
        Some(b"data:image/jpeg;base64,T0xE".to_vec()),
        1, // updated_at far in the past
    );
    let applied = bob_node
        .profiles
        .lock()
        .unwrap()
        .record(
            &alice_acct.account_id(),
            stale.updated_at,
            stale.avatar.clone(),
        )
        .unwrap();
    assert!(!applied, "a stale update must be rejected");
    assert_eq!(
        bob_node.peer_avatar(&alice_acct.account_id()),
        Some(newer),
        "the newer avatar is preserved"
    );
}

// An oversized avatar is rejected at the publish boundary (never sealed/appended).
#[tokio::test]
async fn setting_an_oversized_avatar_is_rejected() {
    let me = DeviceIdentity::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (tx, _rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        me,
        roster,
        tx,
        ch_tx,
        file_tx,
        &dir.path().join("me.log"),
        &dir.path().join("me-sent.log"),
        "pw",
    )
    .unwrap();
    let too_big = vec![0u8; crate::node::profile::MAX_AVATAR_BYTES + 1];
    let err = node.set_avatar(Some(too_big)).await.unwrap_err();
    assert!(
        matches!(err, NodeError::Channel(_)),
        "oversize must be rejected"
    );
}
